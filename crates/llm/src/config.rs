//! Configuration of the optional LLM add-on (the `[llm]` table of `playerbots.toml`).

use serde::{Deserialize, Serialize};

/// Settings for the OpenAI-compatible chat endpoint.
///
/// The API key is **never** part of the config: it is read from the environment variable named by
/// [`api_key_env`](Self::api_key_env), so the config file can be shared and committed.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct LlmConfig {
    /// Master switch. When `false` the add-on does not exist: `Llm::from_config` returns `None`.
    pub enabled: bool,
    /// Base URL of the endpoint; requests go to `{base_url}/chat/completions`.
    pub base_url: String,
    /// Model name sent in every request.
    pub model: String,
    /// Name of the environment variable that holds the API key.
    pub api_key_env: String,
    /// Per-request timeout in seconds (connect + response), minimum 1.
    pub timeout_secs: u64,
    /// Maximum number of requests in flight at once, minimum 1.
    pub max_concurrent: usize,
    /// Requests allowed per rolling minute; `0` disables the limit.
    pub max_requests_per_minute: u32,
    /// Prompt + completion tokens allowed per UTC day; `0` disables the budget.
    pub daily_token_budget: u64,
    /// Sampling temperature for chat replies (JSON-mode requests are capped at 0.5).
    pub temperature: f32,
    /// Default reply length cap in tokens when a request does not set its own.
    pub max_reply_tokens: u32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "https://api.deepseek.com".to_string(),
            model: "deepseek-chat".to_string(),
            api_key_env: "DEEPSEEK_API_KEY".to_string(),
            timeout_secs: 20,
            max_concurrent: 2,
            max_requests_per_minute: 30,
            daily_token_budget: 200_000,
            temperature: 0.8,
            max_reply_tokens: 80,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_table_gives_defaults_and_disabled() {
        let cfg: LlmConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg, LlmConfig::default());
        assert!(!cfg.enabled);
        assert_eq!(cfg.base_url, "https://api.deepseek.com");
        assert_eq!(cfg.model, "deepseek-chat");
        assert_eq!(cfg.api_key_env, "DEEPSEEK_API_KEY");
        assert_eq!(cfg.timeout_secs, 20);
        assert_eq!(cfg.max_concurrent, 2);
        assert_eq!(cfg.max_requests_per_minute, 30);
        assert_eq!(cfg.daily_token_budget, 200_000);
        assert_eq!(cfg.max_reply_tokens, 80);
    }

    #[test]
    fn partial_table_overrides_only_given_fields() {
        let cfg: LlmConfig =
            serde_json::from_str(r#"{"enabled": true, "model": "other", "max_concurrent": 5}"#)
                .unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.model, "other");
        assert_eq!(cfg.max_concurrent, 5);
        assert_eq!(cfg.timeout_secs, 20);
    }
}
