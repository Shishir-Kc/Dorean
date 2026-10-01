//! End-to-end provider tests against a mock OpenRouter server.

use std::time::Duration;

use futures_util::StreamExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use dorean::agent::{AbortHandle, RunOptions};
use dorean::config::{Config, Provider};
use dorean::error::DoreanError;
use dorean::providers::client::{ChatRequest, CompletionEvent, Message};
use dorean::providers::{DEFAULT_FREE_MODEL, OpenRouterProvider};

const MODELS_FIXTURE: &str = r#"{
    "data": [
        {
            "id": "openai/gpt-4o",
            "name": "GPT-4o",
            "context_length": 128000,
            "pricing": { "prompt": "0.0000025", "completion": "0.00001", "request": "0" }
        },
        {
            "id": "meta-llama/llama-3.3-70b-instruct:free",
            "name": "Llama 3.3 70B Instruct (free)",
            "description": "A free model",
            "context_length": 131072,
            "pricing": { "prompt": "0", "completion": "0", "request": "0" }
        }
    ]
}"#;

const STREAM_BODY: &str = concat!(
    "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{\"content\":\"Hello\",\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n\n",
    "data: [DONE]\n\n",
);

async fn mount_models(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("Authorization", "Bearer sk-test"))
        .respond_with(ResponseTemplate::new(200).set_body_string(MODELS_FIXTURE))
        .mount(server)
        .await;
}

fn provider(server: &MockServer) -> OpenRouterProvider {
    OpenRouterProvider::with_base_url("sk-test", server.uri())
}

async fn collect_stream(
    stream: impl futures_util::Stream<Item = Result<CompletionEvent, DoreanError>> + Unpin,
) -> (Vec<CompletionEvent>, Vec<DoreanError>) {
    let mut events = Vec::new();
    let mut errors = Vec::new();
    let mut stream = stream;
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => events.push(event),
            Err(e) => errors.push(e),
        }
    }
    (events, errors)
}

#[tokio::test]
async fn lists_and_filters_free_models() {
    let server = MockServer::start().await;
    mount_models(&server).await;

    let free = provider(&server).list_free_models().await.unwrap();
    assert_eq!(free.len(), 1);
    assert_eq!(free[0].id, "meta-llama/llama-3.3-70b-instruct:free");
    assert!(free[0].is_free);
}

/// Mirrors the live API shape: `request` (and cache fields) come back `null`.
const MODELS_NULL_PRICING: &str = r#"{
    "data": [
        {
            "id": "openai/gpt-4o",
            "name": "GPT-4o",
            "context_length": 128000,
            "pricing": {
                "prompt": "0.0000025",
                "completion": "0.00001",
                "request": null,
                "input_cache_read": null,
                "input_cache_write": null
            }
        },
        {
            "id": "meta-llama/llama-3.3-70b-instruct:free",
            "name": "Llama 3.3 70B Instruct (free)",
            "context_length": 131072,
            "pricing": {
                "prompt": "0",
                "completion": "0",
                "request": null,
                "input_cache_read": null,
                "input_cache_write": null
            }
        }
    ]
}"#;

#[tokio::test]
async fn list_models_tolerates_null_pricing_like_the_live_api() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("Authorization", "Bearer sk-test"))
        .respond_with(ResponseTemplate::new(200).set_body_string(MODELS_NULL_PRICING))
        .mount(&server)
        .await;

    let free = provider(&server).list_free_models().await.unwrap();
    assert_eq!(free.len(), 1);
    assert_eq!(free[0].id, "meta-llama/llama-3.3-70b-instruct:free");
}

#[tokio::test]
async fn model_listing_works_without_an_api_key() {
    let server = MockServer::start().await;
    // No Authorization matcher: the endpoint is public.
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(MODELS_NULL_PRICING))
        .mount(&server)
        .await;

    let config = Config {
        provider: Provider::OpenRouter,
        base_url: Some(server.uri()),
        openrouter_api_key: None,
        ..Config::default()
    };
    let provider = dorean::providers::OpenRouterProvider::for_model_listing(&config);
    assert!(!provider.is_available());
    let free = provider.list_free_models().await.unwrap();
    assert_eq!(free.len(), 1);
}

#[tokio::test]
async fn list_models_fails_fast_instead_of_hanging() {
    let server = MockServer::start().await;
    // Accept the connection but stall the response body past our timeout.
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(MODELS_FIXTURE)
                .set_delay(Duration::from_secs(5)),
        )
        .mount(&server)
        .await;

    let provider = OpenRouterProvider::with_base_url_and_list_timeout(
        "sk-test",
        server.uri(),
        Duration::from_millis(200),
    );
    let result = tokio::time::timeout(Duration::from_secs(2), provider.list_free_models())
        .await
        .expect("list_free_models should return (an error) before the watchdog");
    assert!(result.is_err());
}

#[tokio::test]
async fn stream_chat_yields_deltas_usage_and_done() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("Authorization", "Bearer sk-test"))
        .respond_with(ResponseTemplate::new(200).set_body_string(STREAM_BODY))
        .mount(&server)
        .await;

    let request = ChatRequest {
        model: DEFAULT_FREE_MODEL.to_string(),
        messages: vec![Message::user("hi")],
        max_tokens: None,
        temperature: None,
        tools: Vec::new(),
    };
    let (events, errors) = collect_stream(provider(&server).stream_chat(request)).await;

    assert!(errors.is_empty());
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            CompletionEvent::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello world");

    assert!(events.iter().any(|e| matches!(e, CompletionEvent::Done)));
    let usage = events
        .iter()
        .find_map(|e| match e {
            CompletionEvent::Usage(u) => Some(u),
            _ => None,
        })
        .expect("usage event");
    assert_eq!(usage.total_tokens, 7);
}

#[tokio::test]
async fn stream_chat_reports_http_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_string(r#"{"error":{"message":"Invalid API key"}}"#),
        )
        .mount(&server)
        .await;

    let request = ChatRequest {
        model: DEFAULT_FREE_MODEL.to_string(),
        messages: vec![Message::user("hi")],
        max_tokens: None,
        temperature: None,
        tools: Vec::new(),
    };
    let (events, errors) = collect_stream(provider(&server).stream_chat(request)).await;

    assert!(events.is_empty());
    assert!(matches!(
        errors.as_slice(),
        [DoreanError::Provider { status: 401, .. }]
    ));
}

#[tokio::test]
async fn stream_chat_surfaces_mid_stream_error() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
        "data: {\"error\":{\"message\":\"upstream failed\"},\"choices\":[{\"delta\":{},\"finish_reason\":\"error\"}]}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;

    let request = ChatRequest {
        model: DEFAULT_FREE_MODEL.to_string(),
        messages: vec![Message::user("hi")],
        max_tokens: None,
        temperature: None,
        tools: Vec::new(),
    };
    let (events, errors) = collect_stream(provider(&server).stream_chat(request)).await;

    assert_eq!(events.len(), 1);
    assert!(matches!(
        errors.as_slice(),
        [DoreanError::Stream(msg)] if msg == "upstream failed"
    ));
}

#[tokio::test]
async fn agent_run_once_streams_single_turn() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(STREAM_BODY))
        .mount(&server)
        .await;

    let config = Config {
        provider: Provider::OpenRouter,
        base_url: Some(server.uri()),
        openrouter_api_key: Some("sk-test".to_string()),
        ..Config::default()
    };

    let dir = std::env::temp_dir().join(format!("dorean-agent-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let options = RunOptions {
        message: "hi",
        print: false,
        resume: false,
        cwd: dir.clone(),
        abort: AbortHandle::new(),
    };

    let summary = dorean::agent::run_once(&config, &options).await.unwrap();
    assert_eq!(summary.response, "Hello world");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn agent_requires_key() {
    let config = Config {
        provider: Provider::OpenRouter,
        openrouter_api_key: None,
        ..Config::default()
    };
    let dir = std::env::temp_dir().join(format!("dorean-agent-key-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let options = RunOptions {
        message: "hi",
        print: false,
        resume: false,
        cwd: dir.clone(),
        abort: AbortHandle::new(),
    };
    assert!(dorean::agent::run_once(&config, &options).await.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Live-catalog shape (2026): extra top-level keys, per-entry metadata,
/// `pricing.overrides` arrays, nulls, and extra pricing keys must not break
/// parsing or `:free` detection.
const MODELS_LIVE_SHAPE: &str = r#"{
    "data": [
        {
            "id": "inception/mercury-2.5",
            "canonical_slug": "inception/mercury-2.5",
            "name": "Mercury",
            "created": 1788890061,
            "description": "fast",
            "context_length": 262144,
            "architecture": { "modality": "text", "tokenizer": "GPT" },
            "pricing": { "prompt": "0.00001", "completion": "0.00005", "web_search": "0.01", "input_cache_read": "0.000001", "overrides": [] },
            "top_provider": { "context_length": 262144, "is_moderated": true },
            "per_request_limits": null,
            "supported_voices": null
        },
        {
            "id": "liquid/lfm-2.5-2.6b:free",
            "canonical_slug": "liquid/lfm-2.5-2.6b",
            "name": "Liquid: LFM (free)",
            "created": 1788890061,
            "description": "small free model",
            "context_length": 32768,
            "architecture": { "modality": "text" },
            "pricing": { "prompt": "0", "completion": "0" },
            "top_provider": { "context_length": 32768 },
            "per_request_limits": null
        },
        {
            "id": "openai/gpt-6-astra",
            "name": "GPT Astra",
            "description": "tiered pricing",
            "context_length": 400000,
            "pricing": { "prompt": "0.00001", "completion": "0.00005", "overrides": [{"min_prompt_tokens": 272000, "prompt": "0.00002", "completion": "0.000075"}] }
        }
    ],
    "total_count": 3,
    "links": { "next": null }
}"#;

#[tokio::test]
async fn parses_live_shaped_catalog_and_detects_free() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(MODELS_LIVE_SHAPE))
        .mount(&server)
        .await;

    let provider = OpenRouterProvider::with_base_url("sk-test", server.uri());
    let models = provider.list_models().await.unwrap();
    assert_eq!(models.len(), 3);
    let free: Vec<_> = models.iter().filter(|m| m.is_free).collect();
    assert_eq!(free.len(), 1);
    assert_eq!(free[0].id, "liquid/lfm-2.5-2.6b:free");
}
