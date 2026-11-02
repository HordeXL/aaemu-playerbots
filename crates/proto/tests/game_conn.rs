//! `GameConn` framing over in-memory pipes and a real local TCP socket.

use std::time::Duration;

use aa_proto::cs::{self, Out};
use aa_proto::game::{ConnStats, GameConn, GameError, Packet, LEVEL_GAME, LEVEL_PROXY, MAX_FRAME};
use aa_proto::opcodes::{cs as op, pp, sc};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::net::TcpListener;

/// Reads one `[u16 len][payload]` frame from the server end of a pipe.
async fn read_frame(io: &mut (impl AsyncReadExt + Unpin)) -> Vec<u8> {
    let mut len = [0u8; 2];
    io.read_exact(&mut len).await.unwrap();
    let mut payload = vec![0u8; usize::from(u16::from_le_bytes(len))];
    io.read_exact(&mut payload).await.unwrap();
    payload
}

/// A server-to-client frame `[len] DD 01 00 00 <op> <body>`.
fn sc_frame(opcode: u16, body: &[u8]) -> Vec<u8> {
    let mut payload = vec![0xDD, 1, 0, 0];
    payload.extend_from_slice(&opcode.to_le_bytes());
    payload.extend_from_slice(body);
    framed(&payload)
}

/// A server-to-client proxy frame `[len] DD 02 <op> <body>`.
fn pp_frame(opcode: u16, body: &[u8]) -> Vec<u8> {
    let mut payload = vec![0xDD, 2];
    payload.extend_from_slice(&opcode.to_le_bytes());
    payload.extend_from_slice(body);
    framed(&payload)
}

fn framed(payload: &[u8]) -> Vec<u8> {
    let mut f = (payload.len() as u16).to_le_bytes().to_vec();
    f.extend_from_slice(payload);
    f
}

fn pipe() -> (
    GameConn<tokio::io::ReadHalf<DuplexStream>, tokio::io::WriteHalf<DuplexStream>>,
    DuplexStream,
) {
    let (client, server) = tokio::io::duplex(256 * 1024);
    (GameConn::from_io(client), server)
}

#[tokio::test]
async fn a_game_packet_is_framed_with_a_wrapping_count_byte() {
    let (mut conn, mut server) = pipe();
    conn.send(&cs::notify_in_game()).await.unwrap();
    conn.send(&cs::learn_skill(0x0102_0304)).await.unwrap();
    // DD 01 hash=0 count=0, opcode LE, body.
    assert_eq!(
        read_frame(&mut server).await,
        vec![0xDD, 1, 0, 0, op::CSNotifyInGamePacket as u8, 0]
    );
    assert_eq!(
        read_frame(&mut server).await,
        vec![0xDD, 1, 0, 1, op::CSLearnSkillPacket as u8, 0, 4, 3, 2, 1]
    );
    // The count wraps at 256, never skips, and ignores proxy packets.
    for _ in 2..256 {
        conn.send(&cs::notify_in_game()).await.unwrap();
    }
    conn.send(&cs::ping(1)).await.unwrap();
    conn.send(&cs::notify_in_game()).await.unwrap();
    conn.send(&cs::notify_in_game()).await.unwrap();
    let mut counts = Vec::new();
    for _ in 2..256 {
        counts.push(read_frame(&mut server).await[3]);
    }
    assert_eq!(counts.first(), Some(&2));
    assert_eq!(counts.last(), Some(&255));
    assert_eq!(read_frame(&mut server).await[..2], [0xDD, 2]); // the ping
    assert_eq!(read_frame(&mut server).await[3], 0);
    assert_eq!(read_frame(&mut server).await[3], 1);
}

#[tokio::test]
async fn a_proxy_packet_has_no_hash_or_count() {
    let (mut conn, mut server) = pipe();
    conn.send(&cs::finish_state(0)).await.unwrap();
    assert_eq!(
        read_frame(&mut server).await,
        vec![0xDD, 2, pp::FinishStatePacket as u8, 0, 0, 0, 0, 0]
    );
    conn.send(&cs::ping(0x0102)).await.unwrap();
    let f = read_frame(&mut server).await;
    assert_eq!(f[..4], [0xDD, 2, pp::PingPacket as u8, 0]);
    assert_eq!(f.len(), 4 + 8 + 8 + 4);
    // A proxy send does not consume a count value.
    conn.send(&cs::notify_in_game()).await.unwrap();
    assert_eq!(read_frame(&mut server).await[3], 0);
}

#[tokio::test]
async fn an_oversized_packet_is_refused_and_the_largest_fits() {
    let (mut conn, mut server) = pipe();
    let big = Out {
        level: LEVEL_GAME,
        opcode: 1,
        body: vec![0; MAX_FRAME - 5],
    };
    assert!(matches!(
        conn.send(&big).await,
        Err(GameError::TooLarge { len }) if len == MAX_FRAME + 1
    ));
    let max = Out {
        level: LEVEL_GAME,
        opcode: 1,
        body: vec![7; MAX_FRAME - 6],
    };
    conn.send(&max).await.unwrap();
    let f = read_frame(&mut server).await;
    assert_eq!(f.len(), MAX_FRAME);
    assert_eq!(f[6], 7);
}

#[tokio::test]
async fn recv_returns_game_and_proxy_packets_with_zero_copy_bodies() {
    let (mut conn, mut server) = pipe();
    let mut wire = sc_frame(sc::SCUnitDeathPacket, &[1, 2, 3]);
    wire.extend(pp_frame(pp::PongPacket, &[9, 9]));
    wire.extend(sc_frame(sc::SCTargetChangedPacket, &[]));
    server.write_all(&wire).await.unwrap();

    let p = conn.recv().await.unwrap();
    assert_eq!((p.level, p.opcode), (LEVEL_GAME, sc::SCUnitDeathPacket));
    assert_eq!(&p.body[..], &[1, 2, 3]);
    assert_eq!(p.name(), "SCUnitDeathPacket");
    let p = conn.recv().await.unwrap();
    assert_eq!((p.level, p.opcode), (LEVEL_PROXY, pp::PongPacket));
    assert_eq!(&p.body[..], &[9, 9]);
    assert_eq!(p.name(), "PongPacket");
    let p = conn.recv().await.unwrap();
    assert!(p.is_sc(sc::SCTargetChangedPacket));
    assert!(p.body.is_empty());

    assert_eq!(
        conn.stats(),
        ConnStats {
            packets_in: 3,
            bytes_in: wire.len() as u64,
            ..ConnStats::default()
        }
    );
}

#[tokio::test]
async fn stats_count_what_was_sent() {
    let (mut conn, mut server) = pipe();
    conn.send(&cs::list_characters()).await.unwrap(); // 6 + 6 payload bytes
    conn.send(&cs::ping(0)).await.unwrap(); // 4 + 20 payload bytes
    let _ = read_frame(&mut server).await;
    let _ = read_frame(&mut server).await;
    let s = conn.stats();
    assert_eq!(s.packets_out, 2);
    assert_eq!(s.bytes_out, (2 + 12) + (2 + 24));
    assert_eq!((s.packets_in, s.bytes_in, s.batches), (0, 0, 0));
}

#[tokio::test]
async fn eof_is_a_closed_error() {
    let (mut conn, server) = pipe();
    drop(server);
    assert!(matches!(conn.recv().await, Err(GameError::Closed)));
}

#[tokio::test]
async fn a_frame_without_the_magic_byte_is_rejected() {
    let (mut conn, mut server) = pipe();
    server
        .write_all(&framed(&[0xAA, 1, 0, 0, 5, 0]))
        .await
        .unwrap();
    assert!(matches!(conn.recv().await, Err(GameError::BadFrame(_))));

    let (mut conn, mut server) = pipe();
    server
        .write_all(&framed(&[0xDD, 1, 0, 0, 5]))
        .await
        .unwrap(); // level 1 needs 6 bytes
    assert!(matches!(conn.recv().await, Err(GameError::BadFrame(_))));

    let (mut conn, mut server) = pipe();
    server.write_all(&framed(&[0xDD])).await.unwrap();
    assert!(matches!(conn.recv().await, Err(GameError::BadFrame(_))));
}

#[tokio::test]
async fn an_unknown_level_is_skipped_not_fatal() {
    let (mut conn, mut server) = pipe();
    let mut wire = framed(&[0xDD, 3, 7, 0, 1, 2]);
    wire.extend(sc_frame(sc::SCUnitDeathPacket, &[]));
    server.write_all(&wire).await.unwrap();
    assert_eq!(conn.recv().await.unwrap().opcode, sc::SCUnitDeathPacket);
}

#[tokio::test]
async fn close_shuts_the_write_half_down() {
    let (mut conn, mut server) = pipe();
    conn.send(&cs::notify_in_game()).await.unwrap();
    conn.close().await.unwrap();
    let _ = read_frame(&mut server).await;
    let mut rest = Vec::new();
    server.read_to_end(&mut rest).await.unwrap();
    assert!(rest.is_empty());
}

/// A local TCP server that echoes one frame back as a `SCUnitDeathPacket` and then hangs up.
#[tokio::test]
async fn tcp_round_trip_runs_on_a_spawned_task() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let frame = read_frame(&mut sock).await;
        assert_eq!(
            frame,
            vec![0xDD, 1, 0, 0, op::CSChangeTargetPacket as u8, 0, 1, 2, 3]
        );
        sock.write_all(&sc_frame(sc::SCUnitDeathPacket, &frame[6..]))
            .await
            .unwrap();
        // Dropping the socket closes the connection.
    });

    // `GameConn` is `Send`: the whole exchange runs inside `tokio::spawn`.
    let client = tokio::spawn(async move {
        let mut conn = GameConn::connect(addr).await.unwrap();
        conn.send(&cs::change_target(0x03_0201)).await.unwrap();
        let p: Packet = tokio::time::timeout(Duration::from_secs(5), conn.recv())
            .await
            .unwrap()
            .unwrap();
        let closed = tokio::time::timeout(Duration::from_secs(5), conn.recv())
            .await
            .unwrap();
        (p, closed.is_err(), conn.stats())
    });
    let (p, closed, stats) = client.await.unwrap();
    server.await.unwrap();
    assert_eq!(p.opcode, sc::SCUnitDeathPacket);
    assert_eq!(&p.body[..], &[1, 2, 3]);
    assert!(closed);
    assert_eq!((stats.packets_out, stats.packets_in), (1, 1));
}

#[tokio::test]
async fn connect_accepts_a_host_port_string() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let accept = tokio::spawn(async move { listener.accept().await.unwrap().0 });
    let conn = GameConn::connect(addr.as_str()).await.unwrap();
    drop(conn);
    drop(accept.await.unwrap());
}
