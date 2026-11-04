//! The game server's own web API, as an optional second opinion.
//!
//! With `[population] server_api` set, a background task asks the server every
//! `server_poll_secs` for
//!
//! * `GET /status` (public): uptime, players online, job counts, as the `label: value` rows of the
//!   page. Without the key its `Players online` count (which includes every bot of every process) is
//!   all there is, and the web UI takes our own bots in the world off it ("approx.");
//! * `GET /api/world/logged-characters` (needs the bearer key): the exact list of online characters.
//!   The ones that are not our bots are the real players; the census sees only those near a bot,
//!   so the difference is the part of the world nobody is watching;
//! * `GET /api/auction/list` (needs the key; every fourth poll): every lot, from which the economy
//!   board gets the exact supply and price of each item.
//!
//! The key comes from the environment variable named in `server_api_key_env` or the first line of
//! `server_api_key_file`; it is held in a [`Secret`], sent only as the `Authorization` header and
//! never logged, shown or part of an error text. Everything is optional and fail-soft: an
//! unreachable server is a line in the web UI, not an error. Responses are read up to
//! [`MAX_BODY`] bytes and parsed defensively.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context;
use parking_lot::Mutex;
use pb_web::ServerView;
use rustc_hash::FxHashMap;
use serde_json::Value;

use super::economy::{Economy, LotStat, ServerLots};
use crate::config::PopulationCfg;
use crate::secrets::Secret;

/// The most bytes of a response that are read.
pub const MAX_BODY: usize = 16 * 1024 * 1024;
/// Time allowed for one request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// The auction list is fetched on every this many polls.
const LOTS_EVERY: u32 = 4;
/// The most `label: value` rows of the status page that are shown.
const MAX_STATUS_ROWS: usize = 16;
/// The longest label or value of such a row (characters).
const MAX_STATUS_TEXT: usize = 120;

/// What the web UI shows about the server API, written by the poller.
#[derive(Debug, Default)]
pub struct ServerState {
    view: Mutex<ServerView>,
}

impl ServerState {
    /// A state that says the API is (not) configured.
    pub fn new(configured: bool) -> ServerState {
        ServerState {
            view: Mutex::new(ServerView {
                configured,
                ..ServerView::default()
            }),
        }
    }

    /// A copy of the view.
    pub fn view(&self) -> ServerView {
        self.view.lock().clone()
    }

    /// Changes the view.
    pub fn update(&self, f: impl FnOnce(&mut ServerView)) {
        f(&mut self.view.lock());
    }
}

/// The API's address and key.
pub struct ServerApi {
    base: String,
    key: Option<Secret>,
}

impl std::fmt::Debug for ServerApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerApi")
            .field("base", &self.base)
            .field("key", &self.key.as_ref().map(|_| "<hidden>"))
            .finish()
    }
}

impl ServerApi {
    /// An API at `base` (`http://host:port`, no trailing slash needed).
    pub fn new(base: &str, key: Option<Secret>) -> ServerApi {
        ServerApi {
            base: base.trim_end_matches('/').to_owned(),
            key,
        }
    }

    /// Whether a key is set (the routes beyond `/status` need one).
    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }
}

/// The bearer key: the environment variable `env` if it is set and not empty, else the first line
/// of `file`. `None` when neither gives one.
pub fn load_key(env: &str, file: Option<&Path>) -> Option<Secret> {
    if !env.is_empty() {
        if let Ok(v) = std::env::var(env) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(Secret::new(v));
            }
        }
    }
    let text = std::fs::read_to_string(file?).ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| Secret::new(line))
}

// ---------------------------------------------------------------------------------- parsing

/// The `label: value` rows of the `/status` page (`Server uptime: 00:12:09<br/>Players online: 3<br/>...`).
pub fn parse_status(html: &str) -> Vec<(String, String)> {
    let normalized = html.replace("<br/>", "\n").replace("<br>", "\n");
    let mut out = Vec::new();
    for line in normalized.lines() {
        let mut text = String::with_capacity(line.len());
        let mut in_tag = false;
        for c in line.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag && !c.is_control() => text.push(c),
                _ => {}
            }
        }
        let Some((label, value)) = text.split_once(':') else {
            continue;
        };
        let (label, value) = (label.trim(), value.trim());
        if label.is_empty() {
            continue;
        }
        out.push((
            label.chars().take(MAX_STATUS_TEXT).collect(),
            value.chars().take(MAX_STATUS_TEXT).collect(),
        ));
        if out.len() >= MAX_STATUS_ROWS {
            break;
        }
    }
    out
}

/// The `Players online` count of the status page, when it has one.
fn status_online(rows: &[(String, String)]) -> Option<u32> {
    rows.iter()
        .find(|r| r.0.eq_ignore_ascii_case("players online"))
        .and_then(|r| r.1.trim().parse().ok())
}

/// The online characters of `/api/world/logged-characters` as `(character id, online)`.
pub fn parse_characters(body: &[u8]) -> Result<Vec<(u32, bool)>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "the character list is not JSON")?;
    let list = v
        .as_array()
        .ok_or("the character list is not a JSON array")?;
    Ok(list
        .iter()
        .filter_map(|c| {
            let id = u32::try_from(c.get("Id")?.as_u64()?).ok()?;
            let online = c.get("IsOnline").and_then(Value::as_bool).unwrap_or(true);
            Some((id, online))
        })
        .collect())
}

/// The lots of `/api/auction/list` per item. `at_ms` is unix milliseconds. A lot is priced at its
/// buyout, or at its start price when it has none.
pub fn parse_lots(body: &[u8], at_ms: u64) -> Result<ServerLots, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "the auction list is not JSON")?;
    let list = v
        .get("items")
        .and_then(Value::as_array)
        .ok_or("the auction list has no items")?;
    let mut prices: FxHashMap<u32, (u32, Vec<u32>)> = FxHashMap::default();
    let mut total = 0u32;
    for lot in list {
        let item = lot.get("Item");
        let Some(template) = item
            .and_then(|i| i.get("TemplateId"))
            .and_then(Value::as_u64)
            .and_then(|t| u32::try_from(t).ok())
        else {
            continue;
        };
        let count = item
            .and_then(|i| i.get("Count"))
            .and_then(Value::as_i64)
            .unwrap_or(1)
            .clamp(1, i64::from(i32::MAX)) as u32;
        let money = |key: &str| lot.get(key).and_then(Value::as_i64).unwrap_or(0).max(0);
        let price = match money("DirectMoney") {
            0 => money("StartMoney"),
            p => p,
        };
        let unit = (price / i64::from(count)).clamp(0, i64::from(u32::MAX)) as u32;
        let e = prices.entry(template).or_default();
        e.0 = e.0.saturating_add(count);
        e.1.push(unit);
        total = total.saturating_add(1);
    }
    let items = prices
        .into_iter()
        .map(|(t, (units, mut p))| {
            p.sort_unstable();
            let n = p.len();
            let median = if n % 2 == 1 {
                p[n / 2]
            } else {
                (u64::from(p[n / 2 - 1]) + u64::from(p[n / 2])).div_ceil(2) as u32
            };
            (
                t,
                LotStat {
                    lots: n as u32,
                    units,
                    lowest_unit: p[0],
                    median_unit: median,
                },
            )
        })
        .collect();
    Ok(ServerLots {
        at_ms,
        total,
        items,
    })
}

// ----------------------------------------------------------------------------------- polling

/// A short, key-free text for a failed request.
fn describe(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".to_owned()
    } else if e.is_connect() {
        "cannot connect".to_owned()
    } else if e.is_body() || e.is_decode() {
        "the answer could not be read".to_owned()
    } else {
        "the request failed".to_owned()
    }
}

/// `GET path` on the API; the body, or a short text that says why not.
async fn get(
    client: &reqwest::Client,
    api: &ServerApi,
    path: &str,
    auth: bool,
) -> Result<Vec<u8>, String> {
    let mut req = client.get(format!("{}{path}", api.base));
    if auth {
        let Some(key) = &api.key else {
            return Err("no API key is set".to_owned());
        };
        req = req.bearer_auth(key.expose());
    }
    let mut resp = req.send().await.map_err(|e| describe(&e))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            401 | 403 => format!("the API key was refused (HTTP {})", status.as_u16()),
            404 => "not found (HTTP 404)".to_owned(),
            n => format!("HTTP {n}"),
        });
    }
    if resp.content_length().is_some_and(|n| n > MAX_BODY as u64) {
        return Err("the answer is too big".to_owned());
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| describe(&e))? {
        if body.len() + chunk.len() > MAX_BODY {
            return Err("the answer is too big".to_owned());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// One poll: the status page, and with a key the online characters and (when `lots`) the auction
/// list. Updates `state` and `economy`. `is_bot` says whether a character id is one of our bots.
pub async fn poll_once(
    client: &reqwest::Client,
    api: &ServerApi,
    is_bot: &(dyn Fn(u32) -> bool + Sync),
    state: &ServerState,
    economy: &Economy,
    lots: bool,
    now_ms: u64,
) {
    let started = Instant::now();
    let status = match get(client, api, "/status", false).await {
        Ok(b) => parse_status(&String::from_utf8_lossy(&b)),
        Err(e) => {
            state.update(|v| {
                v.reachable = false;
                v.error = Some(e);
            });
            return;
        }
    };
    let latency_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
    let mut error = None;
    // (characters online, of them ours); without the list the status page's count has no split
    let mut online: Option<(u32, Option<u32>)> = None;
    let mut auction = None;
    if api.has_key() {
        match get(client, api, "/api/world/logged-characters", true)
            .await
            .and_then(|b| parse_characters(&b))
        {
            Ok(list) => {
                let on: Vec<u32> = list.iter().filter(|c| c.1).map(|c| c.0).collect();
                online = Some((
                    on.len() as u32,
                    Some(on.iter().filter(|id| is_bot(**id)).count() as u32),
                ));
            }
            Err(e) => error = Some(format!("character list: {e}")),
        }
        if lots {
            match get(client, api, "/api/auction/list", true)
                .await
                .and_then(|b| parse_lots(&b, now_ms))
            {
                Ok(l) => {
                    auction = Some(l.total);
                    economy.set_server_lots(l);
                }
                Err(e) => error = error.or(Some(format!("auction list: {e}"))),
            }
        }
    }
    if online.is_none() {
        online = status_online(&status).map(|n| (n, None));
    }
    state.update(|v| {
        v.reachable = true;
        v.error = error;
        v.checked_ms = now_ms;
        v.status = status;
        v.latency_ms = latency_ms;
        v.online = online.map(|o| o.0);
        v.online_bots = online.and_then(|o| o.1);
        if lots {
            // (a failed list keeps the last count; the board drops it when it goes stale)
            if let Some(n) = auction {
                v.auction_lots = Some(n);
            }
        }
    });
}

/// Builds the HTTP client of the poller (no proxy: the server is on this machine or its network).
pub fn client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .user_agent("aaemu-playerbots")
        .build()
        .context("building the HTTP client for the server API")
}

/// The poll loop, until `cancel` fires.
pub async fn run(
    api: ServerApi,
    poll: Duration,
    hub: std::sync::Arc<crate::hub::Hub>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let client = match client() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("server API: {e:#}");
            return;
        }
    };
    tracing::info!(
        base = %api.base,
        key = api.has_key(),
        every_secs = poll.as_secs(),
        "polling the server API"
    );
    let mut n = 0u32;
    loop {
        let is_bot = |id: u32| hub.is_bot_char(id);
        poll_once(
            &client,
            &api,
            &is_bot,
            &hub.pop.server,
            &hub.pop.economy,
            n.is_multiple_of(LOTS_EVERY),
            crate::hub::unix_ms(),
        )
        .await;
        n = n.wrapping_add(1);
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(poll) => {}
        }
    }
}

/// Starts the poller when `[population] server_api` is set.
pub fn spawn(
    cfg: &PopulationCfg,
    hub: std::sync::Arc<crate::hub::Hub>,
    cancel: tokio_util::sync::CancellationToken,
) -> Option<tokio::task::JoinHandle<()>> {
    if cfg.server_api.is_empty() {
        return None;
    }
    let key = load_key(&cfg.server_api_key_env, cfg.server_api_key_file.as_deref());
    if key.is_none() {
        tracing::info!(
            "server API: no key (set {} or server_api_key_file): only /status is used",
            if cfg.server_api_key_env.is_empty() {
                "server_api_key_env"
            } else {
                cfg.server_api_key_env.as_str()
            }
        );
    }
    let api = ServerApi::new(&cfg.server_api, key);
    let poll = Duration::from_secs(cfg.server_poll_secs.max(5));
    Some(tokio::spawn(run(api, poll, hub, cancel)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn the_status_page_becomes_rows() {
        let html =
            "Server uptime: 00:12:09<br/>Players online: 3<br/>Number of TaskManager Jobs: 0<br/>\
                    SkillTlIds in use: 0 / 4096 (peak 2, gets 5)<br/>";
        let rows = parse_status(html);
        assert_eq!(rows[0], ("Server uptime".into(), "00:12:09".into()));
        assert_eq!(rows[1], ("Players online".into(), "3".into()));
        assert_eq!(rows[2], ("Number of TaskManager Jobs".into(), "0".into()));
        assert_eq!(rows[3].0, "SkillTlIds in use");
        assert_eq!(rows.len(), 4);
        // markup is dropped, lines without a label are skipped, rows and text are bounded
        let odd = format!(
            "<b>Bold</b>: <i>x</i><br>no colon here<br>: empty label<br>{}",
            (0..40)
                .map(|i| format!("k{i}: v{i}<br/>"))
                .collect::<String>()
        );
        let rows = parse_status(&odd);
        assert_eq!(rows[0], ("Bold".into(), "x".into()));
        assert_eq!(rows.len(), MAX_STATUS_ROWS);
        let long = parse_status(&format!("a: {}", "z".repeat(500)));
        assert_eq!(long[0].1.len(), MAX_STATUS_TEXT);
        assert!(parse_status("").is_empty());
    }

    #[test]
    fn the_character_list_is_read_defensively() {
        let body = br#"[{"Id":5,"Name":"A","Level":10,"IsOnline":true},
                       {"Id":6,"Name":"B","IsOnline":false},
                       {"Id":7,"Name":"C"},
                       {"Name":"no id"},
                       {"Id":-3,"IsOnline":true},
                       {"Id":99999999999,"IsOnline":true}]"#;
        assert_eq!(
            parse_characters(body).unwrap(),
            vec![(5, true), (6, false), (7, true)]
        );
        assert!(parse_characters(b"{}").is_err());
        assert!(parse_characters(b"not json").is_err());
        assert_eq!(parse_characters(b"[]").unwrap(), vec![]);
    }

    #[test]
    fn lots_are_grouped_per_item_with_a_unit_price() {
        let body = br#"{"items":[
            {"Item":{"TemplateId":3412,"Count":10},"DirectMoney":3000,"StartMoney":1000},
            {"Item":{"TemplateId":3412,"Count":5},"DirectMoney":2000,"StartMoney":500},
            {"Item":{"TemplateId":3412,"Count":1},"DirectMoney":0,"StartMoney":700},
            {"Item":{"TemplateId":3411,"Count":2},"DirectMoney":100},
            {"Item":null,"DirectMoney":5},
            {"DirectMoney":5}
        ]}"#;
        let l = parse_lots(body, 42).unwrap();
        assert_eq!((l.at_ms, l.total), (42, 4));
        let copper = l.items[&3412];
        assert_eq!(copper.lots, 3);
        assert_eq!(copper.units, 16);
        // unit prices 300, 400, 700 (the last by its start price)
        assert_eq!((copper.lowest_unit, copper.median_unit), (300, 400));
        assert_eq!(l.items[&3411].median_unit, 50);
        assert!(parse_lots(b"[]", 0).is_err());
        assert!(parse_lots(b"garbage", 0).is_err());
        assert_eq!(parse_lots(br#"{"items":[]}"#, 0).unwrap().total, 0);
    }

    #[test]
    fn the_key_comes_from_the_environment_or_a_file_and_never_shows() {
        let dir = crate::testutil::temp_dir("serverapi-key");
        let file = dir.join("key.txt");
        std::fs::write(&file, "  file-key  \nsecond line\n").unwrap();
        // (a variable name nobody sets)
        let var = "PLAYERBOTS_TEST_SERVER_API_KEY_UNSET";
        assert_eq!(
            load_key(var, Some(&file)).unwrap().expose(),
            "file-key",
            "the first line, trimmed"
        );
        assert!(load_key("", Some(&dir.join("missing.txt"))).is_none());
        assert!(load_key("", None).is_none());
        std::fs::write(&file, "\n").unwrap();
        assert!(
            load_key(var, Some(&file)).is_none(),
            "an empty file is no key"
        );

        let api = ServerApi::new(
            "http://127.0.0.1:1280/",
            Some(Secret::new("hunter2-secret")),
        );
        let shown = format!("{api:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("<hidden>") && shown.contains("127.0.0.1:1280"));
        assert_eq!(api.base, "http://127.0.0.1:1280", "no trailing slash");
    }

    /// A one-shot HTTP server for the poller: answers by path, checks the bearer key.
    async fn fake_server(
        key: &'static str,
        rejects: bool,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let mut seen = Vec::new();
            for _ in 0..3 {
                let Ok(Ok((mut s, _))) =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept()).await
                else {
                    break;
                };
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("").to_owned();
                let authed = req
                    .lines()
                    .any(|l| l.eq_ignore_ascii_case(&format!("authorization: Bearer {key}")));
                seen.push(format!("{path} auth={authed}"));
                let (code, body) = match path.as_str() {
                    "/status" => (200, "Server uptime: 01:00:00<br/>Players online: 5<br/>".to_owned()),
                    _ if !authed || rejects => (401, String::new()),
                    "/api/world/logged-characters" => (
                        200,
                        r#"[{"Id":1,"IsOnline":true},{"Id":2,"IsOnline":true},{"Id":3,"IsOnline":true},{"Id":4,"IsOnline":false}]"#.to_owned(),
                    ),
                    "/api/auction/list" => (
                        200,
                        r#"{"items":[{"Item":{"TemplateId":3412,"Count":2},"DirectMoney":600}]}"#.to_owned(),
                    ),
                    _ => (404, String::new()),
                };
                let resp = format!(
                    "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            }
            seen
        });
        (base, task)
    }

    #[tokio::test]
    async fn a_poll_reads_the_status_the_players_and_the_lots() {
        let (base, server) = fake_server("sekrit", false).await;
        let api = ServerApi::new(&base, Some(Secret::new("sekrit")));
        let state = ServerState::new(true);
        let economy = Economy::default();
        // characters 1 and 2 are ours
        let is_bot = |id: u32| id == 1 || id == 2;
        poll_once(
            &client().unwrap(),
            &api,
            &is_bot,
            &state,
            &economy,
            true,
            1_000,
        )
        .await;
        let v = state.view();
        assert!(v.configured && v.reachable, "{v:?}");
        assert_eq!(v.error, None);
        assert_eq!(v.status[1], ("Players online".into(), "5".into()));
        assert_eq!((v.online, v.online_bots), (Some(3), Some(2)));
        assert_eq!(v.auction_lots, Some(1));
        assert_eq!(v.checked_ms, 1_000);
        let board = economy.view(2_000, &NoItems, &[], 10);
        assert_eq!(board.sources.server_lots, Some(1));
        let seen = server.await.unwrap();
        assert_eq!(
            seen,
            [
                "/status auth=false",
                "/api/world/logged-characters auth=true",
                "/api/auction/list auth=true"
            ]
        );
    }

    struct NoItems;
    impl super::super::economy::ItemInfo for NoItems {
        fn name(&self, _: u32) -> Option<String> {
            None
        }
        fn vendor_price(&self, _: u32) -> u32 {
            0
        }
    }

    #[tokio::test]
    async fn a_refused_key_and_a_dead_server_are_lines_not_errors() {
        // the server answers /status but refuses the key
        let (base, server) = fake_server("right", false).await;
        let api = ServerApi::new(&base, Some(Secret::new("wrong")));
        let state = ServerState::new(true);
        let economy = Economy::default();
        poll_once(
            &client().unwrap(),
            &api,
            &|_| false,
            &state,
            &economy,
            true,
            5,
        )
        .await;
        let v = state.view();
        assert!(v.reachable, "the status page answered");
        let err = v.error.unwrap();
        assert!(err.contains("refused"), "{err}");
        assert!(!err.contains("wrong") && !err.contains("right"), "{err}");
        assert_eq!(
            (v.online, v.online_bots),
            (Some(5), None),
            "the status page's count stands in, without the split"
        );
        let _ = server.await;

        // without a key only the status page is used
        let (base, server) = fake_server("k", false).await;
        let state = ServerState::new(true);
        poll_once(
            &client().unwrap(),
            &ServerApi::new(&base, None),
            &|_| false,
            &state,
            &economy,
            true,
            6,
        )
        .await;
        let v = state.view();
        assert!(v.reachable && v.error.is_none());
        assert_eq!((v.online, v.online_bots), (Some(5), None));
        let seen = server.await.unwrap();
        assert_eq!(seen, ["/status auth=false"]);

        // nothing listens: unreachable, with a short text and the old rows kept
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            format!("http://{}", l.local_addr().unwrap())
        };
        state.update(|v| v.online = Some(9));
        poll_once(
            &client().unwrap(),
            &ServerApi::new(&dead, None),
            &|_| false,
            &state,
            &economy,
            true,
            7,
        )
        .await;
        let v = state.view();
        assert!(!v.reachable);
        assert!(v.error.is_some());
        assert_eq!(v.online, Some(9), "the last known count stays");
        assert!(
            !v.error.unwrap().contains("127.0.0.1"),
            "no address in the text"
        );
    }
}
