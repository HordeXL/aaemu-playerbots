//! Configuration of the optional LLM add-on (the `[llm]` table of `playerbots.toml`).

use serde::{Deserialize, Serialize};

/// Settings for the OpenAI-compatible chat endpoint.
///
/// The API key is **never** part of the config: it is read from the environment variable named by
/// [`api_key_env`](Self::api_key_env), so the config file can be shared and committed.
#[derive(Deserialize, Serialize, Clone, PartialEq)]
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
    /// Let the model occasionally choose a bot's next goal (the "director"). Needs `enabled`; off
    /// by default. The playerbots runtime reads it, the client itself does not.
    pub director: bool,
    /// Average seconds between two director requests of one bot (each bot jitters around it).
    pub director_interval_secs: u64,
    /// Steps the model may plan ahead in one director answer (1..=5; the brain starts the next
    /// one when the running goal ends, re-checking that it is still allowed).
    pub director_plan_steps: u8,
    /// Ask the director sooner than the interval when something happened: a level-up, a death, a
    /// failed goal, a zone the bot has never been in, a plan that runs out.
    pub director_triggers: bool,
    /// The shortest time between two director requests of one bot, in seconds (also for the
    /// triggers; at least 30).
    pub director_min_gap_secs: u64,
    /// Let the director's answer carry a short note the bot keeps in its journal (shown to the
    /// model again as the bot's own memories, quoted as data).
    pub director_notes: bool,
}

impl LlmConfig {
    /// Steps per director plan, clamped to 1..=5.
    pub fn plan_steps(&self) -> usize {
        usize::from(self.director_plan_steps.clamp(1, 5))
    }

    /// The shortest gap between two director requests of one bot, in milliseconds (at least
    /// 30 s).
    pub fn min_gap_ms(&self) -> u64 {
        self.director_min_gap_secs.max(30).saturating_mul(1000)
    }
}

impl std::fmt::Debug for LlmConfig {
    // Hand-written: a base URL may carry a user name, a password or a key; only its host is shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmConfig")
            .field("enabled", &self.enabled)
            .field("base_url_host", &crate::client::url_host(&self.base_url))
            .field("model", &self.model)
            .field("api_key_env", &self.api_key_env)
            .field("timeout_secs", &self.timeout_secs)
            .field("max_concurrent", &self.max_concurrent)
            .field("max_requests_per_minute", &self.max_requests_per_minute)
            .field("daily_token_budget", &self.daily_token_budget)
            .field("temperature", &self.temperature)
            .field("max_reply_tokens", &self.max_reply_tokens)
            .field("director", &self.director)
            .field("director_interval_secs", &self.director_interval_secs)
            .field("director_plan_steps", &self.director_plan_steps)
            .field("director_triggers", &self.director_triggers)
            .field("director_min_gap_secs", &self.director_min_gap_secs)
            .field("director_notes", &self.director_notes)
            .finish()
    }
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
            director: false,
            director_interval_secs: 600,
            director_plan_steps: 3,
            director_triggers: true,
            director_min_gap_secs: 120,
            director_notes: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_shows_the_host_of_the_url_and_nothing_secret() {
        let cfg = LlmConfig {
            base_url: "https://user:p4ss@api.example.com/v1/sk-secret-key".into(),
            ..LlmConfig::default()
        };
        let shown = format!("{cfg:?}");
        assert!(shown.contains("api.example.com"), "{shown}");
        for secret in ["p4ss", "user", "sk-secret-key", "/v1"] {
            assert!(!shown.contains(secret), "{secret} in {shown}");
        }
    }

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
        assert!(!cfg.director);
        assert_eq!(cfg.director_interval_secs, 600);
        assert_eq!(cfg.director_plan_steps, 3);
        assert!(cfg.director_triggers && cfg.director_notes);
        assert_eq!(cfg.director_min_gap_secs, 120);
    }

    #[test]
    fn the_plan_length_and_the_gap_are_clamped_where_they_are_used() {
        let cfg = |steps: u8, gap: u64| LlmConfig {
            director_plan_steps: steps,
            director_min_gap_secs: gap,
            ..LlmConfig::default()
        };
        assert_eq!(cfg(0, 0).plan_steps(), 1);
        assert_eq!(cfg(3, 0).plan_steps(), 3);
        assert_eq!(cfg(200, 0).plan_steps(), 5);
        assert_eq!(cfg(1, 0).min_gap_ms(), 30_000);
        assert_eq!(cfg(1, 120).min_gap_ms(), 120_000);
        let parsed: LlmConfig = serde_json::from_str(
            r#"{"director_plan_steps": 2, "director_triggers": false, "director_min_gap_secs": 300, "director_notes": false}"#,
        )
        .unwrap();
        assert_eq!(parsed.plan_steps(), 2);
        assert!(!parsed.director_triggers && !parsed.director_notes);
        assert_eq!(parsed.min_gap_ms(), 300_000);
    }

    #[test]
    fn director_keys_are_optional_and_parse() {
        let cfg: LlmConfig =
            serde_json::from_str(r#"{"director": true, "director_interval_secs": 90}"#).unwrap();
        assert!(cfg.director);
        assert_eq!(cfg.director_interval_secs, 90);
        assert!(!cfg.enabled);
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
