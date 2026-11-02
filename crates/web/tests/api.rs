//! End-to-end tests of the router: a real listener on an ephemeral port and raw HTTP/1.1 over TCP.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pb_web::{
    Activity, BotDetail, BotHub, BotSnapshot, Command, HubStats, IssueAction, IssueDetail,
    IssueSample, IssueSummary, LogLine, MapInfo, Marker, MarkerKind, QuestLine, WebConfig,
    WorldRect,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really a png";

#[derive(Default)]
struct TestHub {
    commands: Mutex<Vec<(Option<u32>, Command)>>,
    with_image: bool,
    actions: Mutex<Vec<(String, IssueAction)>>,
}

const ISSUE: &str = "pb:6f3b2e1c";

fn issue_summary(fp: &str, state: &str) -> IssueSummary {
    IssueSummary {
        fingerprint: fp.into(),
        kind: "quest_credit_missing".into(),
        title: "Quest 251 gives no credit".into(),
        suspect: "server".into(),
        severity: "medium".into(),
        count: 4,
        bots: 2,
        first_seen: 1_700_000_000_000,
        last_seen: 1_700_000_900_000,
        state: state.into(),
        reportable: true,
    }
}

fn snap(id: u32, activity: Activity) -> BotSnapshot {
    BotSnapshot {
        id,
        name: format!("Bot{id}"),
        race: "Nuian".into(),
        class: "Archery".into(),
        level: 10,
        hp_pct: 80,
        x: 15000.0 + id as f32,
        y: 15000.0,
        z: 100.0,
        zone: Some("Solzreed".into()),
        activity,
        goal: "quest".into(),
        detail: "Quest 330 A Friendly Reminder: talk to Maude".into(),
        ..BotSnapshot::default()
    }
}

impl BotHub for TestHub {
    fn snapshots(&self) -> Vec<BotSnapshot> {
        vec![snap(1, Activity::Questing), snap(2, Activity::TradeRun)]
    }

    fn detail(&self, id: u32) -> Option<BotDetail> {
        (id == 1 || id == 2).then(|| BotDetail {
            snapshot: snap(id, Activity::Idle),
            log: vec![LogLine {
                t: 1_700_000_000_000,
                text: "hello".into(),
            }],
            quests: vec![QuestLine {
                id: 330,
                name: "A Friendly Reminder".into(),
                status: "active".into(),
            }],
            extra: vec![("tick".into(), "250 ms".into())],
            chat: vec![LogLine {
                t: 1_700_000_001_000,
                text: "Bob: hi there".into(),
            }],
            path: Vec::new(),
        })
    }

    fn stats(&self) -> HubStats {
        HubStats {
            bots_total: 2,
            bots_online: 2,
            bytes_in_per_sec: 2048.0,
            bytes_out_per_sec: 512.0,
            open_issues: 2,
            queued_issues: 1,
            activity_counts: vec![(Activity::Questing, 1), (Activity::TradeRun, 1)],
            ..HubStats::default()
        }
    }

    fn markers(&self) -> Vec<Marker> {
        vec![Marker {
            kind: MarkerKind::DuelFlag,
            x: 1.0,
            y: 2.0,
            label: "flag".into(),
        }]
    }

    fn map_info(&self) -> MapInfo {
        MapInfo {
            image_url: self.with_image.then(|| "/map.png".to_string()),
            width: 512,
            height: 308,
            world: WorldRect {
                min_x: -14731.0,
                min_y: 46.0,
                max_x: 49920.0,
                max_y: 38911.0,
            },
        }
    }

    fn map_image(&self) -> Option<Vec<u8>> {
        self.with_image.then(|| PNG.to_vec())
    }

    fn command(&self, bot: Option<u32>, cmd: Command) -> Result<(), String> {
        if let Some(id) = bot {
            if id != 1 && id != 2 {
                return Err(format!("no bot {id}"));
            }
        }
        self.commands.lock().unwrap().push((bot, cmd));
        Ok(())
    }

    fn issues(&self) -> Vec<IssueSummary> {
        vec![
            issue_summary(ISSUE, "open"),
            issue_summary("pb:00000001", "queued"),
        ]
    }

    fn issue(&self, fp: &str) -> Option<IssueDetail> {
        (fp == ISSUE).then(|| IssueDetail {
            summary: issue_summary(ISSUE, "open"),
            entry: "[2026-11-02 14:03:09 EST] Quest 251 gives no credit (playerbots)\r\nObserved by playerbots: x."
                .into(),
            area: Some("rust/crates/aaemu-game/src/systems/quests/".into()),
            evidence: None,
            queued_at: None,
            samples: vec![IssueSample {
                at: 1_700_000_000_000,
                bot: "Bot1".into(),
                expected: "credit".into(),
                actual: "nothing".into(),
                context: "bot Bot1 (level 10)".into(),
                facts: vec![("quest".into(), "251".into())],
                trail: vec!["-   10 ms  S2C L1 0x0123 SCFooPacket".into()],
                server_log: vec!["14:03:09 [WARN] x - y".into()],
                bot_log: vec!["killed a boar".into()],
            }],
        })
    }

    fn issue_action(&self, fp: &str, action: IssueAction) -> Result<(), String> {
        if fp != ISSUE {
            return Err(format!("no issue {fp}"));
        }
        if action == IssueAction::Reopen {
            return Err("it is not closed".into());
        }
        self.actions.lock().unwrap().push((fp.to_string(), action));
        Ok(())
    }
}

struct Server {
    addr: SocketAddr,
    hub: Arc<TestHub>,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Server {
    async fn start(token: Option<&str>, with_image: bool) -> Server {
        Server::start_on("127.0.0.1", token, with_image).await
    }

    /// Listens on `ip` (port 0); requests are made to 127.0.0.1 on the port that was given.
    async fn start_on(ip: &str, token: Option<&str>, with_image: bool) -> Server {
        let hub = Arc::new(TestHub {
            with_image,
            ..TestHub::default()
        });
        let listener = TcpListener::bind(format!("{ip}:0")).await.unwrap();
        let bound = listener.local_addr().unwrap();
        let addr = SocketAddr::from(([127, 0, 0, 1], bound.port()));
        let (stop, stopped) = oneshot::channel::<()>();
        let cfg = WebConfig {
            bind: bound,
            token: token.map(str::to_string),
        };
        let task = tokio::spawn(pb_web::serve_listener(listener, hub.clone(), cfg, async {
            let _ = stopped.await;
        }));
        Server {
            addr,
            hub,
            stop: Some(stop),
            task,
        }
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> Response {
        let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            req.push_str(&format!("Host: {}\r\n", self.addr));
        }
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some(b) = body {
            req.push_str(&format!("Content-Length: {}\r\n", b.len()));
        }
        req.push_str("\r\n");
        if let Some(b) = body {
            req.push_str(b);
        }
        let mut stream = TcpStream::connect(self.addr).await.unwrap();
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
            .await
            .expect("response timed out")
            .unwrap();
        Response::parse(&raw)
    }

    async fn get(&self, path: &str) -> Response {
        self.request("GET", path, &[], None).await
    }

    async fn post(&self, path: &str, headers: &[(&str, &str)], body: &str) -> Response {
        let mut h = vec![("Content-Type", "application/json")];
        h.extend_from_slice(headers);
        self.request("POST", path, &h, Some(body)).await
    }

    async fn shutdown(mut self) {
        let _ = self.stop.take().unwrap().send(());
        let res = tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .expect("server did not stop")
            .unwrap();
        res.unwrap();
    }
}

struct Response {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Response {
    fn parse(raw: &[u8]) -> Response {
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("no header terminator");
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let mut lines = head.split("\r\n");
        let status = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers: HashMap<String, String> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        let mut body = raw[split + 4..].to_vec();
        if headers
            .get("transfer-encoding")
            .is_some_and(|v| v.contains("chunked"))
        {
            body = dechunk(&body);
        }
        Response {
            status,
            headers,
            body,
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text()))
    }

    fn header(&self, name: &str) -> &str {
        self.headers.get(name).map_or("", String::as_str)
    }
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let eol = data.windows(2).position(|w| w == b"\r\n").unwrap();
        let size =
            usize::from_str_radix(std::str::from_utf8(&data[..eol]).unwrap().trim(), 16).unwrap();
        data = &data[eol + 2..];
        if size == 0 {
            return out;
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

#[tokio::test]
async fn serves_the_embedded_ui() {
    let s = Server::start(None, false).await;
    for (path, content_type, needle) in [
        ("/", "text/html", "<canvas id=\"map\""),
        ("/index.html", "text/html", "<canvas id=\"map\""),
        ("/app.js", "text/javascript", "requestAnimationFrame"),
        ("/style.css", "text/css", "prefers-color-scheme"),
    ] {
        let r = s.get(path).await;
        assert_eq!(r.status, 200, "{path}");
        assert!(r.header("content-type").starts_with(content_type), "{path}");
        assert_eq!(r.header("cache-control"), "no-cache", "{path}");
        assert!(r.text().contains(needle), "{path} lacks {needle}");
    }
    // everything is local: no CDN or remote script/style in the page
    let html = s.get("/").await.text();
    assert!(!html.contains("http://") && !html.contains("https://"));
    assert!(s.get("/definitely-not-here").await.status == 404);
    s.shutdown().await;
}

#[tokio::test]
async fn get_api_routes() {
    let s = Server::start(None, false).await;

    let r = s.get("/api/bots").await;
    assert_eq!(r.status, 200);
    assert!(r.header("content-type").starts_with("application/json"));
    let bots = r.json();
    assert_eq!(bots.as_array().unwrap().len(), 2);
    assert_eq!(bots[0]["activity"], "questing");
    assert_eq!(bots[1]["activity"], "trade_run");
    assert_eq!(bots[0]["zone"], "Solzreed");
    assert!(bots[0]["partner"].is_null());
    assert_eq!(bots[0]["name"], "Bot1");

    let d = s.get("/api/bots/2").await;
    assert_eq!(d.status, 200);
    let d = d.json();
    assert_eq!(d["snapshot"]["id"], 2);
    assert_eq!(d["log"][0]["text"], "hello");
    assert_eq!(d["quests"][0]["id"], 330);
    assert_eq!(d["extra"][0][0], "tick");
    assert_eq!(d["chat"][0]["text"], "Bob: hi there");

    let missing = s.get("/api/bots/77").await;
    assert_eq!(missing.status, 404);
    assert!(missing.json()["error"].as_str().unwrap().contains("77"));
    assert_eq!(s.get("/api/bots/abc").await.status, 400);

    let stats = s.get("/api/stats").await.json();
    assert_eq!(stats["bots_total"], 2);
    assert_eq!(stats["bytes_in_per_sec"], 2048.0);
    assert_eq!(stats["bytes_out_per_sec"], 512.0);
    assert_eq!(stats["open_issues"], 2);
    assert_eq!(stats["queued_issues"], 1);
    assert_eq!(stats["activity_counts"][1][0], "trade_run");
    assert!(stats["llm"].is_null());

    let markers = s.get("/api/markers").await.json();
    assert_eq!(markers[0]["kind"], "duel_flag");
    assert_eq!(markers[0]["label"], "flag");

    let map = s.get("/api/map").await.json();
    assert!(map["image_url"].is_null());
    assert_eq!(map["width"], 512);
    assert_eq!(map["world"]["max_x"], 49920.0);

    s.shutdown().await;
}

#[tokio::test]
async fn map_image_etag_and_versioned_url() {
    let s = Server::start(None, true).await;

    let info = s.get("/api/map").await.json();
    let url = info["image_url"].as_str().unwrap().to_string();
    assert!(url.starts_with("/map.png?v="), "{url}");

    let r = s.get("/map.png").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), "image/png");
    assert_eq!(r.body, PNG);
    assert_eq!(r.header("cache-control"), "no-cache");
    let etag = r.header("etag").to_string();
    assert!(etag.starts_with('"') && etag.ends_with('"'));
    assert!(url.ends_with(etag.trim_matches('"')));

    // the versioned URL is immutable; the bare one revalidates
    let v = s.get(&url).await;
    assert_eq!(v.status, 200);
    assert!(v.header("cache-control").contains("immutable"));

    let cached = s
        .request("GET", "/map.png", &[("If-None-Match", &etag)], None)
        .await;
    assert_eq!(cached.status, 304);
    assert!(cached.body.is_empty());
    let other = s
        .request("GET", "/map.png", &[("If-None-Match", "\"other\"")], None)
        .await;
    assert_eq!(other.status, 200);

    s.shutdown().await;
}

#[tokio::test]
async fn no_map_image_is_a_404() {
    let s = Server::start(None, false).await;
    assert_eq!(s.get("/map.png").await.status, 404);
    s.shutdown().await;
}

#[tokio::test]
async fn commands_reach_the_hub() {
    let s = Server::start(None, false).await;

    let r = s
        .post(
            "/api/bots/1/command",
            &[],
            r#"{"type":"goal","goal":"duel"}"#,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["ok"], true);

    let r = s
        .post(
            "/api/bots/2/command",
            &[],
            r#"{"type":"duel_with","other":1}"#,
        )
        .await;
    assert_eq!(r.status, 200);

    let r = s
        .post(
            "/api/bots/1/command",
            &[],
            r#"{"type":"whisper","to":"Bob","text":"psst"}"#,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = s
        .post(
            "/api/bots/1/command",
            &[],
            r#"{"type":"goal","goal":"wander"}"#,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());

    let r = s
        .post("/api/command", &[], r#"{"type":"spawn","count":3}"#)
        .await;
    assert_eq!(r.status, 200);
    let r = s.post("/api/command", &[], r#"{"type":"pause"}"#).await;
    assert_eq!(r.status, 200);
    // charset parameter and odd casing are fine
    let r = s
        .request(
            "POST",
            "/api/bots/1/command",
            &[("content-type", "Application/JSON; charset=utf-8")],
            Some(r#"{"type":"say","text":"hello there"}"#),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());

    let got = s.hub.commands.lock().unwrap().clone();
    assert_eq!(
        got,
        vec![
            (
                Some(1),
                Command::Goal {
                    goal: "duel".into()
                }
            ),
            (Some(2), Command::DuelWith { other: 1 }),
            (
                Some(1),
                Command::Whisper {
                    to: "Bob".into(),
                    text: "psst".into()
                }
            ),
            (
                Some(1),
                Command::Goal {
                    goal: "wander".into()
                }
            ),
            (None, Command::Spawn { count: 3 }),
            (None, Command::Pause),
            (
                Some(1),
                Command::Say {
                    text: "hello there".into()
                }
            ),
        ]
    );
    s.shutdown().await;
}

#[tokio::test]
async fn bad_commands_are_400_and_never_reach_the_hub() {
    let s = Server::start(None, false).await;
    let cases: [(&str, &str); 11] = [
        ("/api/command", "{not json"),
        ("/api/command", r#"{"type":"explode"}"#),
        ("/api/command", r#"{"type":"spawn"}"#),
        ("/api/command", r#"{"type":"spawn","count":0}"#),
        ("/api/command", r#"{"type":"spawn","count":5000}"#),
        ("/api/bots/1/command", r#"{"type":"goal","goal":"fly"}"#),
        (
            "/api/bots/1/command",
            r#"{"type":"say","text":"/gold add self 5"}"#,
        ),
        ("/api/bots/1/command", r#"{"type":"duel_with","other":1}"#),
        (
            "/api/bots/1/command",
            r#"{"type":"whisper","to":"Bob","text":"/gold add self 5"}"#,
        ),
        (
            "/api/bots/1/command",
            r#"{"type":"whisper","to":"","text":"hi"}"#,
        ),
        ("/api/bots/1/command", r#"{"type":"spawn","count":1}"#),
    ];
    for (path, body) in cases {
        let r = s.post(path, &[], body).await;
        assert_eq!(r.status, 400, "{path} {body}: {}", r.text());
        assert!(r.json()["error"].is_string(), "{path} {body}");
    }
    // pairing commands need a bot
    assert_eq!(
        s.post("/api/command", &[], r#"{"type":"say","text":"hi"}"#)
            .await
            .status,
        400
    );
    // not a number
    assert_eq!(
        s.post("/api/bots/xyz/command", &[], r#"{"type":"pause"}"#)
            .await
            .status,
        400
    );
    // missing content type
    let r = s
        .request("POST", "/api/command", &[], Some(r#"{"type":"pause"}"#))
        .await;
    assert_eq!(r.status, 400);
    // an empty body
    assert_eq!(s.post("/api/command", &[], "").await.status, 400);
    assert!(s.hub.commands.lock().unwrap().is_empty());
    s.shutdown().await;
}

#[tokio::test]
async fn hub_errors_become_400_with_the_message() {
    let s = Server::start(None, false).await;
    let r = s
        .post("/api/bots/99/command", &[], r#"{"type":"pause"}"#)
        .await;
    assert_eq!(r.status, 400);
    assert_eq!(r.json()["error"], "no bot 99");
    s.shutdown().await;
}

#[tokio::test]
async fn oversized_bodies_are_rejected() {
    let s = Server::start(None, false).await;
    let big = format!(r#"{{"type":"say","text":"{}"}}"#, "a".repeat(40_000));
    let r = s.post("/api/bots/1/command", &[], &big).await;
    assert_eq!(r.status, 413);
    assert!(s.hub.commands.lock().unwrap().is_empty());
    s.shutdown().await;
}

#[tokio::test]
async fn token_protects_posts_only() {
    let s = Server::start(Some("sekrit"), true).await;

    // GETs stay open
    for path in [
        "/",
        "/api/bots",
        "/api/bots/1",
        "/api/stats",
        "/api/markers",
        "/api/map",
        "/api/issues",
        "/api/issues/pb:6f3b2e1c",
        "/map.png",
    ] {
        assert_eq!(s.get(path).await.status, 200, "{path}");
    }

    let body = r#"{"type":"pause"}"#;
    for headers in [
        vec![],
        vec![("x-playerbots-token", "nope")],
        vec![("x-playerbots-token", "sekre")],
        vec![("x-playerbots-token", "sekrit2")],
        vec![("x-playerbots-token", "")],
    ] {
        let r = s.post("/api/command", &headers, body).await;
        assert_eq!(r.status, 401, "{headers:?}");
        let r = s.post("/api/bots/1/command", &headers, body).await;
        assert_eq!(r.status, 401, "{headers:?}");
    }
    assert!(s.hub.commands.lock().unwrap().is_empty());

    let ok = [("X-Playerbots-Token", "sekrit")];
    assert_eq!(s.post("/api/command", &ok, body).await.status, 200);
    assert_eq!(s.post("/api/bots/2/command", &ok, body).await.status, 200);
    assert_eq!(s.hub.commands.lock().unwrap().len(), 2);

    // authorisation is checked before the body: a bad body without a token is still a 401
    assert_eq!(s.post("/api/command", &[], "{nope").await.status, 401);
    // and with the token it is a 400
    assert_eq!(s.post("/api/command", &ok, "{nope").await.status, 400);
    s.shutdown().await;
}

#[tokio::test]
async fn empty_token_means_no_token() {
    let s = Server::start(Some(""), false).await;
    assert_eq!(
        s.post("/api/command", &[], r#"{"type":"pause"}"#)
            .await
            .status,
        200
    );
    s.shutdown().await;
}

#[tokio::test]
async fn cross_origin_posts_are_refused_without_a_token() {
    let s = Server::start(None, false).await;
    let body = r#"{"type":"pause"}"#;
    let evil = s
        .post("/api/command", &[("Origin", "http://evil.example")], body)
        .await;
    assert_eq!(evil.status, 403);
    let null_origin = s.post("/api/command", &[("Origin", "null")], body).await;
    assert_eq!(null_origin.status, 403);
    let same = format!("http://{}", s.addr);
    let ok = s
        .post("/api/command", &[("Origin", same.as_str())], body)
        .await;
    assert_eq!(ok.status, 200);
    assert_eq!(s.hub.commands.lock().unwrap().len(), 1);
    s.shutdown().await;
}

#[tokio::test]
async fn rebound_host_names_are_refused_on_loopback_without_a_token() {
    let s = Server::start(None, false).await;
    let body = r#"{"type":"pause"}"#;
    // DNS rebinding: the browser thinks evil.example is same-origin, but it resolves to us
    let host = [
        ("Host", "evil.example:8790"),
        ("Origin", "http://evil.example:8790"),
    ];
    assert_eq!(s.post("/api/command", &host, body).await.status, 403);
    // ... and it must not read anything either: every route refuses the foreign Host
    for path in [
        "/",
        "/index.html",
        "/app.js",
        "/style.css",
        "/map.png",
        "/api/bots",
        "/api/bots/1",
        "/api/stats",
        "/api/markers",
        "/api/map",
        "/api/issues",
        "/api/issues/pb:6f3b2e1c",
    ] {
        let r = s
            .request("GET", path, &[("Host", "evil.example")], None)
            .await;
        assert_eq!(r.status, 403, "GET {path}");
        let leaked = r.text();
        assert!(!leaked.contains("Bot "), "GET {path} leaked: {leaked}");
        // the same for a Host with a loopback look-alike
        let r = s
            .request("GET", path, &[("Host", "localhost.evil.example")], None)
            .await;
        assert_eq!(r.status, 403, "GET {path} look-alike");
    }
    // a request without a Host header (HTTP/1.0 style) is refused as well
    let mut stream = TcpStream::connect(s.addr).await.unwrap();
    stream
        .write_all(b"GET /api/bots HTTP/1.1\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    assert!(
        String::from_utf8_lossy(&raw).starts_with("HTTP/1.1 400")
            || String::from_utf8_lossy(&raw).starts_with("HTTP/1.1 403"),
        "{}",
        String::from_utf8_lossy(&raw)
    );
    assert_eq!(s.hub.commands.lock().unwrap().len(), 0);
    // loopback names by name or number work for reads and writes
    for h in [
        "localhost:8790",
        "127.0.0.1:8790",
        "[::1]:8790",
        "bots.localhost",
    ] {
        let r = s.request("GET", "/api/bots", &[("Host", h)], None).await;
        assert_eq!(r.status, 200, "{h}");
    }
    assert_eq!(s.get("/api/bots").await.status, 200);
    let local = [("Host", "localhost:8790")];
    assert_eq!(s.post("/api/command", &local, body).await.status, 200);
    assert_eq!(s.hub.commands.lock().unwrap().len(), 1);
    s.shutdown().await;
}

#[tokio::test]
async fn with_a_token_the_reads_still_check_the_host_but_a_token_post_does_not() {
    let s = Server::start(Some("t"), false).await;
    let body = r#"{"type":"pause"}"#;
    // the token protects the commands; it does not make a rebound page a reader
    for path in ["/api/bots", "/api/stats", "/"] {
        let r = s
            .request("GET", path, &[("Host", "bots.example")], None)
            .await;
        assert_eq!(r.status, 403, "GET {path}");
    }
    let r = s
        .request(
            "GET",
            "/api/bots",
            &[("Host", "bots.example"), ("x-playerbots-token", "t")],
            None,
        )
        .await;
    assert_eq!(r.status, 403, "a GET is never exempt");
    assert_eq!(s.get("/api/bots").await.status, 200);
    // a POST with the token works whatever the host name is called (a proxy, a script) ...
    let h = [("Host", "bots.example"), ("x-playerbots-token", "t")];
    assert_eq!(s.post("/api/command", &h, body).await.status, 200);
    // ... and without it, or with a wrong one, it does not
    let no = [("Host", "bots.example")];
    assert_eq!(s.post("/api/command", &no, body).await.status, 403);
    let wrong = [("Host", "bots.example"), ("x-playerbots-token", "nope")];
    assert_eq!(s.post("/api/command", &wrong, body).await.status, 403);
    // on a loopback Host the old rules hold: no token, no command
    assert_eq!(s.post("/api/command", &[], body).await.status, 401);
    assert_eq!(
        s.post("/api/command", &[("x-playerbots-token", "t")], body)
            .await
            .status,
        200
    );
    assert_eq!(s.hub.commands.lock().unwrap().len(), 2);
    s.shutdown().await;
}

#[tokio::test]
async fn a_non_loopback_bind_does_not_check_the_host_name() {
    // listening on every interface: the Host is whatever the operator's name for the machine is;
    // the token (and the operator's network) protect it
    let s = Server::start_on("0.0.0.0", Some("t"), false).await;
    let r = s
        .request("GET", "/api/bots", &[("Host", "bots.example:8790")], None)
        .await;
    assert_eq!(r.status, 200);
    let h = [("Host", "bots.example:8790"), ("x-playerbots-token", "t")];
    assert_eq!(
        s.post("/api/command", &h, r#"{"type":"pause"}"#)
            .await
            .status,
        200
    );
    s.shutdown().await;
}

#[tokio::test]
async fn serve_binds_and_stops_on_shutdown() {
    let hub = Arc::new(TestHub::default());
    // reserve a free port, then let `serve` bind it
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(pb_web::serve(
        hub,
        WebConfig {
            bind: addr,
            token: None,
        },
        async {
            let _ = stopped.await;
        },
    ));
    let mut up = false;
    for _ in 0..50 {
        if TcpStream::connect(addr).await.is_ok() {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(up, "serve never started listening");
    let _ = stop.send(());
    tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .expect("serve did not return after shutdown")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn serve_reports_a_busy_port() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap();
    let err = pb_web::serve(
        Arc::new(TestHub::default()),
        WebConfig {
            bind: addr,
            token: None,
        },
        std::future::pending(),
    )
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains(&addr.to_string()), "{err:#}");
}

#[tokio::test]
async fn issues_are_listed_and_served_by_fingerprint() {
    let s = Server::start(None, false).await;

    let r = s.get("/api/issues").await;
    assert_eq!(r.status, 200);
    assert!(r.header("content-type").starts_with("application/json"));
    assert_eq!(r.header("cache-control"), "no-store");
    let list = r.json();
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(list[0]["fingerprint"], "pb:6f3b2e1c");
    assert_eq!(list[0]["kind"], "quest_credit_missing");
    assert_eq!(list[0]["suspect"], "server");
    assert_eq!(list[0]["severity"], "medium");
    assert_eq!(list[0]["count"], 4);
    assert_eq!(list[0]["bots"], 2);
    assert_eq!(list[0]["first_seen"], 1_700_000_000_000u64);
    assert_eq!(list[0]["last_seen"], 1_700_000_900_000u64);
    assert_eq!(list[0]["state"], "open");
    assert_eq!(list[0]["reportable"], true);
    assert_eq!(list[1]["state"], "queued");

    let d = s.get("/api/issues/pb:6f3b2e1c").await;
    assert_eq!(d.status, 200, "{}", d.text());
    let d = d.json();
    assert_eq!(d["summary"]["fingerprint"], "pb:6f3b2e1c");
    assert!(d["entry"]
        .as_str()
        .unwrap()
        .contains("(playerbots)\r\nObserved"));
    assert_eq!(d["area"], "rust/crates/aaemu-game/src/systems/quests/");
    assert!(d["evidence"].is_null() && d["queued_at"].is_null());
    let sample = &d["samples"][0];
    assert_eq!(sample["bot"], "Bot1");
    assert_eq!(sample["expected"], "credit");
    assert_eq!(sample["actual"], "nothing");
    assert_eq!(sample["facts"][0][0], "quest");
    assert_eq!(sample["trail"][0], "-   10 ms  S2C L1 0x0123 SCFooPacket");
    assert_eq!(sample["server_log"][0], "14:03:09 [WARN] x - y");
    assert_eq!(sample["bot_log"][0], "killed a boar");

    let missing = s.get("/api/issues/pb:deadbeef").await;
    assert_eq!(missing.status, 404);
    assert!(missing.json()["error"]
        .as_str()
        .unwrap()
        .contains("pb:deadbeef"));
    // things that are not fingerprints never reach the hub
    for bad in [
        "/api/issues/not%20valid",
        "/api/issues/%2e%2e%2fetc",
        &format!("/api/issues/{}", "a".repeat(40)),
    ] {
        assert_eq!(s.get(bad).await.status, 400, "{bad}");
    }
    s.shutdown().await;
}

#[tokio::test]
async fn issue_actions_reach_the_hub() {
    let s = Server::start(None, false).await;
    for action in ["queue", "ignore", "resolve"] {
        let r = s
            .post(
                "/api/issues/pb:6f3b2e1c/action",
                &[],
                &format!(r#"{{"action":"{action}"}}"#),
            )
            .await;
        assert_eq!(r.status, 200, "{action}: {}", r.text());
        assert_eq!(r.json()["ok"], true);
    }
    assert_eq!(
        *s.hub.actions.lock().unwrap(),
        vec![
            (ISSUE.to_string(), IssueAction::Queue),
            (ISSUE.to_string(), IssueAction::Ignore),
            (ISSUE.to_string(), IssueAction::Resolve),
        ]
    );

    // the hub's refusals come back as 400 with their words
    let r = s
        .post(
            "/api/issues/pb:6f3b2e1c/action",
            &[],
            r#"{"action":"reopen"}"#,
        )
        .await;
    assert_eq!(r.status, 400);
    assert_eq!(r.json()["error"], "it is not closed");
    let r = s
        .post(
            "/api/issues/pb:00000009/action",
            &[],
            r#"{"action":"queue"}"#,
        )
        .await;
    assert_eq!(r.status, 400);
    assert_eq!(r.json()["error"], "no issue pb:00000009");

    // bad requests
    for body in [
        "",
        "{nope",
        r#"{}"#,
        r#"{"action":"delete"}"#,
        r#"{"action":7}"#,
    ] {
        let r = s.post("/api/issues/pb:6f3b2e1c/action", &[], body).await;
        assert_eq!(r.status, 400, "{body:?}: {}", r.text());
        assert!(r.json()["error"].is_string());
    }
    let r = s
        .post("/api/issues/bad%20id/action", &[], r#"{"action":"queue"}"#)
        .await;
    assert_eq!(r.status, 400);
    // a missing content type is refused
    let r = s
        .request(
            "POST",
            "/api/issues/pb:6f3b2e1c/action",
            &[],
            Some(r#"{"action":"queue"}"#),
        )
        .await;
    assert_eq!(r.status, 400);
    assert_eq!(
        s.hub.actions.lock().unwrap().len(),
        3,
        "nothing else got through"
    );
    s.shutdown().await;
}

#[tokio::test]
async fn issue_actions_need_the_token_and_the_right_origin() {
    let s = Server::start(Some("sekrit"), false).await;
    let body = r#"{"action":"queue"}"#;
    let path = "/api/issues/pb:6f3b2e1c/action";
    for headers in [
        vec![],
        vec![("x-playerbots-token", "nope")],
        vec![("x-playerbots-token", "")],
    ] {
        assert_eq!(
            s.post(path, &headers, body).await.status,
            401,
            "{headers:?}"
        );
    }
    // authorisation comes before the body and the id are looked at
    assert_eq!(s.post(path, &[], "{nope").await.status, 401);
    assert_eq!(
        s.post("/api/issues/bad%20id/action", &[], body)
            .await
            .status,
        401
    );
    assert!(s.hub.actions.lock().unwrap().is_empty());
    let ok = [("x-playerbots-token", "sekrit")];
    assert_eq!(s.post(path, &ok, body).await.status, 200);
    assert_eq!(s.hub.actions.lock().unwrap().len(), 1);
    // reads are open
    assert_eq!(s.get("/api/issues").await.status, 200);
    s.shutdown().await;

    // without a token a cross-origin page cannot queue anything
    let s = Server::start(None, false).await;
    let r = s
        .post(path, &[("Origin", "http://evil.example")], body)
        .await;
    assert_eq!(r.status, 403);
    let r = s
        .post(
            path,
            &[
                ("Host", "evil.example:8790"),
                ("Origin", "http://evil.example:8790"),
            ],
            body,
        )
        .await;
    assert_eq!(r.status, 403);
    assert!(s.hub.actions.lock().unwrap().is_empty());
    let same = format!("http://{}", s.addr);
    assert_eq!(
        s.post(path, &[("Origin", same.as_str())], body)
            .await
            .status,
        200
    );
    s.shutdown().await;
}
