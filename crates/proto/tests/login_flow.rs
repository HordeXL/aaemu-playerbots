//! `LoginClient` against a mock login server on a local TCP socket.
//!
//! The mock writes the L2C packets as `aaemu-login/src/packets/l2c.rs` does and checks the C2L
//! packets against `c2l.rs` (`CaRequestAuthTrion::read`, `CaListWorld`, `CaEnterWorld`).

use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use aa_proto::login::{ac, ca, trion_ticket, LoginClient, LoginError, WorldCookie};
use aa_proto::stream::PacketStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn bind() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    (listener, addr)
}

/// Reads one frame and returns `(opcode, body)`.
async fn read_packet(sock: &mut TcpStream) -> (u16, Vec<u8>) {
    let mut len = [0u8; 2];
    sock.read_exact(&mut len).await.unwrap();
    let mut payload = vec![0u8; usize::from(u16::from_le_bytes(len))];
    sock.read_exact(&mut payload).await.unwrap();
    (
        u16::from_le_bytes([payload[0], payload[1]]),
        payload[2..].to_vec(),
    )
}

async fn write_packet(sock: &mut TcpStream, opcode: u16, body: &[u8]) {
    let mut f = ((body.len() + 2) as u16).to_le_bytes().to_vec();
    f.extend_from_slice(&opcode.to_le_bytes());
    f.extend_from_slice(body);
    sock.write_all(&f).await.unwrap();
}

/// `ACJoinResponsePacket`: `u16 reason, u64 afs`.
fn join_response(reason: u16) -> Vec<u8> {
    let mut s = PacketStream::new();
    s.write_u16(reason).write_u64(6);
    s.into_bytes()
}

/// `ACAuthResponsePacket`: `u32 account id, sized wsk[32], u8 slot count`.
fn auth_response(account_id: u32) -> Vec<u8> {
    let mut s = PacketStream::new();
    s.write_u32(account_id)
        .write_bytes_sized(&[0; 32])
        .write_u8(6);
    s.into_bytes()
}

/// Reads the auth request and checks it against what `CaRequestAuthTrion::read` expects.
async fn read_auth(sock: &mut TcpStream, user: &str, password: &str) {
    let (op, body) = read_packet(sock).await;
    assert_eq!(op, ca::REQUEST_AUTH_TRION);
    let mut s = PacketStream::from_bytes(body);
    assert_eq!(s.read_u32().unwrap(), 0); // pFrom
    assert_eq!(s.read_u32().unwrap(), 0); // pTo
    assert!(!s.read_bool().unwrap()); // dev
    assert_eq!(
        s.read_bytes_sized().unwrap(),
        vec![0x00, 0x16, 0x3e, 0x00, 0x00, 0x01]
    );
    assert_eq!(s.read_string().unwrap(), trion_ticket(user, password));
    assert_eq!(s.read_string().unwrap(), ""); // signature
    assert!(s.read_bool().unwrap()); // isLast
    assert_eq!(s.remaining(), 0);
}

#[tokio::test]
async fn full_login_sequence() {
    let (listener, addr) = bind().await;
    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        read_auth(&mut sock, "bot1", "hunter2").await;
        write_packet(&mut sock, ac::JOIN_RESPONSE, &join_response(0)).await;
        write_packet(&mut sock, ac::AUTH_RESPONSE, &auth_response(4711)).await;

        let (op, body) = read_packet(&mut sock).await;
        assert_eq!((op, body), (ca::LIST_WORLD, vec![0; 8]));
        let mut w = PacketStream::new();
        w.write_u8(2);
        w.write_u8(1)
            .write_str("AAEmu")
            .write_bool(true)
            .write_u8(1)
            .write_raw(&[0; 9]);
        w.write_u8(2).write_str("Down").write_bool(false);
        w.write_u8(1);
        w.write_u32(4711)
            .write_u8(1)
            .write_u32(77)
            .write_str("Botty");
        w.write_u8(1)
            .write_u8(2)
            .write_bytes_sized(&[0; 16])
            .write_i64(0);
        write_packet(&mut sock, ac::WORLD_LIST, w.as_bytes()).await;

        let (op, body) = read_packet(&mut sock).await;
        assert_eq!(op, ca::ENTER_WORLD);
        assert_eq!(body, [0, 0, 0, 0, 0, 0, 0, 0, 1]); // u64 flag, u8 gs id
        let mut c = PacketStream::new();
        c.write_u32(0x0BAD_F00D);
        for _ in 0..4 {
            c.write_raw(&[1, 0, 0, 127]).write_u16(1239);
        }
        write_packet(&mut sock, ac::WORLD_COOKIE, c.as_bytes()).await;
    });

    let mut login = LoginClient::connect(&addr).await.unwrap();
    assert_eq!(login.account_id, 0);
    assert_eq!(login.auth("bot1", "hunter2").await.unwrap(), 4711);
    assert_eq!(login.account_id, 4711);

    let list = login.list_worlds().await.unwrap();
    assert_eq!(list.worlds.len(), 2);
    assert_eq!(
        (
            list.worlds[0].id,
            list.worlds[0].name.as_str(),
            list.worlds[0].active
        ),
        (1, "AAEmu", true)
    );
    assert!(!list.worlds[1].active);
    assert_eq!(list.characters.len(), 1);
    assert_eq!(
        (
            list.characters[0].id,
            list.characters[0].name.as_str(),
            list.characters[0].gs_id
        ),
        (77, "Botty", 1)
    );

    let cookie = login.enter_world(1).await.unwrap();
    assert_eq!(
        cookie,
        WorldCookie {
            cookie: 0x0BAD_F00D,
            game_addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1239),
        }
    );
    server.await.unwrap();
}

#[tokio::test]
async fn denied_login_carries_the_reason() {
    let (listener, addr) = bind().await;
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = read_packet(&mut sock).await;
        // ACLoginDeniedPacket: u8 reason, sized vp, str msg.
        write_packet(&mut sock, ac::LOGIN_DENIED, &[5, 0, 0, 0, 0]).await;
    });
    let mut login = LoginClient::connect(&addr).await.unwrap();
    assert!(matches!(
        login.auth("bot", "wrong").await,
        Err(LoginError::Denied(5))
    ));
}

#[tokio::test]
async fn a_refused_join_is_reported() {
    let (listener, addr) = bind().await;
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = read_packet(&mut sock).await;
        write_packet(&mut sock, ac::JOIN_RESPONSE, &join_response(3)).await;
    });
    let mut login = LoginClient::connect(&addr).await.unwrap();
    assert!(matches!(
        login.auth("bot", "x").await,
        Err(LoginError::JoinRefused(3))
    ));
}

#[tokio::test]
async fn enter_world_denied_and_unexpected_packets() {
    let (listener, addr) = bind().await;
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = read_packet(&mut sock).await;
        write_packet(&mut sock, ac::ENTER_WORLD_DENIED, &[2]).await;
        let _ = read_packet(&mut sock).await;
        write_packet(&mut sock, ac::WORLD_QUEUE, &[0; 8]).await;
        let _ = read_packet(&mut sock).await;
        write_packet(&mut sock, ac::AUTH_RESPONSE, &auth_response(1)).await;
    });
    let mut login = LoginClient::connect(&addr).await.unwrap();
    assert!(matches!(
        login.enter_world(1).await,
        Err(LoginError::EnterWorldDenied(2))
    ));
    match login.enter_world(1).await {
        Err(LoginError::Unexpected { opcode, got, .. }) => {
            assert_eq!((opcode, got), (ac::WORLD_QUEUE, "ACWorldQueuePacket"));
        }
        other => panic!("expected Unexpected, got {other:?}"),
    }
    // A list-worlds request answered with the wrong packet.
    assert!(matches!(
        login.list_worlds().await,
        Err(LoginError::Unexpected { opcode, .. }) if opcode == ac::AUTH_RESPONSE
    ));
}

#[tokio::test]
async fn silence_times_out_and_a_hangup_is_closed() {
    let (listener, addr) = bind().await;
    let hold = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        sock // keep it open and silent
    });
    let mut login = LoginClient::connect(&addr).await.unwrap();
    login.timeout = Duration::from_millis(50);
    assert!(matches!(
        login.list_worlds().await,
        Err(LoginError::Timeout)
    ));
    drop(hold.await.unwrap());
    login.timeout = Duration::from_secs(5);
    assert!(matches!(
        login.list_worlds().await,
        Err(LoginError::Closed | LoginError::Io(_))
    ));
}

#[tokio::test]
async fn connecting_to_a_closed_port_is_a_connect_error() {
    let (listener, addr) = bind().await;
    drop(listener);
    match LoginClient::connect(&addr).await {
        Err(LoginError::Connect { addr: a, .. }) => assert_eq!(a, addr),
        Err(e) => panic!("unexpected error {e}"),
        Ok(_) => panic!("connected to a closed port"),
    }
}
