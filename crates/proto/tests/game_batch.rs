//! Level-4 (`DD04`) batch expansion and `recv` cancel safety.
//!
//! The entries are built the way `aaemu-game/src/systems/skills/packets.rs` writes them
//! (`CompressedGamePackets::raw_payload`: `[u16 0][u16 opcode][body]` per packet).

use std::time::Duration;

use aa_proto::game::{ConnStats, GameConn, MAX_BATCH_BYTES};
use aa_proto::opcodes::{pp, sc};
use aa_proto::stream::PacketStream;
use tokio::io::{AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};

type Conn = GameConn<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

fn pipe() -> (Conn, DuplexStream) {
    let (client, server) = tokio::io::duplex(1 << 20);
    (GameConn::from_io(client), server)
}

fn framed(payload: &[u8]) -> Vec<u8> {
    let mut f = (payload.len() as u16).to_le_bytes().to_vec();
    f.extend_from_slice(payload);
    f
}

fn sc_frame(opcode: u16, body: &[u8]) -> Vec<u8> {
    let mut payload = vec![0xDD, 1, 0, 0];
    payload.extend_from_slice(&opcode.to_le_bytes());
    payload.extend_from_slice(body);
    framed(&payload)
}

/// Raw deflate with stored blocks, as the server writes it (`deflate_stored`).
fn stored_deflate(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunks = data.chunks(0xFFFF).peekable();
    if chunks.peek().is_none() {
        return vec![0x01, 0x00, 0x00, 0xFF, 0xFF];
    }
    while let Some(chunk) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

/// `[len] DD 04 <count> <deflate>`.
fn batch_frame(count: u16, deflated: &[u8]) -> Vec<u8> {
    let mut payload = vec![0xDD, 4];
    payload.extend_from_slice(&count.to_le_bytes());
    payload.extend_from_slice(deflated);
    framed(&payload)
}

fn entry(opcode: u16, body: &[u8]) -> Vec<u8> {
    let mut e = vec![0, 0];
    e.extend_from_slice(&opcode.to_le_bytes());
    e.extend_from_slice(body);
    e
}

// ---- bodies as the server writes them -------------------------------------------------------

/// `SCSkillCooldownResetPacket`.
fn cooldown_reset() -> Vec<u8> {
    let mut s = PacketStream::new();
    s.write_bc(0x0102)
        .write_u32(11_111)
        .write_u32(0)
        .write_bool(false);
    s.into_bytes()
}

/// `SCUnitDamagedPacket` for a skill cast by a unit.
fn unit_damaged() -> Vec<u8> {
    let mut s = PacketStream::new();
    s.write_u8(0).write_u32(1234).write_u16(7); // CastAction::Skill
    s.write_u8(0).write_bc(0x0102); // SkillCaster::Unit
    s.write_bc(0x0102).write_bc(0x0304).write_u8(0);
    s.write_pisc(&[300, 20, 0]);
    s.write_pisc(&[0, 0, 0]);
    s.write_u8(0).write_u16(288 | 2).write_u8(1).write_u8(1);
    s.into_bytes()
}

/// `SCUnitHealedPacket` for a buff tick (cast action Buff, caster Unit).
fn unit_healed() -> Vec<u8> {
    let mut s = PacketStream::new();
    s.write_u8(2)
        .write_u32(55)
        .write_bc(0x0304)
        .write_u32(9)
        .write_bool(true)
        .write_bool(false);
    s.write_u8(0).write_bc(0x0102);
    s.write_bc(0x0304)
        .write_u8(0)
        .write_u8(1)
        .write_i32(450)
        .write_i32(0)
        .write_u8(1);
    s.into_bytes()
}

/// `SCPlotEventPacket` with two target units; flag 8 appends 13 zero `i32`.
fn plot_event(flag: u8) -> Vec<u8> {
    let mut s = PacketStream::new();
    s.write_u16(12).write_u32(100).write_u32(200);
    s.write_u8(1).write_bc(1).write_u8(1).write_bc(2);
    s.write_u64(0)
        .write_bc(0)
        .write_u16(0)
        .write_bc(0)
        .write_i16(0);
    s.write_u8(2).write_bc(2).write_bc(2).write_u8(flag);
    if flag & 8 != 0 {
        s.write_raw(&[0; 52]);
    }
    s.into_bytes()
}

fn skill_ended() -> Vec<u8> {
    vec![0x34, 0x12]
}

/// The batch of one skill use: a plot event, the damage, a heal and a cooldown reset.
fn skill_batch() -> (Vec<u8>, Vec<(u16, Vec<u8>)>) {
    let entries = vec![
        (sc::SCPlotEventPacket, plot_event(2)),
        (sc::SCUnitDamagedPacket, unit_damaged()),
        (sc::SCUnitHealedPacket, unit_healed()),
        (sc::SCPlotEventPacket, plot_event(8)),
        (sc::SCSkillCooldownResetPacket, cooldown_reset()),
    ];
    let raw = entries.iter().flat_map(|(op, b)| entry(*op, b)).collect();
    (raw, entries)
}

async fn expect_batch(deflated: impl Fn(&[u8]) -> Vec<u8>) {
    let (mut conn, mut server) = pipe();
    let (raw, entries) = skill_batch();
    let mut wire = sc_frame(sc::SCUnitDeathPacket, &[7]);
    wire.extend(batch_frame(entries.len() as u16, &deflated(&raw)));
    wire.extend(sc_frame(sc::SCSkillEndedPacket, &skill_ended()));
    server.write_all(&wire).await.unwrap();

    assert_eq!(conn.recv().await.unwrap().opcode, sc::SCUnitDeathPacket);
    for (opcode, body) in &entries {
        let p = conn.recv().await.unwrap();
        assert_eq!((p.level, p.opcode), (1, *opcode), "{}", p.name());
        assert_eq!(&p.body[..], &body[..], "{}", p.name());
    }
    let p = conn.recv().await.unwrap();
    assert!(p.is_sc(sc::SCSkillEndedPacket));
    assert_eq!(&p.body[..], &skill_ended()[..]);

    assert_eq!(
        conn.stats(),
        ConnStats {
            packets_in: 7,
            bytes_in: wire.len() as u64,
            batches: 1,
            ..ConnStats::default()
        }
    );
}

#[tokio::test]
async fn a_compressed_server_batch_expands_in_order_between_plain_packets() {
    expect_batch(|raw| miniz_oxide::deflate::compress_to_vec(raw, 6)).await;
}

#[tokio::test]
async fn a_stored_block_batch_as_the_server_writes_it_expands_too() {
    expect_batch(stored_deflate).await;
}

#[tokio::test]
async fn an_unsizable_entry_ends_the_batch_and_the_rest_is_counted_as_dropped() {
    let (mut conn, mut server) = pipe();
    let mut raw = entry(sc::SCSkillEndedPacket, &[1, 0]);
    raw.extend(entry(sc::SCUnitHealedPacket, &unit_healed()));
    raw.extend(entry(sc::SCUnitDeathPacket, &[1, 2, 3, 4])); // not a batched opcode
    raw.extend(entry(sc::SCSkillEndedPacket, &[2, 0]));
    raw.extend(entry(sc::SCSkillEndedPacket, &[3, 0]));
    let mut wire = batch_frame(5, &stored_deflate(&raw));
    wire.extend(sc_frame(sc::SCSkillEndedPacket, &[9, 0]));
    server.write_all(&wire).await.unwrap();

    assert!(conn.recv().await.unwrap().is_sc(sc::SCSkillEndedPacket));
    assert!(conn.recv().await.unwrap().is_sc(sc::SCUnitHealedPacket));
    // The next thing is the plain packet behind the batch: the three entries are gone.
    let p = conn.recv().await.unwrap();
    assert_eq!(&p.body[..], &[9, 0]);
    let stats = conn.stats();
    assert_eq!((stats.batches, stats.batch_entries_dropped), (1, 3));
    assert_eq!(stats.packets_in, 3);
}

#[tokio::test]
async fn a_count_larger_than_the_content_drops_the_missing_entries() {
    let (mut conn, mut server) = pipe();
    let raw = entry(sc::SCSkillEndedPacket, &[1, 0]);
    server
        .write_all(&batch_frame(3, &stored_deflate(&raw)))
        .await
        .unwrap();
    server
        .write_all(&sc_frame(sc::SCSkillEndedPacket, &[2, 0]))
        .await
        .unwrap();
    assert_eq!(&conn.recv().await.unwrap().body[..], &[1, 0]);
    assert_eq!(&conn.recv().await.unwrap().body[..], &[2, 0]);
    assert_eq!(conn.stats().batch_entries_dropped, 2);
}

#[tokio::test]
async fn an_entry_with_a_nonzero_header_is_not_delivered() {
    let (mut conn, mut server) = pipe();
    let mut raw = entry(sc::SCSkillEndedPacket, &[1, 0]);
    raw.extend([1, 0, 0x10, 0x00, 0, 0]); // first u16 must be 0
    server
        .write_all(&batch_frame(2, &stored_deflate(&raw)))
        .await
        .unwrap();
    server
        .write_all(&sc_frame(sc::SCUnitDeathPacket, &[]))
        .await
        .unwrap();
    assert!(conn.recv().await.unwrap().is_sc(sc::SCSkillEndedPacket));
    assert!(conn.recv().await.unwrap().is_sc(sc::SCUnitDeathPacket));
    assert_eq!(conn.stats().batch_entries_dropped, 1);
}

#[tokio::test]
async fn bombs_garbage_and_oversized_counts_drop_the_batch_but_not_the_connection() {
    let (mut conn, mut server) = pipe();
    // 4 MiB of zeros deflate to a few KiB: far over the inflate cap.
    let bomb = miniz_oxide::deflate::compress_to_vec(&vec![0u8; 4 * MAX_BATCH_BYTES], 6);
    assert!(bomb.len() < 60_000);
    server.write_all(&batch_frame(5, &bomb)).await.unwrap();
    // Not a deflate stream.
    server
        .write_all(&batch_frame(2, &[0xFF; 40]))
        .await
        .unwrap();
    // A valid batch whose count exceeds the entry cap.
    let ok = stored_deflate(&entry(sc::SCSkillEndedPacket, &[1, 0]));
    server.write_all(&batch_frame(5000, &ok)).await.unwrap();
    // The connection still works.
    server
        .write_all(&sc_frame(sc::SCSkillEndedPacket, &[7, 0]))
        .await
        .unwrap();

    assert_eq!(&conn.recv().await.unwrap().body[..], &[7, 0]);
    let stats = conn.stats();
    assert_eq!(stats.batches, 3);
    assert_eq!(stats.batch_entries_dropped, 5 + 2 + 5000);
    assert_eq!(stats.packets_in, 1);
}

#[tokio::test]
async fn an_empty_batch_yields_nothing() {
    let (mut conn, mut server) = pipe();
    server
        .write_all(&batch_frame(0, &stored_deflate(&[])))
        .await
        .unwrap();
    server
        .write_all(&sc_frame(sc::SCSkillEndedPacket, &[1, 0]))
        .await
        .unwrap();
    assert_eq!(&conn.recv().await.unwrap().body[..], &[1, 0]);
    assert_eq!(conn.stats().batches, 1);
    assert_eq!(conn.stats().batch_entries_dropped, 0);
}

/// `recv` is the one place a bot task waits on the network, raced against its tick timer and
/// command channel. A `select!` that loses to the timer drops the `recv` future at its await
/// point; no byte of a half-read frame and no entry of a half-delivered batch may be lost.
///
/// The stream is fed three bytes at a time, ten (virtual) milliseconds apart, while the reader
/// cancels `recv` every millisecond, so every frame is cancelled mid-flight many times.
#[tokio::test(start_paused = true)]
async fn recv_is_cancel_safe_across_partial_frames_and_batches() {
    let (mut conn, server) = pipe();
    let (batch_raw, batch_entries) = skill_batch();

    let mut expected: Vec<(u8, u16, Vec<u8>)> = Vec::new();
    let mut wire = Vec::new();
    wire.extend(sc_frame(sc::SCUnitDeathPacket, &[1, 2, 3]));
    expected.push((1, sc::SCUnitDeathPacket, vec![1, 2, 3]));
    for round in 0..2u8 {
        wire.extend(batch_frame(
            batch_entries.len() as u16,
            &miniz_oxide::deflate::compress_to_vec(&batch_raw, 6),
        ));
        for (op, body) in &batch_entries {
            expected.push((1, *op, body.clone()));
        }
        // A proxy packet and a plain packet between the batches.
        let mut pong = vec![0xDD, 2];
        pong.extend_from_slice(&pp::PongPacket.to_le_bytes());
        pong.extend([round, 0, 0]);
        wire.extend(framed(&pong));
        expected.push((2, pp::PongPacket, vec![round, 0, 0]));
        wire.extend(sc_frame(sc::SCSkillEndedPacket, &skill_ended()));
        expected.push((1, sc::SCSkillEndedPacket, skill_ended()));
    }

    let writer = tokio::spawn(async move {
        let mut server = server;
        for chunk in wire.chunks(3) {
            server.write_all(chunk).await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        server // keep the pipe open until the reader is done
    });

    let mut got = Vec::new();
    let mut cancelled = 0u32;
    while got.len() < expected.len() {
        tokio::select! {
            r = conn.recv() => {
                let p = r.unwrap();
                got.push((p.level, p.opcode, p.body.to_vec()));
            }
            _ = tokio::time::sleep(Duration::from_millis(1)) => cancelled += 1,
        }
    }
    drop(writer.await.unwrap());

    assert_eq!(got, expected);
    // Every wire frame was cancelled many times while incomplete.
    assert!(cancelled > 200, "only {cancelled} cancellations");
    let stats = conn.stats();
    assert_eq!(stats.packets_in, expected.len() as u64);
    assert_eq!(stats.batches, 2);
    assert_eq!(stats.batch_entries_dropped, 0);
}

/// The other direction of the same property: entries still queued from a batch are returned
/// first, and a `recv` that is cancelled before it is ever polled leaves them in place.
#[tokio::test]
async fn queued_batch_entries_survive_a_cancelled_recv() {
    let (mut conn, mut server) = pipe();
    let raw: Vec<u8> = (1..=4u8)
        .flat_map(|i| entry(sc::SCSkillEndedPacket, &[i, 0]))
        .collect();
    server
        .write_all(&batch_frame(4, &stored_deflate(&raw)))
        .await
        .unwrap();

    assert_eq!(&conn.recv().await.unwrap().body[..], &[1, 0]);
    // A recv future that loses a race before it is ever polled leaves the queue untouched.
    tokio::select! {
        biased;
        _ = std::future::ready(()) => {}
        _ = conn.recv() => unreachable!(),
    }
    assert_eq!(&conn.recv().await.unwrap().body[..], &[2, 0]);
    // A recv that has data queued completes in its first poll, so the sleep never wins.
    tokio::select! {
        biased;
        r = conn.recv() => assert_eq!(&r.unwrap().body[..], &[3, 0]),
        _ = tokio::time::sleep(Duration::from_secs(60)) => unreachable!(),
    }
    assert_eq!(&conn.recv().await.unwrap().body[..], &[4, 0]);
    assert_eq!(conn.stats().packets_in, 4);
}
