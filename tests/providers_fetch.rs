//! Aggregation tests for `fetch_all_models`: every provider's catalog lands
//! in one tagged list (free first), failures degrade to hints, and a total
//! outage names every provider instead of showing an empty picker.

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use dorean::config::{Config, Provider};
use dorean::providers::fetch_all_models;

/// One body that parses under every provider's `GET /models` shape
/// (OpenRouter pricing, NVIDIA owned_by, local/generic bare ids).
const COMBINED_MODELS: &str = r#"{
    "object": "list",
    "total_count": 2,
    "data": [
        {
            "id": "meta-llama/llama-3.3-70b-instruct:free",
            "object": "model",
            "owned_by": "meta",
            "name": "Llama 3.3 70B (free)",
            "description": "free",
            "context_length": 131072,
            "pricing": { "prompt": "0", "completion": "0" }
        },
        {
            "id": "some-paid-model",
            "object": "model",
            "owned_by": "acme",
            "name": "Paid",
            "description": "paid",
            "context_length": 32000,
            "pricing": { "prompt": "0.0000025", "completion": "0.00001" }
        }
    ]
}"#;

fn keyed_config(base_url: String) -> Config {
    Config {
        provider: Provider::OpenRouter,
        base_url: Some(base_url),
        openrouter_api_key: Some("sk-test".to_string()),
        nvidia_api_key: Some("nvapi-test".to_string()),
        deepseek_api_key: Some("ds-test".to_string()),
        generic_api_key: Some("g-test".to_string()),
        ..Config::default()
    }
}

#[tokio::test]
async fn aggregates_all_providers_free_first() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(COMBINED_MODELS))
        .mount(&server)
        .await;

    let fetched = fetch_all_models(&keyed_config(server.uri())).await;
    assert!(fetched.problems.is_empty(), "{:?}", fetched.problems);
    // 5 providers × 2 models each.
    assert_eq!(fetched.tagged.len(), 10);
    // Free rows sort before paid rows.
    let first_paid = fetched.tagged.iter().position(|(_, m)| !m.is_free).unwrap();
    assert!(fetched.tagged[..first_paid].iter().all(|(_, m)| m.is_free));
    // The OpenRouter free model made it through with its provider tag.
    assert!(fetched.tagged.iter().any(
        |(p, m)| *p == Provider::OpenRouter && m.id == "meta-llama/llama-3.3-70b-instruct:free"
    ));
}

#[tokio::test]
async fn total_outage_names_every_provider() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let fetched = fetch_all_models(&keyed_config(server.uri())).await;
    assert!(fetched.tagged.is_empty());
    assert_eq!(fetched.problems.len(), 5);
    for id in ["openrouter", "nvidia", "deepseek", "local", "generic"] {
        assert!(
            fetched.problems.iter().any(|p| p.starts_with(id)),
            "missing {id} in {:?}",
            fetched.problems
        );
    }
}
