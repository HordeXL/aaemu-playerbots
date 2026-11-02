//! Operator web UI for aaemu-playerbots.
//!
//! A live world map of every bot, a JSON API and a small command API. The crate knows nothing
//! about bots: it only talks to a [`BotHub`] and the plain snapshot types defined in this crate.
//! The single-page UI (vanilla JS on a canvas, no external libraries) is embedded in the binary,
//! so it works offline.
//!
//! ```text
//! GET  /                       the UI (also /app.js, /style.css)
//! GET  /map.png                the world map from BotHub::map_image (ETag, long cache with ?v=)
//! GET  /api/bots               [BotSnapshot]
//! GET  /api/bots/{id}          BotDetail            (404 if unknown)
//! GET  /api/stats              HubStats
//! GET  /api/markers            [Marker]
//! GET  /api/map                MapInfo
//! POST /api/bots/{id}/command  Command as JSON      -> {"ok":true} or 400 {"error":"…"}
//! POST /api/command            Command as JSON (global commands: spawn, despawn, pause all, …)
//! ```
//!
//! With [`WebConfig::token`] set, `POST`s need the header `x-playerbots-token: <token>` (401
//! otherwise); `GET`s stay open. Without a token, `POST`s must be same-origin and carry
//! `Content-Type: application/json`. When the server listens on a loopback address, *every* route
//! (`GET`s included) only answers requests whose `Host` is a loopback name (DNS-rebinding
//! defence; a `POST` with the right token is exempt).
//!
//! ```no_run
//! # async fn demo(hub: std::sync::Arc<dyn pb_web::BotHub>) -> anyhow::Result<()> {
//! let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
//! let web = tokio::spawn(pb_web::serve(hub, pb_web::WebConfig::default(), async {
//!     let _ = stop_rx.await;
//! }));
//! // … later: stop_tx.send(()); web.await??;
//! # Ok(()) }
//! ```

mod server;
mod types;

pub use server::{router, serve, serve_listener, WebConfig, TOKEN_HEADER};
pub use types::{
    Activity, BotDetail, BotHub, BotSnapshot, Command, HubStats, LlmStats, LogLine, MapInfo,
    Marker, MarkerKind, QuestLine, WorldRect,
};
