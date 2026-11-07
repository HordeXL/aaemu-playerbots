//! The chat duty and the LLM director through the real brain and the real `Ctx`, against the real
//! `main_world` data and a local mock endpoint (no external host is contacted):
//!
//! ```text
//! AA_COMPACT=<.../app/Data/compact.sqlite3> AA_SERVER_DATA=<.../app/Data> [AA_BAKED=<data dir>] \
//!   cargo test -p playerbots --test social_real_data -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use aa_proto::cs::Out;
use aa_proto::opcodes::cs as op;
use aa_proto::sc::ServerEvent;
use aa_proto::sc_world::WorldEvent;
use aa_world::{World, WorldPaths};
use playerbots::archetype::Archetypes;
use playerbots::bot::brain::Brain;
use playerbots::bot::combat::Combat;
use playerbots::bot::coordinator::CoordHandle;
use playerbots::bot::ctx::{Ctx, Persona};
use playerbots::bot::motion::{Motion, MotionCfg, MotionEnv};
use playerbots::bot::state::BotState;
use playerbots::config::{BehaviorCfg, Config};
use playerbots::hub::{Hub, HubParams, Logger};
use playerbots::roster::Temperament;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn world() -> Option<Arc<World>> {
    static WORLD: OnceLock<Option<Arc<World>>> = OnceLock::new();
    WORLD
        .get_or_init(|| {
            let paths = WorldPaths {
                compact: PathBuf::from(std::env::var_os("AA_COMPACT")?),
                server_data: PathBuf::from(std::env::var_os("AA_SERVER_DATA")?),
                baked: std::env::var_os("AA_BAKED").map(PathBuf::from),
                world: "main_world".into(),
            };
            Some(Arc::new(World::load(&paths).expect("load main_world")))
        })
        .clone()
}

const ME: u32 = 0x400;

struct Rig {
    world: Arc<World>,
    cfg: Config,
    hub: Hub,
    log: Logger,
    coord: CoordHandle,
    persona: Persona,
    state: BotState,
    motion: Motion,
    combat: Combat,
    brain: Brain,
    out: Vec<Out>,
    rng: fastrand::Rng,
    now_ms: u64,
}

impl Rig {
    fn new(world: Arc<World>, cfg: Config, llm: Option<Arc<pb_llm::Llm>>) -> Rig {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let hub = Hub::new(HubParams {
            world: &world,
            baked_dir: None,
            supervisor: tx,
            llm,
        });
        let archetypes = Archetypes::builtin().unwrap();
        let archetype = archetypes.get("Darkrunner").unwrap().clone();
        let slot = hub.register(pb_web::BotSnapshot {
            id: 1,
            name: "Tester".into(),
            ..pb_web::BotSnapshot::default()
        });
        let persona = Persona {
            id: 1,
            name: "Tester".into(),
            archetype,
            temper: Temperament::NEUTRAL,
            profile: playerbots::roster::Profile::NEUTRAL,
        };
        let start = world.race_start(1).expect("Nuian start");
        let z = world.ground_z(start.x, start.y).unwrap_or(start.z);
        let mut state = BotState::new();
        state.me.char_id = 500;
        state.me.obj = ME;
        state.me.spawned = true;
        state.me.level = 5;
        state.me.hp = 1000;
        state.me.hp_max = 1000;
        state.me.mp = 400;
        state.me.mp_max = 400;
        state.me.faction = world.race_faction(1).expect("Nuian faction");
        state.me.pos = [start.x, start.y, z];
        state.skills = vec![(persona.archetype.starter_skill, 1)];
        let mut motion = Motion::new(MotionCfg::default());
        motion.enter_world(ME, state.me.pos, 0.0, 1);
        let brain = Brain::new(&cfg.behavior, &persona.temper);
        Rig {
            log: Logger::new(slot),
            coord: CoordHandle::detached(),
            cfg,
            hub,
            persona,
            state,
            motion,
            combat: Combat::new(),
            brain,
            out: Vec::new(),
            rng: fastrand::Rng::with_seed(7),
            now_ms: 1,
            world,
        }
    }

    fn tick(&mut self, dt: u64) {
        self.now_ms += dt;
        self.state.now_ms = self.now_ms;
        self.state.me.pos = self.motion.pos();
        {
            let mut ctx = Ctx {
                world: &self.world,
                cfg: &self.cfg,
                persona: &self.persona,
                state: &mut self.state,
                motion: &mut self.motion,
                combat: &mut self.combat,
                out: &mut self.out,
                coord: &self.coord,
                hub: &self.hub,
                log: &self.log,
                rng: &mut self.rng,
                now: tokio::time::Instant::now(),
                now_ms: self.now_ms,
                real_player_near: true,
            };
            self.brain.tick(&mut ctx);
        }
        let mut env = MotionEnv {
            ground: &*self.world,
            now_ms: self.now_ms,
            out: &mut self.out,
            rng: &mut self.rng,
            send_gap_ms: 250,
            combat: self.combat.target().is_some(),
        };
        self.motion.tick(&mut env);
        self.state.me.pos = self.motion.pos();
    }

    fn feed(&mut self, ev: WorldEvent) {
        let ev = ServerEvent::World(ev);
        self.state.apply(&ev);
        self.combat
            .on_event(&ev, self.state.me.obj, self.now_ms, &mut self.out);
    }

    fn chat_packets(&self) -> Vec<&Out> {
        self.out
            .iter()
            .filter(|o| o.opcode == op::CSSendChatMessagePacket)
            .collect()
    }
}

fn chat(kind: i16, name: &str, msg: &str) -> WorldEvent {
    WorldEvent::Chat {
        kind,
        sender_obj: 0,
        sender_id: 900,
        name: name.into(),
        msg: msg.into(),
    }
}

#[tokio::test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
async fn system_lines_stay_readable_and_a_whisper_gets_a_whisper_back() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let mut rig = Rig::new(world, Config::default(), None);
    rig.tick(250);

    // The server's answer to a GM command arrives while a goal runs: it must survive the tick.
    rig.feed(chat(-2, "", "House construction complete"));
    rig.feed(chat(-1, "", "Server maintenance in 10 minutes"));
    let sent_at = rig.now_ms;
    rig.tick(250);
    assert!(
        rig.state.inbox.is_empty(),
        "the inbox is emptied by the chat duty"
    );
    let sys = rig.log.with_slot(|g| g.social.system_since(0));
    assert_eq!(sys.len(), 2, "{sys:?}");
    assert_eq!(&*sys[0].text, "House construction complete");
    // a goal that sent a command at `sent_at` finds the answer with a time filter
    assert_eq!(
        rig.log.with_slot(|g| g.social.system_since(sent_at)).len(),
        2
    );
    assert!(rig
        .log
        .with_slot(|g| g.social.system_since(rig.now_ms + 1))
        .is_empty());
    assert!(
        rig.chat_packets().is_empty(),
        "system text is never answered"
    );

    // A whisper from a GM is answered with a whisper to the character behind the GM name.
    rig.feed(chat(-3, "GM Nick", "hello Tester, how is it going?"));
    for _ in 0..60 {
        rig.tick(250);
        if !rig.chat_packets().is_empty() {
            break;
        }
    }
    let packets = rig.chat_packets();
    assert_eq!(packets.len(), 1, "exactly one reply");
    let body = &packets[0].body;
    assert_eq!(i16::from_le_bytes([body[0], body[1]]), -3, "a whisper");
    let text = String::from_utf8_lossy(body);
    assert!(text.contains("Nick") && !text.contains("GM Nick"), "{text}");
    // the panel shows the exchange
    let chat_lines = rig.log.with_slot(|g| g.social.chat_lines());
    assert!(chat_lines.iter().any(|l| l.text.contains("(whisper)")));
    assert!(chat_lines
        .iter()
        .any(|l| l.text.starts_with("(me to Nick)")));
    assert_eq!(
        rig.hub
            .chat
            .gate
            .stats
            .canned_replies
            .load(Ordering::Relaxed),
        1
    );
}

/// A local chat-completions endpoint that always answers `content`.
async fn mock(content: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let h = h.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let head_end = loop {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break p + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                while buf.len() < head_end + len {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                h.fetch_add(1, Ordering::SeqCst);
                let body = serde_json::json!({
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": content}}],
                    "usage": {"prompt_tokens": 20, "completion_tokens": 8, "total_tokens": 28}
                })
                .to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    (base, hits)
}

fn director_config(base: String) -> (Config, Arc<pb_llm::Llm>) {
    let cfg = Config {
        llm: pb_llm::LlmConfig {
            enabled: true,
            base_url: base,
            director: true,
            director_interval_secs: 30,
            timeout_secs: 5,
            max_requests_per_minute: 1000,
            daily_token_budget: 1_000_000,
            ..pb_llm::LlmConfig::default()
        },
        ..Config::default()
    };
    let llm = pb_llm::Llm::with_key(&cfg.llm, "test-key".into());
    (cfg, llm)
}

#[tokio::test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
async fn the_director_starts_the_goal_the_model_picks() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    // only wander and rest are switched on, so the options are [wander, rest] and 1 is "rest"
    let (base, hits) = mock(r#"{"choice": 1, "reason": "feels like a break"}"#).await;
    let (mut cfg, llm) = director_config(base);
    cfg.behavior = BehaviorCfg {
        quest: 0.0,
        grind: 0.0,
        duel: 0.0,
        trade: 0.0,
        trade_run: 0.0,
        housing: 0.0,
        ..BehaviorCfg::default()
    };
    let mut rig = Rig::new(world, cfg, Some(llm));
    let mut picked = false;
    for _ in 0..400 {
        rig.tick(250);
        tokio::time::sleep(Duration::from_millis(5)).await;
        if rig
            .hub
            .chat
            .gate
            .stats
            .director_picks
            .load(Ordering::Relaxed)
            > 0
        {
            picked = true;
            break;
        }
    }
    assert!(
        picked,
        "the director never applied a pick ({} requests)",
        hits.load(Ordering::SeqCst)
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1, "one question");
    rig.tick(250);
    assert_eq!(rig.brain.goal_name(), "rest");
    // and it is in the bot's log for the operator
    let detail = pb_web::BotHub::detail(&rig.hub, 1).unwrap();
    assert!(
        detail
            .log
            .iter()
            .any(|l| l.text.starts_with("director: rest")),
        "{:?}",
        detail.log
    );
}

#[tokio::test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
async fn a_useless_answer_or_a_forced_goal_leaves_the_bot_alone() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    // an index nobody offered
    let (base, hits) = mock(r#"{"choice": 99}"#).await;
    let (cfg, llm) = director_config(base);
    let mut rig = Rig::new(world.clone(), cfg, Some(llm));
    for _ in 0..200 {
        rig.tick(250);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(hits.load(Ordering::SeqCst) >= 1, "it did ask");
    assert_eq!(
        rig.hub
            .chat
            .gate
            .stats
            .director_picks
            .load(Ordering::Relaxed),
        0
    );

    // the operator forced a goal: the director stays out of it
    let (base, hits) = mock(r#"{"choice": 1}"#).await;
    let (cfg, llm) = director_config(base);
    let mut rig = Rig::new(world, cfg, Some(llm));
    rig.brain.command(&playerbots::bot::cmd::BotCmd::Goal(
        playerbots::bot::cmd::GoalChoice::Forced(playerbots::bot::cmd::GoalKind::Wander),
    ));
    for _ in 0..200 {
        rig.tick(250);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "no question while a goal is forced"
    );
}
