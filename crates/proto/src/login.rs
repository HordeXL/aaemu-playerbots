//! Login server client (C2L / L2C), as spoken by the 1.2 (r208022) Trion client.
//!
//! Ported from the AAEmu-1.2-Rust test client (`aaemu-testclient/src/login.rs`, GPL-3.0-or-later),
//! without its recorder. Frames are `[u16 len][u16 opcode][body]`. The 1.2 client started by a
//! launcher authenticates with `CARequestAuthTrionPacket`: an XML ticket carrying the user name
//! and the hex SHA-256 of the password. A login server with `AutoAccount` creates unknown
//! accounts on first login.
//!
//! A bot uses the login server exactly once per session: [`LoginClient::auth`], then
//! [`LoginClient::list_worlds`] and [`LoginClient::enter_world`]; the returned [`WorldCookie`]
//! is what the game server wants in `X2EnterWorldPacket`.

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

use crate::codec::{FrameCodec, Parts};
use crate::stream::{MarshalError, PacketStream};

/// C2L opcodes (`aaemu-login/src/packets/c2l.rs`).
pub mod ca {
    /// `CARequestAuthTrionPacket`.
    pub const REQUEST_AUTH_TRION: u16 = 0x004;
    /// `CAListWorldPacket`.
    pub const LIST_WORLD: u16 = 0x00b;
    /// `CAEnterWorldPacket`.
    pub const ENTER_WORLD: u16 = 0x00c;
    /// `CACancelEnterWorldPacket`.
    pub const CANCEL_ENTER_WORLD: u16 = 0x00d;
}

/// L2C opcodes (`aaemu-login/src/packets/l2c.rs`).
pub mod ac {
    /// `ACJoinResponsePacket`.
    pub const JOIN_RESPONSE: u16 = 0x000;
    /// `ACChallengePacket`.
    pub const CHALLENGE: u16 = 0x002;
    /// `ACAuthResponsePacket`.
    pub const AUTH_RESPONSE: u16 = 0x003;
    /// `ACChallenge2Packet`.
    pub const CHALLENGE2: u16 = 0x004;
    /// `ACEnterOtpPacket`.
    pub const ENTER_OTP: u16 = 0x005;
    /// `ACShowArsPacket`.
    pub const SHOW_ARS: u16 = 0x006;
    /// `ACEnterPcCertPacket`.
    pub const ENTER_PC_CERT: u16 = 0x007;
    /// `ACWorldListPacket`.
    pub const WORLD_LIST: u16 = 0x008;
    /// `ACWorldQueuePacket`.
    pub const WORLD_QUEUE: u16 = 0x009;
    /// `ACWorldCookiePacket`.
    pub const WORLD_COOKIE: u16 = 0x00a;
    /// `ACEnterWorldDeniedPacket`.
    pub const ENTER_WORLD_DENIED: u16 = 0x00b;
    /// `ACLoginDeniedPacket`.
    pub const LOGIN_DENIED: u16 = 0x00c;
    /// `ACAccountWarnedPacket`.
    pub const ACCOUNT_WARNED: u16 = 0x00d;
}

/// C# name of an L2C opcode, for logging.
pub fn ac_name(op: u16) -> &'static str {
    match op {
        ac::JOIN_RESPONSE => "ACJoinResponsePacket",
        ac::CHALLENGE => "ACChallengePacket",
        ac::AUTH_RESPONSE => "ACAuthResponsePacket",
        ac::CHALLENGE2 => "ACChallenge2Packet",
        ac::ENTER_OTP => "ACEnterOtpPacket",
        ac::SHOW_ARS => "ACShowArsPacket",
        ac::ENTER_PC_CERT => "ACEnterPcCertPacket",
        ac::WORLD_LIST => "ACWorldListPacket",
        ac::WORLD_QUEUE => "ACWorldQueuePacket",
        ac::WORLD_COOKIE => "ACWorldCookiePacket",
        ac::ENTER_WORLD_DENIED => "ACEnterWorldDeniedPacket",
        ac::LOGIN_DENIED => "ACLoginDeniedPacket",
        ac::ACCOUNT_WARNED => "ACAccountWarnedPacket",
        _ => "ACUnknown",
    }
}

/// C# name of a C2L opcode, for logging.
pub fn ca_name(op: u16) -> &'static str {
    match op {
        ca::REQUEST_AUTH_TRION => "CARequestAuthTrionPacket",
        ca::LIST_WORLD => "CAListWorldPacket",
        ca::ENTER_WORLD => "CAEnterWorldPacket",
        ca::CANCEL_ENTER_WORLD => "CACancelEnterWorldPacket",
        _ => "CAUnknown",
    }
}

/// Why a login-server exchange failed.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    /// The TCP connection could not be opened.
    #[error("connecting to login server {addr}: {source}")]
    Connect {
        /// The address that was dialled.
        addr: String,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// The socket failed after it was open.
    #[error("login socket error: {0}")]
    Io(#[from] io::Error),
    /// No packet arrived within [`LoginClient::timeout`].
    #[error("timed out waiting for a login packet")]
    Timeout,
    /// The server closed the connection.
    #[error("login server closed the connection")]
    Closed,
    /// `ACLoginDeniedPacket`: bad credentials, banned account, ... (the `LoginDeniedReason`).
    #[error("login denied (reason {0})")]
    Denied(u8),
    /// `ACJoinResponsePacket` with a non-zero reason.
    #[error("join refused (reason {0})")]
    JoinRefused(u16),
    /// `ACEnterWorldDeniedPacket`: the game server is down or refused the account.
    #[error("enter world denied (reason {0})")]
    EnterWorldDenied(u8),
    /// A packet other than the one the exchange expects.
    #[error("expected {expected} but got {got} (0x{opcode:03x})")]
    Unexpected {
        /// The packet that was expected.
        expected: &'static str,
        /// The name of the packet that arrived instead.
        got: &'static str,
        /// The opcode that arrived instead.
        opcode: u16,
    },
    /// A frame too short to hold an opcode.
    #[error("login frame too short")]
    ShortFrame,
    /// A packet body that does not match its layout.
    #[error("malformed login packet: {0}")]
    Malformed(#[from] MarshalError),
}

/// Result of a login-server exchange.
pub type Result<T> = std::result::Result<T, LoginError>;

/// A world of `ACWorldListPacket`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct World {
    /// Game server id, what [`LoginClient::enter_world`] takes.
    pub id: u8,
    /// Display name.
    pub name: String,
    /// Whether the game server is registered and accepting players.
    pub active: bool,
}

/// A character of `ACWorldListPacket`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginCharacter {
    /// Owning account.
    pub account_id: u32,
    /// Game server the character lives on.
    pub gs_id: u8,
    /// Character id.
    pub id: u32,
    /// Character name.
    pub name: String,
}

/// `ACWorldListPacket`.
#[derive(Clone, Debug, Default)]
pub struct WorldList {
    /// Game servers known to the login server.
    pub worlds: Vec<World>,
    /// The account's characters on all of them.
    pub characters: Vec<LoginCharacter>,
}

/// `ACWorldCookiePacket`: the one-time ticket for the game server and where to present it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldCookie {
    /// The login connection id; `X2EnterWorldPacket.cookie`.
    pub cookie: u32,
    /// The game server endpoint.
    pub game_addr: SocketAddrV4,
}

/// The launcher's Trion ticket for `user` / `password`.
///
/// The user name is XML-escaped (the login server parses the ticket as XML); the password is
/// sent as the lowercase hex SHA-256 of its UTF-8 bytes, so it needs no escaping.
pub fn trion_ticket(user: &str, password: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hash = String::with_capacity(64);
    for b in Sha256::digest(password.as_bytes()) {
        hash.push(char::from(HEX[usize::from(b >> 4)]));
        hash.push(char::from(HEX[usize::from(b & 15)]));
    }
    let mut ticket = String::with_capacity(200 + user.len() + hash.len());
    ticket.push_str(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <authTicket version=\"1.2\"><storeToken>1</storeToken><username>",
    );
    for c in user.chars() {
        match c {
            '&' => ticket.push_str("&amp;"),
            '<' => ticket.push_str("&lt;"),
            '>' => ticket.push_str("&gt;"),
            '"' => ticket.push_str("&quot;"),
            '\'' => ticket.push_str("&apos;"),
            c => ticket.push(c),
        }
    }
    ticket.push_str("</username><password>");
    ticket.push_str(&hash);
    ticket.push_str("</password></authTicket>");
    ticket
}

/// One login-server connection.
pub struct LoginClient {
    framed: Framed<TcpStream, FrameCodec>,
    /// How long to wait for each server packet (default 15 s).
    pub timeout: Duration,
    /// The account id from `ACAuthResponsePacket`; 0 before [`LoginClient::auth`].
    pub account_id: u32,
}

impl LoginClient {
    /// Opens a TCP connection to the login server at `addr` (`host:port`).
    pub async fn connect(addr: &str) -> Result<Self> {
        let sock = TcpStream::connect(addr)
            .await
            .map_err(|source| LoginError::Connect {
                addr: addr.to_owned(),
                source,
            })?;
        sock.set_nodelay(true)?;
        Ok(Self {
            framed: Framed::with_capacity(sock, FrameCodec, 1024),
            timeout: Duration::from_secs(15),
            account_id: 0,
        })
    }

    async fn send(&mut self, opcode: u16, body: &[u8]) -> Result<()> {
        let head = opcode.to_le_bytes();
        self.framed.send(Parts { head: &head, body }).await?;
        Ok(())
    }

    /// Next L2C packet as `(opcode, body)`.
    async fn recv(&mut self) -> Result<(u16, Bytes)> {
        let frame = tokio::time::timeout(self.timeout, self.framed.next())
            .await
            .map_err(|_| LoginError::Timeout)?
            .ok_or(LoginError::Closed)??;
        if frame.len() < 2 {
            return Err(LoginError::ShortFrame);
        }
        Ok((u16::from_le_bytes([frame[0], frame[1]]), frame.slice(2..)))
    }

    /// Next packet, which must be `want`; `ACLoginDeniedPacket` becomes [`LoginError::Denied`].
    async fn expect(&mut self, want: u16) -> Result<PacketStream> {
        let (op, body) = self.recv().await?;
        if op == ac::LOGIN_DENIED {
            let reason = body.first().copied().unwrap_or(0);
            return Err(LoginError::Denied(reason));
        }
        if op != want {
            return Err(unexpected(want, op));
        }
        Ok(PacketStream::from_bytes(body.to_vec()))
    }

    /// `CARequestAuthTrionPacket` then `ACJoinResponsePacket` + `ACAuthResponsePacket`.
    /// Returns the account id.
    pub async fn auth(&mut self, user: &str, password: &str) -> Result<u32> {
        let mut s = PacketStream::new();
        s.write_u32(0) // pFrom
            .write_u32(0) // pTo
            .write_bool(false) // dev
            .write_bytes_sized(&[0x00, 0x16, 0x3e, 0x00, 0x00, 0x01]) // mac
            .write_str(&trion_ticket(user, password))
            .write_str("") // signature
            .write_bool(true); // isLast
        self.send(ca::REQUEST_AUTH_TRION, s.as_bytes()).await?;
        let reason = self.expect(ac::JOIN_RESPONSE).await?.read_u16()?;
        if reason != 0 {
            return Err(LoginError::JoinRefused(reason));
        }
        self.account_id = self.expect(ac::AUTH_RESPONSE).await?.read_u32()?;
        Ok(self.account_id)
    }

    /// `CAListWorldPacket` then `ACWorldListPacket`.
    pub async fn list_worlds(&mut self) -> Result<WorldList> {
        self.send(ca::LIST_WORLD, &0u64.to_le_bytes()).await?;
        let mut r = self.expect(ac::WORLD_LIST).await?;
        let mut out = WorldList::default();
        let n = r.read_u8()?;
        for _ in 0..n {
            let id = r.read_u8()?;
            let name = r.read_string()?;
            let active = r.read_bool()?;
            if active {
                r.read_bytes(10)?; // con + 9 race congestion values
            }
            out.worlds.push(World { id, name, active });
        }
        let n = r.read_u8()?;
        for _ in 0..n {
            let account_id = r.read_u32()?;
            let gs_id = r.read_u8()?;
            let id = r.read_u32()?;
            let name = r.read_string()?;
            r.read_bytes(2)?; // race, gender
            r.read_bytes_sized()?; // guid
            r.read_u64()?; // v
            out.characters.push(LoginCharacter {
                account_id,
                gs_id,
                id,
                name,
            });
        }
        Ok(out)
    }

    /// `CAEnterWorldPacket` then `ACWorldCookiePacket`.
    ///
    /// The server answers after the game server has accepted the account, which can take a
    /// moment; [`LoginClient::timeout`] applies. IPv6 game server hosts are not supported.
    pub async fn enter_world(&mut self, gs_id: u8) -> Result<WorldCookie> {
        let mut s = PacketStream::new();
        s.write_u64(0).write_u8(gs_id);
        self.send(ca::ENTER_WORLD, s.as_bytes()).await?;
        let (op, body) = self.recv().await?;
        let mut r = PacketStream::from_bytes(body.to_vec());
        match op {
            ac::ENTER_WORLD_DENIED => Err(LoginError::EnterWorldDenied(r.read_u8().unwrap_or(0))),
            ac::LOGIN_DENIED => Err(LoginError::Denied(r.read_u8().unwrap_or(0))),
            ac::WORLD_COOKIE => parse_world_cookie(&mut r),
            _ => Err(unexpected(ac::WORLD_COOKIE, op)),
        }
    }
}

fn unexpected(want: u16, got: u16) -> LoginError {
    LoginError::Unexpected {
        expected: ac_name(want),
        got: ac_name(got),
        opcode: got,
    }
}

/// `ACWorldCookiePacket` body: `u32 cookie`, the address as `IPv4 octets in reversed order`
/// and `u16 port`. The server repeats the address/port pair four times; the first one is used.
/// (`aaemu-login/src/packets/l2c.rs` `AcWorldCookie::write_body`.)
fn parse_world_cookie(r: &mut PacketStream) -> Result<WorldCookie> {
    let cookie = r.read_u32()?;
    let ip = [r.read_u8()?, r.read_u8()?, r.read_u8()?, r.read_u8()?];
    let port = r.read_u16()?;
    Ok(WorldCookie {
        cookie,
        // The address is written with its octets reversed.
        game_addr: SocketAddrV4::new(Ipv4Addr::new(ip[3], ip[2], ip[1], ip[0]), port),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_contains_hashed_password() {
        let t = trion_ticket("bob", "secret");
        assert!(t.contains("<username>bob</username>"));
        assert!(t.contains(
            "<password>2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b</password>"
        ));
    }

    #[test]
    fn ticket_matches_the_launcher_layout_exactly() {
        assert_eq!(
            trion_ticket("U", ""),
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <authTicket version=\"1.2\"><storeToken>1</storeToken><username>U</username>\
             <password>e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855</password>\
             </authTicket>"
        );
    }

    #[test]
    fn ticket_escapes_the_user_name() {
        let t = trion_ticket("a<b&\"c'>", "x");
        assert!(t.contains("<username>a&lt;b&amp;&quot;c&apos;&gt;</username>"));
    }

    #[test]
    fn world_cookie_reverses_the_octets() {
        let mut s = PacketStream::new();
        s.write_u32(0xDEAD_BEEF)
            .write_raw(&[1, 0, 0, 127])
            .write_u16(1239);
        // The repeated address/port pairs of the real packet are ignored.
        s.write_raw(&[1, 0, 0, 127]).write_u16(1239);
        let c = parse_world_cookie(&mut s).unwrap();
        assert_eq!(c.cookie, 0xDEAD_BEEF);
        assert_eq!(c.game_addr, SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1239));
        assert!(parse_world_cookie(&mut PacketStream::from_bytes(vec![1, 2, 3])).is_err());
    }
}
