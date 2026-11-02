//! Error type of the LLM add-on.

use thiserror::Error;

/// Why a request did not produce a usable answer. None of these is fatal: callers fall back to
/// canned lines or the utility scorer.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LlmError {
    /// A local limit (requests per minute, concurrency cap, cool-down) or an HTTP 429 from the
    /// endpoint. Nothing was queued; try again later or skip.
    #[error("rate limited")]
    RateLimited,
    /// The daily token budget is used up; it resets at UTC midnight.
    #[error("daily token budget exhausted")]
    BudgetExhausted,
    /// The endpoint did not answer within the per-request timeout.
    #[error("request timed out")]
    Timeout,
    /// The endpoint answered with a non-success HTTP status (other than 429).
    #[error("http status {0}")]
    Http(u16),
    /// Connection-level failure (DNS, refused, reset, TLS).
    #[error("transport error: {0}")]
    Transport(String),
    /// The answer arrived but is unusable (malformed JSON, no choices, invalid choice index).
    #[error("bad response: {0}")]
    BadResponse(String),
}
