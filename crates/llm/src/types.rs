//! Request, reply and statistics types.

use serde::{Deserialize, Serialize};

/// Author of a chat message.
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

impl From<&str> for Role {
    /// `"system"` / `"assistant"` (any case); everything else is `User`.
    fn from(s: &str) -> Self {
        if s.eq_ignore_ascii_case("system") {
            Role::System
        } else if s.eq_ignore_ascii_case("assistant") {
            Role::Assistant
        } else {
            Role::User
        }
    }
}

impl From<String> for Role {
    fn from(s: String) -> Self {
        Role::from(s.as_str())
    }
}

/// One message of the conversation.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Msg {
    pub role: Role,
    pub content: String,
}

impl Msg {
    pub fn new(role: impl Into<Role>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }
}

/// A chat completion request.
#[derive(Clone, Debug, Default)]
pub struct ChatRequest {
    /// System prompt; skipped when empty.
    pub system: String,
    /// Conversation so far (oldest first).
    pub messages: Vec<Msg>,
    /// Reply length cap; `None` uses `LlmConfig::max_reply_tokens`.
    pub max_tokens: Option<u32>,
    /// Ask for a JSON object (`response_format: json_object`).
    pub json: bool,
}

impl ChatRequest {
    /// A system prompt plus a single user message.
    pub fn simple(system: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            messages: vec![Msg::user(user)],
            max_tokens: None,
            json: false,
        }
    }
}

/// The model's answer with the token usage that was charged to the budget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatReply {
    pub text: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// Result of [`Llm::choose`](crate::Llm::choose).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    /// Zero-based index into the options that were offered; always in range.
    pub index: usize,
    /// The model's short justification (may be empty).
    pub reason: String,
}

/// Counters for the operator UI. All token numbers are what the endpoint reported (or an
/// estimate when it reported none).
#[derive(Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LlmUsage {
    /// Requests actually sent to the endpoint.
    pub requests: u64,
    /// Sent requests that failed (timeout, transport, HTTP, unusable answer).
    pub failures: u64,
    /// Requests refused by a local limit or answered with HTTP 429.
    pub rate_limited: u64,
    /// Prompt tokens since start.
    pub prompt_tokens: u64,
    /// Completion tokens since start.
    pub completion_tokens: u64,
    /// Tokens charged against today's (UTC) budget.
    pub tokens_today: u64,
    /// Tokens left today; `u64::MAX` when the budget is disabled.
    pub budget_left: u64,
}
