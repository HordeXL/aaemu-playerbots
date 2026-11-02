//! The network side of one bot: the login server exchange, the lobby (enter world, character
//! list, create or select, spawn), the keep-alive and the logout.
//!
//! The game connection is a plain [`GameConn`] owned by the bot task; there is no reader task per
//! bot. The lobby is a small state machine over the server's answers ([`enter_game`]), generic
//! over the transport so a fake server on in-memory pipes can drive it in tests.
//!
//! The login connection stays open for the whole session: the login server forgets the game
//! session when it closes.

use std::time::Duration;

use aa_proto::cs::{self, Out};
use aa_proto::game::{GameConn, GameError, Packet};
use aa_proto::login::{LoginClient, LoginError, WorldCookie};
use aa_proto::sc::{self, ServerEvent};
use aa_proto::sc_world::{CharSummary, WorldEvent};
use smallvec::SmallVec;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::Instant;

use super::state::BotState;

/// How long each lobby step may take.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(25);
/// Keep-alive interval (the server drops a connection that is silent for 30 s).
pub const PING_EVERY: Duration = Duration::from_secs(5);
/// How long the login server exchange may take in total.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(45);
/// Quiet time after the character data before the bot spawns (the inventory burst has ended).
const SETTLE: Duration = Duration::from_millis(300);
/// The last `ChangeState` the lobby answers with `FinishState`.
const LAST_STATE: i32 = 7;

/// Why a session step failed.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The login server refused or did not answer.
    #[error("login server: {0}")]
    Login(#[from] LoginError),
    /// The game connection failed.
    #[error("game server: {0}")]
    Game(#[from] GameError),
    /// The login server lists no active world to enter.
    #[error("the login server lists no active world to enter")]
    NoWorld,
    /// `X2EnterWorldResponse` with a non-zero reason.
    #[error("the game server refused the connection (reason {0})")]
    Refused(i16),
    /// A step took too long.
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    /// `SCCharacterCreationFailed`: the name is taken or not allowed.
    #[error("the server rejected the character (reason {0})")]
    NameRejected(u8),
    /// `SCKicked`.
    #[error("kicked by the server (reason {reason}): {msg}")]
    Kicked {
        /// Reason code.
        reason: u8,
        /// Message text.
        msg: String,
    },
}

impl SessionError {
    /// The server rejected the character name: pick another one and retry at once.
    pub fn is_name_rejected(&self) -> bool {
        matches!(self, SessionError::NameRejected(_))
    }

    /// Trying again soon is pointless (wrong password, banned account): wait the maximum.
    pub fn is_permanent(&self) -> bool {
        matches!(
            self,
            SessionError::Login(LoginError::Denied(_) | LoginError::JoinRefused(_))
        )
    }
}

/// Decodes a packet into an event; a packet that does not match its layout is logged at debug
/// level and dropped (a bot must survive whatever the server sends).
pub fn decode(p: &Packet) -> Option<ServerEvent> {
    match sc::parse(p.level, p.opcode, &p.body) {
        Ok(ev) => ev,
        Err(e) => {
            tracing::debug!(packet = p.name(), "undecodable packet: {e}");
            None
        }
    }
}

/// Sends and empties `out`.
pub async fn flush_out<R, W>(conn: &mut GameConn<R, W>, out: &mut Vec<Out>) -> Result<(), GameError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    for o in out.drain(..) {
        conn.send(&o).await?;
    }
    Ok(())
}

/// `CSInstanceLoaded` (level 1, empty body); `aa_proto::cs` has no builder for it.
pub fn instance_loaded() -> Out {
    Out {
        level: aa_proto::game::LEVEL_GAME,
        opcode: aa_proto::opcodes::cs::CSInstanceLoadedPacket,
        body: Vec::new(),
    }
}

/// What the login server gave: the open login connection and the ticket for the game server.
pub struct LoginTicket {
    /// Keep this alive as long as the game connection lives.
    pub login: LoginClient,
    /// The account id.
    pub account_id: u32,
    /// The cookie and where to present it.
    pub cookie: WorldCookie,
    /// Name of the world entered.
    pub world_name: String,
}

/// Logs in on the login server and asks to enter a world (`world_id` 0 = the first active one).
pub async fn login_server(
    addr: &str,
    account: &str,
    password: &str,
    world_id: u8,
) -> Result<LoginTicket, SessionError> {
    match tokio::time::timeout(
        LOGIN_TIMEOUT,
        login_exchange(addr, account, password, world_id),
    )
    .await
    {
        Ok(r) => r,
        Err(_) => Err(SessionError::Timeout("the login server")),
    }
}

async fn login_exchange(
    addr: &str,
    account: &str,
    password: &str,
    world_id: u8,
) -> Result<LoginTicket, SessionError> {
    let mut login = LoginClient::connect(addr).await?;
    let account_id = login.auth(account, password).await?;
    let worlds = login.list_worlds().await?;
    let world = worlds
        .worlds
        .iter()
        .find(|w| w.active && (world_id == 0 || w.id == world_id))
        .ok_or(SessionError::NoWorld)?;
    let (gs_id, world_name) = (world.id, world.name.clone());
    let cookie = login.enter_world(gs_id).await?;
    Ok(LoginTicket {
        login,
        account_id,
        cookie,
        world_name,
    })
}

/// The character a bot wants.
#[derive(Debug, Clone)]
pub struct Wish<'a> {
    /// Name for a new character.
    pub name: &'a str,
    /// Character id from the roster, if the character exists.
    pub char_id: Option<u32>,
    /// Race of a new character.
    pub race: u8,
    /// Gender of a new character.
    pub gender: u8,
    /// Starting ability (class) of a new character.
    pub ability: u8,
}

/// The result of a successful lobby.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entered {
    /// Character id.
    pub char_id: u32,
    /// Character name on the server.
    pub name: String,
    /// The character was created in this session.
    pub created: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    Enter,
    List,
    Create,
    Select,
    Settle,
    Spawn,
}

impl Step {
    fn what(self) -> &'static str {
        match self {
            Step::Enter => "the enter-world answer",
            Step::List => "the character list",
            Step::Create => "the character creation",
            Step::Select => "the character data",
            Step::Settle => "the character data",
            Step::Spawn => "the own unit state",
        }
    }
}

/// Runs the lobby on an open game connection: presents the cookie, answers the connection state
/// machine, lists the account's characters, creates or selects one and spawns it. `st` receives
/// every event on the way; `start` is the bot clock's origin.
pub async fn enter_game<R, W>(
    conn: &mut GameConn<R, W>,
    st: &mut BotState,
    account_id: u32,
    cookie: u32,
    wish: &Wish<'_>,
    start: Instant,
) -> Result<Entered, SessionError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    conn.send(&cs::enter_world(account_id, cookie)).await?;
    let mut step = Step::Enter;
    let mut deadline = Instant::now() + STEP_TIMEOUT;
    let mut next_ping = Instant::now() + PING_EVERY;
    let mut chars: SmallVec<[CharSummary; 4]> = SmallVec::new();
    let mut created = false;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(SessionError::Timeout(step.what()));
        }
        if now >= next_ping {
            conn.send(&cs::ping(start.elapsed().as_millis() as i64))
                .await?;
            next_ping = now + PING_EVERY;
        }
        let mut wait = deadline.min(next_ping).saturating_duration_since(now);
        if step == Step::Settle {
            wait = wait.min(SETTLE);
        }
        let packet = match tokio::time::timeout(wait, conn.recv()).await {
            Ok(r) => r?,
            Err(_) => {
                if step == Step::Settle {
                    conn.send(&cs::spawn_character()).await?;
                    step = Step::Spawn;
                    deadline = Instant::now() + STEP_TIMEOUT;
                }
                continue;
            }
        };
        let Some(ev) = decode(&packet) else { continue };
        st.now_ms = start.elapsed().as_millis() as u64;
        st.apply(&ev);
        let ServerEvent::World(w) = &ev else { continue };
        match (step, w) {
            (_, WorldEvent::Kicked { reason, msg }) => {
                return Err(SessionError::Kicked {
                    reason: *reason,
                    msg: msg.to_string(),
                });
            }
            (Step::Enter, WorldEvent::EnterWorldResponse { reason, .. }) if *reason != 0 => {
                return Err(SessionError::Refused(*reason));
            }
            (Step::Enter, WorldEvent::ChangeState(n)) => {
                if (0..=LAST_STATE).contains(n) {
                    conn.send(&cs::finish_state(*n)).await?;
                }
                if *n >= LAST_STATE {
                    conn.send(&cs::list_characters()).await?;
                    step = Step::List;
                    chars.clear();
                }
                deadline = Instant::now() + STEP_TIMEOUT;
            }
            (Step::List, WorldEvent::CharacterList { last, chars: c }) => {
                chars.extend(c.iter().cloned());
                if *last {
                    let pick = wish
                        .char_id
                        .and_then(|id| chars.iter().find(|c| c.id == id))
                        .or_else(|| {
                            chars
                                .iter()
                                .find(|c| c.name.eq_ignore_ascii_case(wish.name))
                        })
                        .or_else(|| chars.first());
                    match pick {
                        Some(c) => {
                            conn.send(&cs::select_character(c.id)).await?;
                            step = Step::Select;
                        }
                        None => {
                            conn.send(&cs::create_character(&cs::CreateCharacter {
                                name: wish.name,
                                race: wish.race,
                                gender: wish.gender,
                                body_items: [0; 7],
                                ability1: wish.ability,
                            }))
                            .await?;
                            step = Step::Create;
                        }
                    }
                    deadline = Instant::now() + STEP_TIMEOUT;
                }
            }
            (Step::Create, WorldEvent::CharacterCreated { id, .. }) => {
                created = true;
                conn.send(&cs::select_character(*id)).await?;
                step = Step::Select;
                deadline = Instant::now() + STEP_TIMEOUT;
            }
            (Step::Create, WorldEvent::CharacterCreateFailed { reason }) => {
                return Err(SessionError::NameRejected(*reason));
            }
            (Step::Select, WorldEvent::CharacterState(_)) => {
                step = Step::Settle;
                deadline = Instant::now() + STEP_TIMEOUT;
            }
            (Step::Spawn, WorldEvent::UnitState(_)) if st.me.spawned && st.me.obj != 0 => {
                conn.send(&cs::notify_in_game()).await?;
                conn.send(&cs::notify_in_game_completed()).await?;
                return Ok(Entered {
                    char_id: st.me.char_id,
                    name: st.me.name.to_string(),
                    created,
                });
            }
            _ => {}
        }
    }
}

/// Logs the character out: `CSLeaveWorld`, a short wait for the server's grant (or for it to
/// close the connection), then closes the write half.
pub async fn leave<R, W>(conn: &mut GameConn<R, W>, st: &mut BotState, wait: Duration)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if conn.send(&cs::leave_world(cs::LEAVE_QUIT)).await.is_ok() {
        let end = Instant::now() + wait;
        st.reactions.leave_granted = false;
        while !st.reactions.leave_granted {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match tokio::time::timeout(left, conn.recv()).await {
                Ok(Ok(p)) => {
                    if let Some(ev) = decode(&p) {
                        st.apply(&ev);
                    }
                }
                _ => break,
            }
        }
    }
    let _ = conn.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use aa_proto::game::{LEVEL_GAME, LEVEL_PROXY};
    use aa_proto::opcodes::{cs as cso, pp, sc as sco};
    use tokio::io::duplex;

    fn sc_out(opcode: u16, body: Vec<u8>) -> Out {
        Out {
            level: LEVEL_GAME,
            opcode,
            body,
        }
    }

    fn change_state(n: i32) -> Out {
        Out {
            level: LEVEL_PROXY,
            opcode: pp::ChangeStatePacket,
            body: n.to_le_bytes().to_vec(),
        }
    }

    /// A server that accepts the cookie, walks the state machine, lists no characters and
    /// refuses the creation.
    async fn fake_server<T: AsyncRead + AsyncWrite + Unpin>(
        mut srv: GameConn<tokio::io::ReadHalf<T>, tokio::io::WriteHalf<T>>,
        reject_with: u8,
    ) -> (Vec<u16>, String) {
        let mut seen = Vec::new();
        let mut created_name = String::new();
        loop {
            let p = match srv.recv().await {
                Ok(p) => p,
                Err(_) => return (seen, created_name),
            };
            seen.push(p.opcode);
            match (p.level, p.opcode) {
                (LEVEL_GAME, cso::X2EnterWorldPacket) => {
                    // reason 0, gm false, token 7, stream port 1250
                    let mut body = vec![0, 0, 0];
                    body.extend(7u32.to_le_bytes());
                    body.extend(1250u16.to_le_bytes());
                    srv.send(&sc_out(sco::X2EnterWorldResponsePacket, body))
                        .await
                        .unwrap();
                    srv.send(&change_state(0)).await.unwrap();
                }
                (LEVEL_PROXY, pp::FinishStatePacket) => {
                    let n = i32::from_le_bytes(p.body[..4].try_into().unwrap());
                    if n < LAST_STATE {
                        srv.send(&change_state(n + 1)).await.unwrap();
                    }
                }
                (LEVEL_GAME, cso::CSListCharacterPacket) => {
                    srv.send(&sc_out(sco::SCCharacterListPacket, vec![1, 0]))
                        .await
                        .unwrap();
                }
                (LEVEL_GAME, cso::CSCreateCharacterPacket) => {
                    let len = u16::from_le_bytes([p.body[0], p.body[1]]) as usize;
                    created_name = String::from_utf8_lossy(&p.body[2..2 + len]).into_owned();
                    srv.send(&sc_out(
                        sco::SCCharacterCreationFailedPacket,
                        vec![reject_with],
                    ))
                    .await
                    .unwrap();
                }
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn the_lobby_walks_the_state_machine_and_reports_a_rejected_name() {
        let (a, b) = duplex(64 * 1024);
        let mut client = GameConn::from_io(a);
        let server = tokio::spawn(fake_server(GameConn::from_io(b), 5));
        let mut st = BotState::new();
        let wish = Wish {
            name: "Tester",
            char_id: None,
            race: 1,
            gender: 1,
            ability: 1,
        };
        let err = enter_game(&mut client, &mut st, 42, 9, &wish, Instant::now())
            .await
            .expect_err("the fake server refuses the name");
        assert!(err.is_name_rejected(), "{err}");
        assert!(matches!(err, SessionError::NameRejected(5)));
        drop(client);
        let (seen, name) = server.await.unwrap();
        assert_eq!(name, "Tester");
        // enter, ten state answers in order (0..=7), the list request, the creation
        assert_eq!(seen[0], cso::X2EnterWorldPacket);
        let finishes = seen.iter().filter(|o| **o == pp::FinishStatePacket).count();
        assert_eq!(finishes, 8, "FinishState 0..=7: {seen:?}");
        assert!(seen.contains(&cso::CSListCharacterPacket));
        assert_eq!(*seen.last().unwrap(), cso::CSCreateCharacterPacket);
    }

    #[tokio::test]
    async fn a_refused_connection_is_reported() {
        let (a, b) = duplex(4096);
        let mut client = GameConn::from_io(a);
        let mut srv = GameConn::from_io(b);
        let server = tokio::spawn(async move {
            let _ = srv.recv().await;
            let mut body = vec![];
            body.extend(3i16.to_le_bytes());
            body.extend([0, 0, 0, 0, 0, 0, 0]);
            srv.send(&sc_out(sco::X2EnterWorldResponsePacket, body))
                .await
                .unwrap();
            srv
        });
        let mut st = BotState::new();
        let wish = Wish {
            name: "Tester",
            char_id: None,
            race: 1,
            gender: 1,
            ability: 1,
        };
        let err = enter_game(&mut client, &mut st, 1, 1, &wish, Instant::now())
            .await
            .expect_err("refused");
        assert!(matches!(err, SessionError::Refused(3)), "{err}");
        let _keep = server.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_server_times_out_instead_of_hanging() {
        let (a, _b) = duplex(4096);
        let mut client = GameConn::from_io(a);
        let mut st = BotState::new();
        let wish = Wish {
            name: "Tester",
            char_id: None,
            race: 1,
            gender: 1,
            ability: 1,
        };
        let err = enter_game(&mut client, &mut st, 1, 1, &wish, Instant::now())
            .await
            .expect_err("silent server");
        assert!(matches!(err, SessionError::Timeout(_)), "{err}");
    }

    #[test]
    fn permanent_login_errors_are_recognised() {
        assert!(SessionError::Login(LoginError::Denied(2)).is_permanent());
        assert!(!SessionError::Login(LoginError::Timeout).is_permanent());
        assert!(!SessionError::Timeout("x").is_permanent());
    }

    #[test]
    fn instance_loaded_is_an_empty_level_one_packet() {
        let o = instance_loaded();
        assert_eq!((o.level, o.opcode, o.body.len()), (1, 0x0e0, 0));
    }
}
