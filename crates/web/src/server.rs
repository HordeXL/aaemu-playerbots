//! The axum router, the handlers and [`serve`].

use std::future::{Future, IntoFuture};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::population::PolicyPatch;
use crate::types::{BotHub, Command, IssueActionRequest};

/// Header that carries the operator token on `POST` requests when one is configured.
pub const TOKEN_HEADER: &str = "x-playerbots-token";

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");
const POPULATION_JS: &str = include_str!("../assets/population.js");
const STYLE_CSS: &str = include_str!("../assets/style.css");

/// Path the hub's map image is served at (and the `image_url` a hub reports for it).
const MAP_PATH: &str = "/map.png";
/// How long a fetched map image is reused before the hub is asked again.
const MAP_TTL: Duration = Duration::from_secs(300);
/// Same, when the hub had no image yet (a bake may finish while the UI is open).
const MAP_TTL_MISSING: Duration = Duration::from_secs(5);
/// POST bodies are tiny JSON commands.
const MAX_BODY_BYTES: usize = 16 * 1024;
/// How long a graceful shutdown waits for open connections before giving up.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

const CSP: &str =
    "default-src 'self'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; \
script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// Settings of the web server.
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct WebConfig {
    /// Address to listen on. Default `127.0.0.1:8790`.
    pub bind: SocketAddr,
    /// When set, every `POST` must carry `x-playerbots-token: <token>`; `GET`s stay open.
    pub token: Option<String>,
}

impl std::fmt::Debug for WebConfig {
    // Hand-written: the token must never end up in a log line or a panic message.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebConfig")
            .field("bind", &self.bind)
            .field(
                "token",
                &self
                    .token
                    .as_ref()
                    .map(|t| if t.is_empty() { "" } else { "<hidden>" }),
            )
            .finish()
    }
}

impl Default for WebConfig {
    fn default() -> Self {
        WebConfig {
            bind: SocketAddr::from(([127, 0, 0, 1], 8790)),
            token: None,
        }
    }
}

struct AppState {
    hub: Arc<dyn BotHub>,
    token: Option<Vec<u8>>,
    /// Bound to a loopback address: without a token only loopback `Host` names are served.
    loopback_only: bool,
    map: Mutex<Option<CachedMap>>,
}

struct CachedMap {
    fetched: Instant,
    image: Option<MapImage>,
}

#[derive(Clone)]
struct MapImage {
    bytes: Bytes,
    /// Strong validator, quotes included.
    etag: String,
    /// The same without quotes; used as `?v=` cache buster.
    version: String,
}

impl AppState {
    /// The cached map image, refreshed from the hub when stale.
    async fn map_image(&self) -> Option<MapImage> {
        {
            let guard = self.map.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = guard.as_ref() {
                let ttl = if cached.image.is_some() {
                    MAP_TTL
                } else {
                    MAP_TTL_MISSING
                };
                if cached.fetched.elapsed() < ttl {
                    return cached.image.clone();
                }
            }
        }
        let hub = self.hub.clone();
        // The hub may read a file; keep that off the async workers.
        let fetched = tokio::task::spawn_blocking(move || hub.map_image())
            .await
            .ok()
            .flatten();
        let image = fetched.map(|png| {
            let version = format!("{:x}-{:016x}", png.len(), fnv1a64(&png));
            MapImage {
                etag: format!("\"{version}\""),
                version,
                bytes: Bytes::from(png),
            }
        });
        *self.map.lock().unwrap_or_else(|e| e.into_inner()) = Some(CachedMap {
            fetched: Instant::now(),
            image: image.clone(),
        });
        image
    }
}

fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Build the router: embedded UI, JSON API and command endpoints.
pub fn router(hub: Arc<dyn BotHub>, cfg: &WebConfig) -> Router {
    let state = Arc::new(AppState {
        hub,
        token: cfg
            .token
            .as_ref()
            .filter(|t| !t.is_empty())
            .map(|t| t.as_bytes().to_vec()),
        loopback_only: cfg.bind.ip().is_loopback(),
        map: Mutex::new(None),
    });
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/app.js", get(app_js))
        .route("/population.js", get(population_js))
        .route("/style.css", get(style_css))
        .route(MAP_PATH, get(map_png))
        .route("/api/bots", get(api_bots))
        .route("/api/bots/{id}", get(api_bot))
        .route("/api/stats", get(api_stats))
        .route("/api/markers", get(api_markers))
        .route("/api/map", get(api_map))
        .route("/api/issues", get(api_issues))
        .route("/api/issues/{fp}", get(api_issue))
        .route("/api/world", get(api_world))
        .route("/api/economy", get(api_economy))
        .route("/api/director", get(api_director))
        .route("/api/bots/{id}/command", post(api_bot_command))
        .route("/api/command", post(api_command))
        .route("/api/issues/{fp}/action", post(api_issue_action))
        .route("/api/director/policy", post(api_policy))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            host_guard,
        ))
        .with_state(state)
}

/// DNS-rebinding defence for every route, `GET`s included: on a loopback bind only requests whose
/// `Host` is a loopback name are served. A page on `evil.example` that resolves to 127.0.0.1 is
/// "same-origin" for the browser and could otherwise read the bots' state and chat. The one
/// exception keeps the token semantics: a `POST` that carries the right token is the operator's
/// own tool (a reverse proxy, a script) and needs no particular `Host`.
async fn host_guard(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    if st.loopback_only {
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok());
        let token_post = req.method() == Method::POST && token_matches(&st, req.headers());
        if !host.is_some_and(is_loopback_host) && !token_post {
            return ApiError::new(
                StatusCode::FORBIDDEN,
                "host name not allowed; use http://localhost or bind to a non-loopback address with a token",
            )
            .into_response();
        }
    }
    next.run(req).await
}

/// Whether the request carries the configured token (false when there is none).
fn token_matches(st: &AppState, headers: &HeaderMap) -> bool {
    const TOKEN: HeaderName = HeaderName::from_static(TOKEN_HEADER);
    match &st.token {
        Some(want) => headers
            .get(TOKEN)
            .is_some_and(|got| ct_eq(got.as_bytes(), want)),
        None => false,
    }
}

/// Bind `cfg.bind` and serve until `shutdown` completes.
pub async fn serve(
    hub: Arc<dyn BotHub>,
    cfg: WebConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(cfg.bind)
        .await
        .with_context(|| format!("binding the web UI to {}", cfg.bind))?;
    serve_listener(listener, hub, cfg, shutdown).await
}

/// Like [`serve`], on an already bound listener (`cfg.bind` is ignored). Handy for port 0.
pub async fn serve_listener(
    listener: TcpListener,
    hub: Arc<dyn BotHub>,
    cfg: WebConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let addr = listener
        .local_addr()
        .context("reading the web UI address")?;
    // (what decides about the Host check is where the socket really listens)
    let mut cfg = cfg;
    cfg.bind = addr;
    let has_token = cfg.token.as_ref().is_some_and(|t| !t.is_empty());
    if !has_token && !addr.ip().is_loopback() {
        tracing::warn!(%addr, "web UI is reachable from the network and has no token: anyone can command the bots");
    }
    tracing::info!(%addr, token = has_token, "web UI listening on http://{addr}/");

    let (stopping_tx, stopping_rx) = tokio::sync::oneshot::channel::<()>();
    let server = axum::serve(listener, router(hub, &cfg))
        .with_graceful_shutdown(async move {
            shutdown.await;
            let _ = stopping_tx.send(());
        })
        .into_future();
    tokio::pin!(server);
    tokio::select! {
        res = &mut server => res.context("web server failed"),
        () = async {
            if stopping_rx.await.is_ok() {
                tokio::time::sleep(SHUTDOWN_GRACE).await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            tracing::warn!("web UI still had open connections at shutdown; closing them");
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- errors and JSON helpers

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        ApiError {
            status,
            message: message.into(),
        }
    }
    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut res = json_response(&serde_json::json!({ "error": self.message }));
        *res.status_mut() = self.status;
        res
    }
}

fn json_response<T: Serialize + ?Sized>(value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(body) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            body,
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "serializing an API response failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"error":"serialization failed"}"#,
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------- static files

fn static_page(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        body,
    )
        .into_response()
}

async fn index() -> Response {
    static_page("text/html; charset=utf-8", INDEX_HTML)
}

async fn app_js() -> Response {
    static_page("text/javascript; charset=utf-8", APP_JS)
}

async fn population_js() -> Response {
    static_page("text/javascript; charset=utf-8", POPULATION_JS)
}

async fn style_css() -> Response {
    static_page("text/css; charset=utf-8", STYLE_CSS)
}

#[derive(Deserialize)]
struct MapQuery {
    v: Option<String>,
}

fn etag_matches(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',').any(|t| {
                let t = t.trim();
                t == "*" || t.strip_prefix("W/").unwrap_or(t) == etag
            })
        })
}

async fn map_png(
    State(st): State<Arc<AppState>>,
    Query(q): Query<MapQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(img) = st.map_image().await else {
        return ApiError::new(StatusCode::NOT_FOUND, "the hub has no map image").into_response();
    };
    // A versioned URL never changes its content: cache it for good. Without a version the
    // browser revalidates through the ETag.
    let cache = if q.v.is_some() {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let etag = HeaderValue::from_str(&img.etag).expect("etag is ascii");
    if etag_matches(&headers, &img.etag) {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, etag),
                (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
            ],
        )
            .into_response();
    }
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/png")),
            (header::ETAG, etag),
            (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        Body::from(img.bytes),
    )
        .into_response()
}

// ---------------------------------------------------------------- JSON API (GET)

async fn api_bots(State(st): State<Arc<AppState>>) -> Response {
    json_response(&st.hub.snapshots())
}

async fn api_bot(State(st): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Ok(id) = id.parse::<u32>() else {
        return ApiError::bad_request(format!("bot id {id:?} is not a number")).into_response();
    };
    match st.hub.detail(id) {
        Some(detail) => json_response(&detail),
        None => ApiError::new(StatusCode::NOT_FOUND, format!("no bot {id}")).into_response(),
    }
}

async fn api_stats(State(st): State<Arc<AppState>>) -> Response {
    json_response(&st.hub.stats())
}

async fn api_markers(State(st): State<Arc<AppState>>) -> Response {
    json_response(&st.hub.markers())
}

async fn api_map(State(st): State<Arc<AppState>>) -> Response {
    let mut info = st.hub.map_info();
    if info.image_url.as_deref() == Some(MAP_PATH) {
        info.image_url = st
            .map_image()
            .await
            .map(|img| format!("{MAP_PATH}?v={}", img.version));
    }
    json_response(&info)
}

/// Issue fingerprints are `pb:` and eight hex digits; accept anything short and plain (the hub
/// decides what exists) so odd input never reaches it.
fn plain_fingerprint(fp: &str) -> bool {
    !fp.is_empty()
        && fp.len() <= 32
        && fp
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
}

async fn api_issues(State(st): State<Arc<AppState>>) -> Response {
    json_response(&st.hub.issues())
}

async fn api_issue(State(st): State<Arc<AppState>>, Path(fp): Path<String>) -> Response {
    if !plain_fingerprint(&fp) {
        return ApiError::bad_request("not an issue fingerprint").into_response();
    }
    match st.hub.issue(&fp) {
        Some(detail) => json_response(&detail),
        None => ApiError::new(StatusCode::NOT_FOUND, format!("no issue {fp}")).into_response(),
    }
}

async fn api_world(State(st): State<Arc<AppState>>) -> Response {
    json_response(&st.hub.world())
}

async fn api_economy(State(st): State<Arc<AppState>>) -> Response {
    json_response(&st.hub.economy())
}

async fn api_director(State(st): State<Arc<AppState>>) -> Response {
    json_response(&st.hub.director())
}

// ---------------------------------------------------------------- commands (POST)

async fn api_policy(State(st): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(e) = authorize(&st, &headers) {
        return e.into_response();
    }
    if !is_json(&headers) {
        return ApiError::bad_request("send the policy as Content-Type: application/json")
            .into_response();
    }
    let patch: PolicyPatch = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => return ApiError::bad_request(format!("invalid policy: {e}")).into_response(),
    };
    if let Err(msg) = patch.validate(&Command::GOALS) {
        return ApiError::bad_request(msg).into_response();
    }
    match st.hub.set_policy(patch) {
        Ok(()) => json_response(&serde_json::json!({ "ok": true })),
        Err(msg) => ApiError::bad_request(msg).into_response(),
    }
}

async fn api_issue_action(
    State(st): State<Arc<AppState>>,
    Path(fp): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Authorize first so an unauthenticated caller learns nothing.
    if let Err(e) = authorize(&st, &headers) {
        return e.into_response();
    }
    if !plain_fingerprint(&fp) {
        return ApiError::bad_request("not an issue fingerprint").into_response();
    }
    if !is_json(&headers) {
        return ApiError::bad_request("send the action as Content-Type: application/json")
            .into_response();
    }
    let req: IssueActionRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return ApiError::bad_request(format!(
                "invalid action: {e}; expected {{\"action\":\"queue\"|\"ignore\"|\"resolve\"|\"reopen\"}}"
            ))
            .into_response()
        }
    };
    match st.hub.issue_action(&fp, req.action) {
        Ok(()) => json_response(&serde_json::json!({ "ok": true })),
        Err(msg) => ApiError::bad_request(msg).into_response(),
    }
}

async fn api_command(State(st): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    run_command(&st, &headers, &body, None)
}

async fn api_bot_command(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Authorize first so an unauthenticated caller learns nothing, not even whether the id parses.
    if let Err(e) = authorize(&st, &headers) {
        return e.into_response();
    }
    let Ok(id) = id.parse::<u32>() else {
        return ApiError::bad_request(format!("bot id {id:?} is not a number")).into_response();
    };
    run_command(&st, &headers, &body, Some(id))
}

fn run_command(st: &AppState, headers: &HeaderMap, body: &[u8], bot: Option<u32>) -> Response {
    if let Err(e) = authorize(st, headers) {
        return e.into_response();
    }
    if !is_json(headers) {
        return ApiError::bad_request("send the command as Content-Type: application/json")
            .into_response();
    }
    let cmd: Command = match serde_json::from_slice(body) {
        Ok(cmd) => cmd,
        Err(e) => return ApiError::bad_request(format!("invalid command: {e}")).into_response(),
    };
    if let Err(msg) = cmd.validate(bot) {
        return ApiError::bad_request(msg).into_response();
    }
    match st.hub.command(bot, cmd) {
        Ok(()) => json_response(&serde_json::json!({ "ok": true })),
        Err(msg) => ApiError::bad_request(msg).into_response(),
    }
}

/// Token check for `POST`s. Without a token, refuse cross-site browser requests instead.
fn authorize(st: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    match &st.token {
        Some(_) => {
            if token_matches(st, headers) {
                Ok(())
            } else {
                Err(ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    format!("missing or wrong {TOKEN_HEADER} header"),
                ))
            }
        }
        None => {
            // A page on another origin must not be able to drive the bots through the
            // operator's browser: same-origin requests only (CSRF), and on a loopback bind only
            // loopback host names (DNS rebinding).
            let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
            if st.loopback_only && !host.is_some_and(is_loopback_host) {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "host name not allowed; use http://localhost or set a token",
                ));
            }
            let Some(origin) = headers.get(header::ORIGIN) else {
                return Ok(());
            };
            let authority = origin
                .to_str()
                .ok()
                .and_then(|o| o.split_once("://"))
                .map(|(_, a)| a);
            match (authority, host) {
                (Some(a), Some(h)) if a.eq_ignore_ascii_case(h) => Ok(()),
                _ => Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "cross-origin request refused",
                )),
            }
        }
    }
}

/// `localhost`, `*.localhost` and loopback IP literals, with or without a port.
fn is_loopback_host(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => host.split(':').next().unwrap_or(""),
    };
    let name = name.to_ascii_lowercase();
    name == "localhost"
        || name.ends_with(".localhost")
        || name
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_compare() {
        assert!(ct_eq(b"secret", b"secret"));
        assert!(!ct_eq(b"secret", b"secreT"));
        assert!(!ct_eq(b"secret", b"secre"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn loopback_host_names() {
        for ok in [
            "localhost",
            "LOCALHOST:8790",
            "127.0.0.1:8790",
            "127.1.2.3",
            "[::1]:8790",
            "[::1]",
            "bots.localhost:80",
        ] {
            assert!(is_loopback_host(ok), "{ok}");
        }
        for bad in [
            "evil.example",
            "evil.example:8790",
            "localhost.evil.example",
            "10.0.0.5:8790",
            "[2001:db8::1]:8790",
            "",
        ] {
            assert!(!is_loopback_host(bad), "{bad}");
        }
    }

    #[test]
    fn etag_list_matching() {
        let mut h = HeaderMap::new();
        h.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_static("\"a\", W/\"b\""),
        );
        assert!(etag_matches(&h, "\"a\""));
        assert!(etag_matches(&h, "\"b\""));
        assert!(!etag_matches(&h, "\"c\""));
        h.insert(header::IF_NONE_MATCH, HeaderValue::from_static("*"));
        assert!(etag_matches(&h, "\"anything\""));
    }

    #[test]
    fn the_token_never_shows_in_debug_output() {
        let cfg = WebConfig {
            token: Some("s3kr1t-token".into()),
            ..WebConfig::default()
        };
        let shown = format!("{cfg:?} {:#?}", cfg);
        assert!(!shown.contains("s3kr1t"), "{shown}");
        assert!(shown.contains("<hidden>"), "{shown}");
        assert!(shown.contains("127.0.0.1:8790"));
        assert!(!format!("{:?}", WebConfig::default()).contains("hidden"));
    }

    #[test]
    fn default_config_binds_loopback() {
        let cfg = WebConfig::default();
        assert_eq!(cfg.bind.to_string(), "127.0.0.1:8790");
        assert!(cfg.token.is_none());
        let parsed: WebConfig = serde_json::from_str(r#"{"token":"x"}"#).unwrap();
        assert_eq!(parsed.bind.port(), 8790);
        assert_eq!(parsed.token.as_deref(), Some("x"));
    }
}
