//! End-to-end tests of `Llm` against the local mock endpoint. No external host is ever contacted.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mock::{completion, MockServer, Reply};
use crate::{ChatRequest, Llm, LlmConfig, LlmError, Msg};

const KEY: &str = "sk-test-secret-key-123";

fn cfg(mock: &MockServer) -> LlmConfig {
    LlmConfig {
        enabled: true,
        base_url: mock.base_url(),
        // generous defaults so a test only trips the limit it is about
        max_requests_per_minute: 1000,
        daily_token_budget: 1_000_000,
        ..LlmConfig::default()
    }
}

fn llm(mock: &MockServer) -> Arc<Llm> {
    Llm::with_key(&cfg(mock), KEY.to_string())
}

async fn mock_ok(content: &str) -> MockServer {
    MockServer::start(vec![Reply::ok(completion(content, 40, 12))]).await
}

fn options(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("option {i}")).collect()
}

// ---- chat ------------------------------------------------------------------------------------

#[tokio::test]
async fn a_huge_response_is_refused_without_being_read_to_the_end() {
    use crate::client::MAX_RESPONSE_BYTES;
    // a valid-looking completion whose content alone is over the cap
    let big = "x".repeat(MAX_RESPONSE_BYTES + 1024);
    let mock = MockServer::start(vec![Reply::ok(completion(&big, 5, 5))]).await;
    let llm1 = llm(&mock);
    let err = llm1
        .chat(ChatRequest::simple("be brief", "hi"))
        .await
        .expect_err("over the cap");
    match &err {
        LlmError::BadResponse(m) => assert!(m.contains("over"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(llm1.usage().failures, 1);
    // nothing was charged for an answer that was thrown away
    assert_eq!(llm1.usage().tokens_today, 0);

    // the same without a Content-Length: the cap is enforced while reading
    let mock = MockServer::start(vec![Reply::ok(completion(&big, 5, 5)).chunked()]).await;
    let llm2 = llm(&mock);
    let err = llm2
        .chat(ChatRequest::simple("be brief", "hi"))
        .await
        .expect_err("over the cap");
    assert!(matches!(err, LlmError::BadResponse(_)), "{err:?}");

    // one just under the cap still works, with or without a Content-Length
    let fits = "y".repeat(MAX_RESPONSE_BYTES - 4096);
    for reply in [
        Reply::ok(completion(&fits, 5, 5)),
        Reply::ok(completion(&fits, 5, 5)).chunked(),
    ] {
        let mock = MockServer::start(vec![reply]).await;
        let llm3 = llm(&mock);
        let ok = llm3
            .chat(ChatRequest::simple("be brief", "hi"))
            .await
            .expect("under the cap");
        assert_eq!(ok.text.len(), fits.len());
    }
}

#[tokio::test]
async fn chat_parses_the_reply_and_accounts_usage() {
    let mock = mock_ok("hey there, wolves quest anyone?").await;
    let llm = llm(&mock);

    let reply = llm
        .chat(ChatRequest {
            system: "You are Aria.".into(),
            messages: vec![
                Msg::user("hi"),
                Msg::assistant("hello"),
                Msg::user("what are you doing?"),
            ],
            max_tokens: Some(33),
            json: false,
        })
        .await
        .unwrap();
    assert_eq!(reply.text, "hey there, wolves quest anyone?");
    assert_eq!(reply.prompt_tokens, 40);
    assert_eq!(reply.completion_tokens, 12);

    let usage = llm.usage();
    assert_eq!(usage.requests, 1);
    assert_eq!(usage.failures, 0);
    assert_eq!(usage.rate_limited, 0);
    assert_eq!(usage.prompt_tokens, 40);
    assert_eq!(usage.completion_tokens, 12);
    assert_eq!(usage.tokens_today, 52);
    assert_eq!(usage.budget_left, 1_000_000 - 52);

    // what went over the wire
    assert_eq!(mock.hits(), 1);
    let sent = mock.request(0);
    assert_eq!(sent.request_line, "POST /chat/completions HTTP/1.1");
    assert_eq!(
        sent.header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    let body = sent.json();
    assert_eq!(body["model"], "deepseek-chat");
    assert_eq!(body["max_tokens"], 33);
    assert_eq!(body["stream"], false);
    assert!((body["temperature"].as_f64().unwrap() - 0.8).abs() < 1e-6);
    assert!(body.get("response_format").is_none());
    let roles: Vec<&str> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["system", "user", "assistant", "user"]);
    assert_eq!(body["messages"][0]["content"], "You are Aria.");
    assert_eq!(body["messages"][3]["content"], "what are you doing?");
}

#[tokio::test]
async fn chat_uses_the_configured_default_max_tokens_and_skips_an_empty_system_prompt() {
    let mock = mock_ok("ok").await;
    let mut c = cfg(&mock);
    c.max_reply_tokens = 55;
    c.model = "tiny".into();
    let llm = Llm::with_key(&c, KEY.into());
    llm.chat(ChatRequest::simple("", "hello")).await.unwrap();
    let body = mock.request(0).json();
    assert_eq!(body["max_tokens"], 55);
    assert_eq!(body["model"], "tiny");
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["role"], "user");
}

#[tokio::test]
async fn base_url_with_a_trailing_slash_or_full_path_works() {
    for suffix in ["/", "/chat/completions"] {
        let mock = mock_ok("ok").await;
        let mut c = cfg(&mock);
        c.base_url = format!("{}{suffix}", mock.base_url());
        Llm::with_key(&c, KEY.into())
            .chat(ChatRequest::simple("s", "u"))
            .await
            .unwrap();
        assert_eq!(
            mock.request(0).request_line,
            "POST /chat/completions HTTP/1.1"
        );
    }
}

#[tokio::test]
async fn json_mode_sets_response_format_and_the_json_hint() {
    let mock = mock_ok("{}").await;
    let llm = llm(&mock);
    llm.chat(ChatRequest {
        system: "be brief".into(),
        messages: vec![Msg::user("go")],
        max_tokens: None,
        json: true,
    })
    .await
    .unwrap();
    let body = mock.request(0).json();
    assert_eq!(body["response_format"]["type"], "json_object");
    assert!(body["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("JSON"));
    // json requests are capped to a lower temperature
    assert!(body["temperature"].as_f64().unwrap() <= 0.5 + 1e-6);
}

#[tokio::test]
async fn missing_usage_is_estimated_and_still_charged() {
    let body = serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content": "a".repeat(40)}}]
    })
    .to_string();
    let mock = MockServer::start(vec![Reply::ok(body)]).await;
    let llm = llm(&mock);
    let reply = llm
        .chat(ChatRequest::simple("x".repeat(20), "y".repeat(20)))
        .await
        .unwrap();
    assert_eq!(reply.prompt_tokens, 10); // 40 chars / 4
    assert_eq!(reply.completion_tokens, 10);
    assert_eq!(llm.usage().tokens_today, 20);
}

#[tokio::test]
async fn unusable_bodies_are_bad_responses() {
    let bodies = [
        "not json at all".to_string(),
        r#"{"choices": []}"#.to_string(),
        r#"{"choices": [{"message": {"content": null}}]}"#.to_string(),
        r#"{"choices": [{"message": {"content": "   "}}]}"#.to_string(),
        r#"{"error": {"message": "weird"}}"#.to_string(),
    ];
    for body in bodies {
        let mock = MockServer::start(vec![Reply::ok(body.clone())]).await;
        let llm = llm(&mock);
        let err = llm.chat(ChatRequest::simple("s", "u")).await.unwrap_err();
        assert!(matches!(err, LlmError::BadResponse(_)), "{body}: {err:?}");
        assert_eq!(llm.usage().failures, 1, "{body}");
    }
}

#[tokio::test]
async fn http_errors_carry_the_status() {
    let mock = MockServer::start(vec![Reply::status(500, "{}")]).await;
    let llm = llm(&mock);
    let err = llm.chat(ChatRequest::simple("s", "u")).await.unwrap_err();
    assert_eq!(err, LlmError::Http(500));
    let usage = llm.usage();
    assert_eq!((usage.requests, usage.failures), (1, 1));

    let mock = MockServer::start(vec![Reply::status(401, "{}")]).await;
    let err = self::llm(&mock)
        .chat(ChatRequest::simple("s", "u"))
        .await
        .unwrap_err();
    assert_eq!(err, LlmError::Http(401));
}

#[tokio::test]
async fn connection_refused_is_a_transport_error() {
    // grab a free port and close it again
    let port = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let c = LlmConfig {
        enabled: true,
        base_url: format!("http://127.0.0.1:{port}"),
        ..LlmConfig::default()
    };
    let llm = Llm::with_key(&c, KEY.into());
    let err = llm.chat(ChatRequest::simple("s", "u")).await.unwrap_err();
    assert!(matches!(err, LlmError::Transport(_)), "{err:?}");
    assert!(!err.to_string().contains(KEY));
    assert_eq!(llm.usage().failures, 1);
}

// ---- choose ----------------------------------------------------------------------------------

#[tokio::test]
async fn choose_parses_code_fenced_json() {
    let mock = mock_ok("```json\n{\"choice\": 2, \"reason\": \"closest quest\"}\n```").await;
    let llm = llm(&mock);
    let choice = llm
        .choose("You are Aria.", "hp 90%, idle", &options(3))
        .await
        .unwrap();
    assert_eq!(choice.index, 2);
    assert_eq!(choice.reason, "closest quest");

    let body = mock.request(0).json();
    assert_eq!(body["response_format"]["type"], "json_object");
    let system = body["messages"][0]["content"].as_str().unwrap();
    assert!(system.starts_with("You are Aria."));
    let user = body["messages"][1]["content"].as_str().unwrap();
    assert!(user.contains("hp 90%, idle"));
    assert!(user.contains("0. option 0"));
    assert!(user.contains("2. option 2"));
    assert!(body["max_tokens"].as_u64().unwrap() >= 60);
}

#[tokio::test]
async fn choose_tolerates_prose_around_the_json() {
    let mock = mock_ok("Sure! {\"choice\": \"1\", \"reason\": \"fun\"} cheers").await;
    let choice = llm(&mock).choose("", "idle", &options(2)).await.unwrap();
    assert_eq!(choice.index, 1);
    assert_eq!(choice.reason, "fun");
}

#[tokio::test]
async fn choose_rejects_an_invalid_index() {
    for answer in [
        r#"{"choice": 3, "reason": "x"}"#,
        r#"{"choice": -1}"#,
        r#"{"choice": 99}"#,
        "I pick the second one",
    ] {
        let mock = mock_ok(answer).await;
        let llm = llm(&mock);
        let err = llm.choose("", "idle", &options(3)).await.unwrap_err();
        assert!(matches!(err, LlmError::BadResponse(_)), "{answer}: {err:?}");
        // the tokens were spent and the failure is visible
        let usage = llm.usage();
        assert_eq!(usage.tokens_today, 52, "{answer}");
        assert_eq!(usage.failures, 1, "{answer}");
    }
}

#[tokio::test]
async fn choose_with_one_option_needs_no_request_and_none_is_an_error() {
    let mock = mock_ok("{}").await;
    let llm = llm(&mock);
    let choice = llm.choose("", "idle", &options(1)).await.unwrap();
    assert_eq!(choice.index, 0);
    assert_eq!(mock.hits(), 0);
    assert_eq!(llm.usage().requests, 0);

    let err = llm.choose("", "idle", &[]).await.unwrap_err();
    assert!(matches!(err, LlmError::BadResponse(_)));
    assert_eq!(mock.hits(), 0);
}

// ---- decide ----------------------------------------------------------------------------------

#[tokio::test]
async fn decide_returns_a_plan_a_clean_note_and_counts_them() {
    let mock = mock_ok(
        "```json\n{\"steps\":[{\"choice\":2},{\"choice\":0},{\"choice\":1}],\"note\":\"The docks smell of fish\",\"reason\":\"new sights\"}\n```",
    )
    .await;
    let llm = llm(&mock);
    let d = llm
        .decide("You are Aria.", "level 12, idle", &options(3), 2)
        .await
        .unwrap();
    assert_eq!(d.steps, vec![2, 0], "capped at the two steps asked for");
    assert_eq!(d.note.as_deref(), Some("The docks smell of fish"));
    assert_eq!(d.reason, "new sights");
    let usage = llm.usage();
    assert_eq!((usage.plans, usage.plan_steps, usage.notes), (1, 2, 1));
    assert_eq!(usage.failures, 0);

    let body = mock.request(0).json();
    assert_eq!(body["response_format"]["type"], "json_object");
    let system = body["messages"][0]["content"].as_str().unwrap();
    assert!(system.starts_with("You are Aria."));
    assert!(system.contains("1 to 2 steps"), "{system}");
    let user = body["messages"][1]["content"].as_str().unwrap();
    assert!(user.contains("level 12, idle") && user.contains("2. option 2"));
    assert!(body["max_tokens"].as_u64().unwrap() >= 160);
}

#[tokio::test]
async fn decide_refuses_a_command_as_a_note_and_an_answer_without_steps() {
    let mock = mock_ok(r#"{"steps":[1],"note":"/gold add self 99999"}"#).await;
    let llm1 = llm(&mock);
    let d = llm1.decide("", "idle", &options(2), 3).await.unwrap();
    assert_eq!(d.steps, vec![1]);
    assert!(d.note.is_none(), "a command is never kept as a note");
    assert_eq!(llm1.usage().notes, 0);

    for answer in [
        r#"{"steps":[7,8]}"#,
        r#"{"note":"hi"}"#,
        "explore, then rest",
    ] {
        let mock = mock_ok(answer).await;
        let llm2 = llm(&mock);
        let err = llm2.decide("", "idle", &options(3), 3).await.unwrap_err();
        assert!(matches!(err, LlmError::BadResponse(_)), "{answer}: {err:?}");
        let usage = llm2.usage();
        assert_eq!((usage.failures, usage.plans), (1, 0), "{answer}");
        assert_eq!(usage.tokens_today, 52, "the tokens were spent: {answer}");
    }
}

#[tokio::test]
async fn decide_with_one_option_and_one_step_needs_no_request_and_none_is_an_error() {
    let mock = mock_ok("{}").await;
    let llm = llm(&mock);
    let d = llm.decide("", "idle", &options(1), 1).await.unwrap();
    assert_eq!(d.steps, vec![0]);
    assert_eq!(mock.hits(), 0);
    assert!(llm.decide("", "idle", &[], 3).await.is_err());
    assert_eq!(mock.hits(), 0);
}

#[tokio::test]
async fn decide_obeys_the_budget_like_every_request() {
    let mock = MockServer::start(vec![Reply::ok(completion(r#"{"steps":[0]}"#, 100, 20))]).await;
    let mut c = cfg(&mock);
    c.daily_token_budget = 100;
    let llm = Llm::with_key(&c, KEY.into());
    assert!(llm.decide("", "idle", &options(2), 3).await.is_ok());
    let err = llm.decide("", "idle", &options(2), 3).await.unwrap_err();
    assert_eq!(err, LlmError::BudgetExhausted);
    assert_eq!(mock.hits(), 1);
}

// ---- limits ----------------------------------------------------------------------------------

#[tokio::test]
async fn http_429_is_rate_limited_and_pauses_all_requests() {
    let mock = MockServer::start(vec![
        Reply::status(429, "{}").header("Retry-After", "30"),
        Reply::ok(completion("ok", 1, 1)),
    ])
    .await;
    let llm = llm(&mock);

    let err = llm.chat(ChatRequest::simple("s", "u")).await.unwrap_err();
    assert_eq!(err, LlmError::RateLimited);
    assert_eq!(mock.hits(), 1);

    // the global cool-down refuses the next requests without touching the endpoint
    let started = Instant::now();
    for _ in 0..3 {
        let err = llm.chat(ChatRequest::simple("s", "u")).await.unwrap_err();
        assert_eq!(err, LlmError::RateLimited);
    }
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(mock.hits(), 1);

    let usage = llm.usage();
    assert_eq!(usage.requests, 1);
    assert_eq!(usage.failures, 0);
    assert_eq!(usage.rate_limited, 4);
}

#[tokio::test]
async fn timeout_is_reported_and_counted() {
    let mock = MockServer::start(vec![
        Reply::ok(completion("late", 1, 1)).delayed(Duration::from_secs(5))
    ])
    .await;
    let mut llm = Llm::try_build(&cfg(&mock), KEY.into()).unwrap();
    llm.timeout = Duration::from_millis(200);

    let started = Instant::now();
    let err = llm.chat(ChatRequest::simple("s", "u")).await.unwrap_err();
    assert_eq!(err, LlmError::Timeout);
    assert!(started.elapsed() < Duration::from_secs(3));
    let usage = llm.usage();
    assert_eq!((usage.requests, usage.failures), (1, 1));
    // nothing was charged for an answer that never came
    assert_eq!(usage.tokens_today, 0);
}

#[tokio::test]
async fn the_n_plus_one_th_request_within_a_minute_is_rate_limited() {
    let mock = mock_ok("ok").await;
    let mut c = cfg(&mock);
    c.max_requests_per_minute = 3;
    let llm = Llm::with_key(&c, KEY.into());

    for i in 0..3 {
        llm.chat(ChatRequest::simple("s", "u"))
            .await
            .unwrap_or_else(|e| panic!("request {i}: {e}"));
    }
    let started = Instant::now();
    let err = llm.chat(ChatRequest::simple("s", "u")).await.unwrap_err();
    assert_eq!(err, LlmError::RateLimited);
    assert!(started.elapsed() < Duration::from_millis(200), "no waiting");
    assert_eq!(mock.hits(), 3, "the refused request was never sent");

    let usage = llm.usage();
    assert_eq!(usage.requests, 3);
    assert_eq!(usage.rate_limited, 1);
}

#[tokio::test]
async fn the_concurrency_cap_refuses_instead_of_queueing() {
    let mock = MockServer::start(vec![
        Reply::ok(completion("slow", 1, 1)).delayed(Duration::from_millis(400))
    ])
    .await;
    let mut c = cfg(&mock);
    c.max_concurrent = 1;
    let mut llm = Llm::try_build(&c, KEY.into()).unwrap();
    llm.permit_wait = Duration::from_millis(30);

    let req = || ChatRequest::simple("s", "u");
    let (a, b) = tokio::join!(llm.chat(req()), llm.chat(req()));
    let results = [a, b];
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let limited = results
        .iter()
        .filter(|r| matches!(r, Err(LlmError::RateLimited)))
        .count();
    assert_eq!((ok, limited), (1, 1), "{results:?}");
    assert_eq!(mock.hits(), 1);

    // the slot is free again afterwards
    assert!(llm.chat(req()).await.is_ok());
}

#[tokio::test]
async fn concurrent_requests_up_to_the_cap_all_run() {
    let mock = MockServer::start(vec![
        Reply::ok(completion("ok", 1, 1)).delayed(Duration::from_millis(100))
    ])
    .await;
    let mut c = cfg(&mock);
    c.max_concurrent = 3;
    let llm = Llm::with_key(&c, KEY.into());
    let req = || ChatRequest::simple("s", "u");
    let (a, b, c) = tokio::join!(llm.chat(req()), llm.chat(req()), llm.chat(req()));
    assert!(a.is_ok() && b.is_ok() && c.is_ok());
    assert_eq!(mock.hits(), 3);
}

#[tokio::test]
async fn budget_exhaustion_stops_requests_before_they_are_sent() {
    // every answer costs 120 tokens against a budget of 100
    let mock = MockServer::start(vec![Reply::ok(completion("ok", 100, 20))]).await;
    let mut c = cfg(&mock);
    c.daily_token_budget = 100;
    let llm = Llm::with_key(&c, KEY.into());

    let usage = llm.usage();
    assert_eq!((usage.tokens_today, usage.budget_left), (0, 100));

    assert!(llm.chat(ChatRequest::simple("s", "u")).await.is_ok());
    let usage = llm.usage();
    assert_eq!(usage.tokens_today, 120);
    assert_eq!(usage.budget_left, 0);

    let err = llm.chat(ChatRequest::simple("s", "u")).await.unwrap_err();
    assert_eq!(err, LlmError::BudgetExhausted);
    let err = llm.choose("", "idle", &options(2)).await.unwrap_err();
    assert_eq!(err, LlmError::BudgetExhausted);
    assert_eq!(mock.hits(), 1, "nothing was sent after the budget ran out");
    assert_eq!(llm.usage().requests, 1);
}

#[tokio::test]
async fn a_zero_budget_and_zero_window_mean_unlimited() {
    let mock = mock_ok("ok").await;
    let mut c = cfg(&mock);
    c.daily_token_budget = 0;
    c.max_requests_per_minute = 0;
    let llm = Llm::with_key(&c, KEY.into());
    for _ in 0..5 {
        llm.chat(ChatRequest::simple("s", "u")).await.unwrap();
    }
    assert_eq!(llm.usage().budget_left, u64::MAX);
}

// ---- construction ----------------------------------------------------------------------------

#[test]
fn disabled_config_gives_none_even_with_a_key_in_the_environment() {
    let var = "PB_LLM_TEST_KEY_DISABLED";
    std::env::set_var(var, KEY);
    let c = LlmConfig {
        enabled: false,
        api_key_env: var.into(),
        ..LlmConfig::default()
    };
    assert!(Llm::from_config(&c).is_none());
    assert!(Llm::from_config(&LlmConfig::default()).is_none());
    std::env::remove_var(var);
}

#[test]
fn missing_or_empty_env_var_gives_none() {
    let c = LlmConfig {
        enabled: true,
        api_key_env: "PB_LLM_TEST_KEY_NEVER_SET".into(),
        ..LlmConfig::default()
    };
    assert!(Llm::from_config(&c).is_none());

    let var = "PB_LLM_TEST_KEY_EMPTY";
    std::env::set_var(var, "   ");
    let c = LlmConfig {
        enabled: true,
        api_key_env: var.into(),
        ..LlmConfig::default()
    };
    assert!(Llm::from_config(&c).is_none());
    std::env::remove_var(var);

    let c = LlmConfig {
        enabled: true,
        api_key_env: String::new(),
        ..LlmConfig::default()
    };
    assert!(Llm::from_config(&c).is_none());
}

#[test]
fn bad_base_url_gives_none() {
    let var = "PB_LLM_TEST_KEY_BADURL";
    std::env::set_var(var, KEY);
    let c = LlmConfig {
        enabled: true,
        api_key_env: var.into(),
        base_url: "api.deepseek.com".into(),
        ..LlmConfig::default()
    };
    assert!(Llm::from_config(&c).is_none());
    std::env::remove_var(var);
}

#[tokio::test]
async fn enabled_config_with_a_key_in_the_environment_gives_a_working_client() {
    let mock = mock_ok("hello").await;
    let var = "PB_LLM_TEST_KEY_PRESENT";
    std::env::set_var(var, format!("  {KEY}\n"));
    let c = LlmConfig {
        api_key_env: var.into(),
        ..cfg(&mock)
    };
    let llm = Llm::from_config(&c).expect("enabled with a key");
    std::env::remove_var(var);
    assert_eq!(
        llm.chat(ChatRequest::simple("s", "u")).await.unwrap().text,
        "hello"
    );
    // the key is trimmed before it goes into the header
    assert_eq!(
        mock.request(0).header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
}

#[tokio::test]
async fn the_key_never_shows_up_in_debug_output_or_error_text() {
    let mock = MockServer::start(vec![Reply::status(500, KEY)]).await;
    let llm = llm(&mock);
    assert!(!format!("{llm:?}").contains(KEY));
    let err = llm.chat(ChatRequest::simple("s", "u")).await.unwrap_err();
    assert!(!format!("{err:?} {err}").contains(KEY));
}
