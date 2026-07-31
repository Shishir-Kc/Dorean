//! End-to-end provider tests against a mock NVIDIA NIM server.

use futures_util::StreamExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use dorean::agent::{AbortHandle, RunOptions};
use dorean::config::{Config, Provider};
use dorean::error::DoreanError;
use dorean::providers::client::{ChatRequest, CompletionEvent, Message};
use dorean::providers::{DEFAULT_NVIDIA_MODEL, NvidiaProvider};

/// The live `GET /v1/models` shape: an OpenAI-standard list with no pricing.
const MODELS_FIXTURE: &str = r#"{
    "object": "list",
    "data": [
        { "id": "nvidia/nemotron-3-super-120b-a12b", "object": "model", "created": 735790403, "owned_by": "nvidia" },
        { "id": "meta/llama-3.3-70b-instruct", "object": "model", "created": 735790403, "owned_by": "meta" }
    ]
}"#;

const STREAM_BODY: &str = concat!(
    "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{\"content\":\"Hello\",\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n\n",
    "data: [DONE]\n\n",
);

fn provider(server: &MockServer) -> NvidiaProvider {
    NvidiaProvider::with_base_url("nvapi-test", server.uri())
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
async fn lists_models_and_marks_all_free() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("Authorization", "Bearer nvapi-test"))
        .respond_with(ResponseTemplate::new(200).set_body_string(MODELS_FIXTURE))
        .mount(&server)
        .await;

    let models = provider(&server).list_models().await.unwrap();
    assert_eq!(models.len(), 2);
    assert!(models.iter().all(|m| m.is_free));
    assert_eq!(models[0].id, "nvidia/nemotron-3-super-120b-a12b");
    assert_eq!(models[0].description, "nvidia");
}

#[tokio::test]
async fn list_free_models_sorts_by_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("Authorization", "Bearer nvapi-test"))
        .respond_with(ResponseTemplate::new(200).set_body_string(MODELS_FIXTURE))
        .mount(&server)
        .await;

    let free = provider(&server).list_free_models().await.unwrap();
    assert_eq!(free.len(), 2);
    assert_eq!(free[0].id, "meta/llama-3.3-70b-instruct");
    assert_eq!(free[1].id, "nvidia/nemotron-3-super-120b-a12b");
}

#[tokio::test]
async fn model_listing_works_without_an_api_key() {
    let server = MockServer::start().await;
    // No Authorization matcher: listing is attempted keyless.
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(MODELS_FIXTURE))
        .mount(&server)
        .await;

    let config = Config {
        provider: Provider::Nvidia,
        base_url: Some(server.uri()),
        nvidia_api_key: None,
        ..Config::default()
    };
    let provider = dorean::providers::NvidiaProvider::for_model_listing(&config);
    assert!(!provider.is_available());
    let free = provider.list_free_models().await.unwrap();
    assert_eq!(free.len(), 2);
}

#[tokio::test]
async fn stream_chat_yields_deltas_usage_and_done() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("Authorization", "Bearer nvapi-test"))
        .respond_with(ResponseTemplate::new(200).set_body_string(STREAM_BODY))
        .mount(&server)
        .await;

    let request = ChatRequest {
        model: DEFAULT_NVIDIA_MODEL.to_string(),
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
        model: DEFAULT_NVIDIA_MODEL.to_string(),
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
async fn agent_run_once_streams_single_turn() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(STREAM_BODY))
        .mount(&server)
        .await;

    let config = Config {
        provider: Provider::Nvidia,
        base_url: Some(server.uri()),
        nvidia_api_key: Some("nvapi-test".to_string()),
        ..Config::default()
    };

    let dir = std::env::temp_dir().join(format!("dorean-nvidia-agent-{}", std::process::id()));
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
        provider: Provider::Nvidia,
        nvidia_api_key: None,
        ..Config::default()
    };
    let dir = std::env::temp_dir().join(format!("dorean-nvidia-key-{}", std::process::id()));
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
