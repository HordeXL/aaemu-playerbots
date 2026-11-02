//! The shared chat-completions client.

use std::borrow::Cow;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{Semaphore, SemaphorePermit, TryAcquireError};

use crate::choose;
use crate::config::LlmConfig;
use crate::error::LlmError;
use crate::limit::{utc_day, Cooldown, DailyBudget, RateWindow};
use crate::types::{ChatReply, ChatRequest, Choice, LlmUsage, Role};

/// How long a request waits for a free concurrency slot before it is refused.
const PERMIT_WAIT: Duration = Duration::from_millis(250);
/// Global pause after an HTTP 429 without a usable `Retry-After`.
const DEFAULT_COOLDOWN: Duration = Duration::from_secs(5);
const MAX_COOLDOWN: Duration = Duration::from_secs(60);
/// Temperature ceiling for JSON-mode requests (decisions, not banter).
const JSON_TEMPERATURE_CAP: f32 = 0.5;
/// Minimum reply budget for `choose` so the JSON object is not cut off.
const CHOOSE_MIN_TOKENS: u32 = 60;
const ERROR_TEXT_CAP: usize = 200;

#[derive(Default)]
struct Counters {
    requests: AtomicU64,
    failures: AtomicU64,
    rate_limited: AtomicU64,
    prompt_tokens: AtomicU64,
    completion_tokens: AtomicU64,
}

/// An OpenAI-compatible chat-completions client with a request window, a concurrency cap and a
/// daily token budget. One instance (behind an `Arc`) is shared by all bots.
///
/// It is never on a bot's tick path: callers spawn a task, await [`chat`](Self::chat) or
/// [`choose`](Self::choose) there and use the answer only if it arrives in time.
pub struct Llm {
    client: reqwest::Client,
    url: String,
    key: String,
    model: String,
    temperature: f32,
    default_max_tokens: u32,
    pub(crate) timeout: Duration,
    pub(crate) permit_wait: Duration,
    permits: Semaphore,
    window: RateWindow,
    budget: DailyBudget,
    cooldown: Cooldown,
    counters: Counters,
}

impl fmt::Debug for Llm {
    // Hand-written so the API key can never end up in a log line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Llm")
            .field("url", &self.url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl Llm {
    /// Builds the client from the config and the key in the environment variable named by
    /// `api_key_env`. Returns `None` (and says why at info level, never printing the key) when
    /// the add-on is disabled, the variable is missing or empty, or the config is unusable.
    pub fn from_config(cfg: &LlmConfig) -> Option<Arc<Llm>> {
        if !cfg.enabled {
            tracing::info!("LLM add-on disabled in the config");
            return None;
        }
        let var = cfg.api_key_env.trim();
        if var.is_empty() {
            tracing::info!("LLM add-on off: llm.api_key_env is empty");
            return None;
        }
        let key = match std::env::var(var) {
            Ok(value) if !value.trim().is_empty() => value,
            Ok(_) => {
                tracing::info!("LLM add-on off: environment variable {var} is empty");
                return None;
            }
            Err(_) => {
                tracing::info!("LLM add-on off: environment variable {var} is not set");
                return None;
            }
        };
        if !(cfg.base_url.starts_with("http://") || cfg.base_url.starts_with("https://")) {
            tracing::warn!("LLM add-on off: llm.base_url must start with http:// or https://");
            return None;
        }
        match Self::try_build(cfg, key) {
            Ok(llm) => {
                tracing::info!(
                    "LLM add-on on: model {} at {} ({} req/min, {} tokens/day)",
                    llm.model,
                    cfg.base_url,
                    cfg.max_requests_per_minute,
                    cfg.daily_token_budget
                );
                Some(Arc::new(llm))
            }
            Err(e) => {
                tracing::warn!("LLM add-on off: cannot build the HTTP client: {e}");
                None
            }
        }
    }

    /// Like [`from_config`](Self::from_config) with an explicit key and without the `enabled`
    /// check; for tests and tools.
    ///
    /// # Panics
    /// If the HTTP client cannot be built (TLS backend initialisation failure).
    pub fn with_key(cfg: &LlmConfig, key: String) -> Arc<Llm> {
        Arc::new(Self::try_build(cfg, key).expect("cannot build the HTTP client for pb-llm"))
    }

    pub(crate) fn try_build(cfg: &LlmConfig, key: String) -> Result<Llm, reqwest::Error> {
        let base = cfg.base_url.trim().trim_end_matches('/');
        let url = if base.ends_with("/chat/completions") {
            base.to_string()
        } else {
            format!("{base}/chat/completions")
        };
        let timeout = Duration::from_secs(cfg.timeout_secs.max(1));
        let mut builder = reqwest::Client::builder()
            .connect_timeout(timeout)
            .pool_idle_timeout(Duration::from_secs(60))
            .user_agent(concat!("aaemu-playerbots/", env!("CARGO_PKG_VERSION")));
        if is_loopback_url(&url) {
            // A local server (Ollama, a test double) must not be routed through a system proxy.
            builder = builder.no_proxy();
        }
        Ok(Llm {
            client: builder.build()?,
            url,
            key: key.trim().to_string(),
            model: cfg.model.clone(),
            temperature: cfg.temperature,
            default_max_tokens: cfg.max_reply_tokens.max(1),
            timeout,
            permit_wait: PERMIT_WAIT,
            permits: Semaphore::new(cfg.max_concurrent.max(1)),
            window: RateWindow::per_minute(cfg.max_requests_per_minute),
            budget: DailyBudget::new(cfg.daily_token_budget, utc_day()),
            cooldown: Cooldown::new(),
            counters: Counters::default(),
        })
    }

    /// The configured model name.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// One chat completion. Local limits are checked before anything is sent and fail fast with
    /// [`LlmError::RateLimited`] / [`LlmError::BudgetExhausted`]; nothing is queued.
    pub async fn chat(&self, req: ChatRequest) -> Result<ChatReply, LlmError> {
        let _permit = self.admit().await?;
        self.counters.requests.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let result = self.send(&req).await;
        match &result {
            Ok(reply) => tracing::debug!(
                "llm reply in {} ms ({} prompt + {} completion tokens)",
                started.elapsed().as_millis(),
                reply.prompt_tokens,
                reply.completion_tokens
            ),
            Err(LlmError::RateLimited) => {
                self.counters.rate_limited.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                self.counters.failures.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    "llm request failed after {} ms: {e}",
                    started.elapsed().as_millis()
                );
            }
        }
        result
    }

    /// Asks the model to pick one of `options` for the situation. The answer is requested as
    /// `{"choice": <index>, "reason": "<short>"}` in JSON mode and parsed tolerantly (code fences,
    /// surrounding prose, numeric strings); an index outside `0..options.len()` is a
    /// [`LlmError::BadResponse`]. With a single option no request is made.
    pub async fn choose(
        &self,
        system: &str,
        situation: &str,
        options: &[String],
    ) -> Result<Choice, LlmError> {
        match options.len() {
            0 => {
                return Err(LlmError::BadResponse(
                    "choose called without options".into(),
                ))
            }
            1 => {
                return Ok(Choice {
                    index: 0,
                    reason: String::new(),
                })
            }
            _ => {}
        }
        let reply = self
            .chat(ChatRequest {
                system: choose::system_prompt(system),
                messages: vec![crate::types::Msg::user(choose::user_prompt(
                    situation, options,
                ))],
                max_tokens: Some(self.default_max_tokens.max(CHOOSE_MIN_TOKENS)),
                json: true,
            })
            .await?;
        choose::parse_choice(&reply.text, options.len()).inspect_err(|e| {
            self.counters.failures.fetch_add(1, Ordering::Relaxed);
            tracing::warn!("llm choice unusable: {e}");
        })
    }

    /// A snapshot of the counters and today's budget.
    pub fn usage(&self) -> LlmUsage {
        let today = utc_day();
        LlmUsage {
            requests: self.counters.requests.load(Ordering::Relaxed),
            failures: self.counters.failures.load(Ordering::Relaxed),
            rate_limited: self.counters.rate_limited.load(Ordering::Relaxed),
            prompt_tokens: self.counters.prompt_tokens.load(Ordering::Relaxed),
            completion_tokens: self.counters.completion_tokens.load(Ordering::Relaxed),
            tokens_today: self.budget.used(today),
            budget_left: self.budget.left(today),
        }
    }

    /// Local gate: cool-down, budget, concurrency slot, request window (in that order, so a
    /// refused request never burns a window slot).
    async fn admit(&self) -> Result<SemaphorePermit<'_>, LlmError> {
        let now = Instant::now();
        if self.cooldown.is_active(now) {
            return Err(self.refused());
        }
        if self.budget.is_exhausted(utc_day()) {
            return Err(LlmError::BudgetExhausted);
        }
        let permit = match self.permits.try_acquire() {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => {
                match tokio::time::timeout(self.permit_wait, self.permits.acquire()).await {
                    Ok(Ok(permit)) => permit,
                    _ => return Err(self.refused()),
                }
            }
            Err(TryAcquireError::Closed) => {
                return Err(LlmError::Transport("concurrency gate closed".into()))
            }
        };
        if !self.window.try_take(Instant::now()) {
            return Err(self.refused());
        }
        Ok(permit)
    }

    fn refused(&self) -> LlmError {
        self.counters.rate_limited.fetch_add(1, Ordering::Relaxed);
        LlmError::RateLimited
    }

    async fn send(&self, req: &ChatRequest) -> Result<ChatReply, LlmError> {
        let system = system_text(req);
        let mut messages = Vec::with_capacity(req.messages.len() + 1);
        if !system.trim().is_empty() {
            messages.push(ApiMessage {
                role: Role::System.as_str(),
                content: &system,
            });
        }
        for m in &req.messages {
            messages.push(ApiMessage {
                role: m.role.as_str(),
                content: &m.content,
            });
        }
        let prompt_chars: usize = messages.iter().map(|m| m.content.chars().count()).sum();
        let temperature = if req.json {
            self.temperature.min(JSON_TEMPERATURE_CAP)
        } else {
            self.temperature
        };
        let body = ApiRequest {
            model: &self.model,
            messages,
            max_tokens: req.max_tokens.unwrap_or(self.default_max_tokens).max(1),
            temperature,
            stream: false,
            response_format: req.json.then_some(ResponseFormat {
                kind: "json_object",
            }),
        };

        let response = self
            .client
            .post(&self.url)
            .bearer_auth(&self.key)
            .timeout(self.timeout)
            .json(&body)
            .send()
            .await
            .map_err(map_transport)?;
        let status = response.status();
        if status.as_u16() == 429 {
            let pause = retry_after(response.headers()).unwrap_or(DEFAULT_COOLDOWN);
            self.cooldown.start(Instant::now(), pause);
            tracing::warn!("llm endpoint answered 429; pausing requests for {pause:?}");
            return Err(LlmError::RateLimited);
        }
        if !status.is_success() {
            return Err(LlmError::Http(status.as_u16()));
        }
        let bytes = response.bytes().await.map_err(map_transport)?;
        let parsed: ApiResponse = serde_json::from_slice(&bytes)
            .map_err(|e| LlmError::BadResponse(format!("invalid JSON: {e}")))?;

        let text = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default();
        let (prompt_tokens, completion_tokens) = match parsed.usage {
            Some(u) => (u.prompt_tokens, u.completion_tokens),
            None => (
                estimate_tokens(prompt_chars),
                estimate_tokens(text.chars().count()),
            ),
        };
        // Charge before judging the content: the tokens were spent either way.
        self.counters
            .prompt_tokens
            .fetch_add(prompt_tokens, Ordering::Relaxed);
        self.counters
            .completion_tokens
            .fetch_add(completion_tokens, Ordering::Relaxed);
        self.budget
            .charge(utc_day(), prompt_tokens.saturating_add(completion_tokens));

        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(LlmError::BadResponse("empty reply".into()));
        }
        Ok(ChatReply {
            text,
            prompt_tokens,
            completion_tokens,
        })
    }
}

/// The system prompt; JSON mode requires the word "JSON" somewhere in the prompt.
fn system_text(req: &ChatRequest) -> Cow<'_, str> {
    if !req.json {
        return Cow::Borrowed(&req.system);
    }
    let mentions_json = |s: &str| s.to_ascii_lowercase().contains("json");
    if mentions_json(&req.system) || req.messages.iter().any(|m| mentions_json(&m.content)) {
        Cow::Borrowed(&req.system)
    } else if req.system.trim().is_empty() {
        Cow::Borrowed("Reply with a JSON object.")
    } else {
        Cow::Owned(format!("{}\n\nReply with a JSON object.", req.system))
    }
}

/// Rough token count (about four characters per token) for answers without `usage`.
fn estimate_tokens(chars: usize) -> u64 {
    (chars as u64).div_ceil(4)
}

fn map_transport(e: reqwest::Error) -> LlmError {
    if e.is_timeout() {
        return LlmError::Timeout;
    }
    let mut text = e.without_url().to_string();
    if let Some((cut, _)) = text.char_indices().nth(ERROR_TEXT_CAP) {
        text.truncate(cut);
    }
    LlmError::Transport(text)
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let secs: u64 = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(secs).clamp(Duration::from_secs(1), MAX_COOLDOWN))
}

fn is_loopback_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(v6) = host.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[derive(Serialize)]
struct ApiRequest<'a> {
    model: &'a str,
    messages: Vec<ApiMessage<'a>>,
    max_tokens: u32,
    temperature: f32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

#[derive(Serialize)]
struct ApiMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Deserialize)]
struct ApiResponse {
    #[serde(default)]
    choices: Vec<ApiChoice>,
    usage: Option<ApiUsage>,
}

#[derive(Deserialize)]
struct ApiChoice {
    message: ApiReplyMessage,
}

#[derive(Deserialize)]
struct ApiReplyMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct ApiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn loopback_detection() {
        for url in [
            "http://127.0.0.1:8080/chat/completions",
            "http://localhost:11434/v1",
            "http://LOCALHOST/v1",
            "http://[::1]:9000/x",
            "http://user:pw@127.0.0.1:1/x",
        ] {
            assert!(is_loopback_url(url), "{url}");
        }
        for url in [
            "https://api.deepseek.com/chat/completions",
            "http://127.example.com/x",
            "https://localhost.evil.com/x",
            "http://10.0.0.5:8000/v1",
        ] {
            assert!(!is_loopback_url(url), "{url}");
        }
    }

    #[test]
    fn token_estimate_rounds_up() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(1), 1);
        assert_eq!(estimate_tokens(4), 1);
        assert_eq!(estimate_tokens(5), 2);
    }

    #[test]
    fn json_mode_adds_the_word_json_only_when_missing() {
        let mut req = ChatRequest::simple("be brief", "hello");
        req.json = true;
        assert!(system_text(&req).contains("JSON"));
        req.system = "answer as JSON".into();
        assert_eq!(system_text(&req), "answer as JSON");
        req.system = String::new();
        req.messages = vec![crate::types::Msg::user("give me json")];
        assert_eq!(system_text(&req), "");
        req.json = false;
        req.messages = vec![crate::types::Msg::user("hi")];
        assert_eq!(system_text(&req), "");
    }
}
