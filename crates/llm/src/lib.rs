//! Optional LLM add-on for the player bots.
//!
//! A thin client for any OpenAI-compatible `chat/completions` endpoint (DeepSeek by default)
//! with the cost guards a swarm of bots needs: a rolling per-minute request window, a
//! concurrency cap, a global cool-down after HTTP 429, and a daily token budget that resets at
//! UTC midnight.
//!
//! - It is never on a bot's tick path: a caller spawns a task, awaits [`Llm::chat`] or
//!   [`Llm::choose`] there, and uses the answer only if it arrives in time.
//! - It is optional: [`Llm::from_config`] returns `None` when the add-on is disabled or the key
//!   is missing, and every caller must work without it.
//! - Model-written chat goes through [`persona::sanitize_reply`] before it is sent; that filter
//!   guarantees a reply never starts with `/` (the server would run it as a GM command).

mod choose;
mod client;
mod config;
mod error;
mod limit;
pub mod persona;
mod types;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

pub use client::Llm;
pub use config::LlmConfig;
pub use error::LlmError;
pub use persona::{sanitize_reply, system_prompt, Persona};
pub use types::{ChatReply, ChatRequest, Choice, LlmUsage, Msg, Role};
