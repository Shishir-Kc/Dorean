//! OpenRouter adapter: free-model discovery and streaming chat.
//!
//! Base URL defaults to `https://openrouter.ai/api/v1`. The API key is taken
//! from config, `DOREAN_OPENROUTER_API_KEY`, or the ecosystem-standard
//! `OPENROUTER_API_KEY` env var.

use std::pin::Pin;
use std::time::Duration;

use futures_util::stream::Stream;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;

use crate::config::Config;
use crate::error::DoreanError;

use super::client::{ChatRequest, CompletionEvent, ModelInfo, stream_chat};

pub const OPENROUTER_DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
pub const DEFAULT_FREE_MODEL: &str = "openrouter/free";
/// Ceiling for a single `GET /models` call so the model selector can never
/// hang on a stuck connection. Streaming chat is unaffected (connect-only cap).
const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(30);
/// Cap on establishing any TCP connection (applies to chat streaming too).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Provider for OpenRouter's OpenAI-compatible API.
#[derive(Clone)]
pub struct OpenRouterProvider {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    referer: Option<String>,
    title: Option<String>,
    list_timeout: Duration,
}

impl OpenRouterProvider {
    /// Build a provider from config. Requires an API key.
    pub fn from_config(config: &Config) -> Result<Self, DoreanError> {
        let api_key = config
            .openrouter_api_key
            .clone()
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .ok_or_else(|| {
                DoreanError::Config(
                    "OpenRouter requires an API key. Set DOREAN_OPENROUTER_API_KEY, \
                     OPENROUTER_API_KEY, or `openrouter_api_key` in ~/.dorean/config.json"
                        .to_string(),
                )
            })?;

        Ok(OpenRouterProvider {
            http: Self::http_client(),
            api_key,
            base_url: config
                .base_url
                .clone()
                .unwrap_or_else(|| OPENROUTER_DEFAULT_BASE_URL.to_string()),
            referer: std::env::var("DOREAN_REFERER").ok(),
            title: std::env::var("DOREAN_TITLE")
                .ok()
                .or_else(|| Some("dorean".to_string())),
            list_timeout: LIST_MODELS_TIMEOUT,
        })
    }

    /// Build a provider for model *listing* only. OpenRouter's `GET /models`
    /// endpoint is public, so this never errors on a missing key — the key is
    /// attached when present. Actual chat still requires [`Self::from_config`].
    pub fn for_model_listing(config: &Config) -> Self {
        let api_key = config
            .openrouter_api_key
            .clone()
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .unwrap_or_default();
        OpenRouterProvider {
            http: Self::http_client(),
            api_key,
            base_url: config
                .base_url
                .clone()
                .unwrap_or_else(|| OPENROUTER_DEFAULT_BASE_URL.to_string()),
            referer: std::env::var("DOREAN_REFERER").ok(),
            title: std::env::var("DOREAN_TITLE")
                .ok()
                .or_else(|| Some("dorean".to_string())),
            list_timeout: LIST_MODELS_TIMEOUT,
        }
    }

    fn http_client() -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .pool_max_idle_per_host(8)
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build http client")
    }

    /// Build a provider pointing at a custom base URL (used in tests).
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        OpenRouterProvider {
            http: Self::http_client(),
            api_key: api_key.into(),
            base_url: base_url.into(),
            referer: None,
            title: Some("dorean".to_string()),
            list_timeout: LIST_MODELS_TIMEOUT,
        }
    }

    /// Like [`Self::with_base_url`] but with a short model-list timeout, for
    /// tests that assert a hung `/models` request fails fast.
    pub fn with_base_url_and_list_timeout(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        list_timeout: Duration,
    ) -> Self {
        OpenRouterProvider {
            http: Self::http_client(),
            api_key: api_key.into(),
            base_url: base_url.into(),
            referer: None,
            title: Some("dorean".to_string()),
            list_timeout,
        }
    }

    pub fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![
            (
                AUTHORIZATION.as_str().to_string(),
                format!("Bearer {}", self.api_key),
            ),
            (
                CONTENT_TYPE.as_str().to_string(),
                "application/json".to_string(),
            ),
        ];
        if let Some(referer) = &self.referer {
            headers.push(("HTTP-Referer".to_string(), referer.clone()));
        }
        if let Some(title) = &self.title {
            headers.push(("X-OpenRouter-Title".to_string(), title.clone()));
        }
        headers
    }

    /// Fetch the full model list from `GET /models`. The endpoint is public, so
    /// the auth header is only attached when a key is present.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, DoreanError> {
        let url = format!("{}/models", self.base_url);
        let mut request = self.http.get(&url);
        if !self.api_key.is_empty() {
            request = request.header(AUTHORIZATION, format!("Bearer {}", self.api_key));
        }
        let response = request.timeout(self.list_timeout).send().await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(DoreanError::Provider {
                status: status.as_u16(),
                message: body,
            });
        }
        let parsed: RawModelsResponse =
            serde_json::from_str(&body).map_err(|e| DoreanError::Provider {
                status: status.as_u16(),
                message: format!("invalid /models response: {e}"),
            })?;
        Ok(parsed.data.into_iter().map(ModelInfo::from).collect())
    }

    /// Model list restricted to free models, sorted by id.
    pub async fn list_free_models(&self) -> Result<Vec<ModelInfo>, DoreanError> {
        let mut models = self.list_models().await?;
        models.retain(|m| m.is_free);
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    /// Stream a chat completion.
    pub fn stream_chat(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Stream<Item = Result<CompletionEvent, DoreanError>> + Send>> {
        let url = format!("{}/chat/completions", self.base_url);
        stream_chat(self.http.clone(), url, self.headers(), request.to_json())
    }
}

#[derive(Debug, Deserialize)]
struct RawModelsResponse {
    data: Vec<RawModel>,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    pricing: RawPricing,
}

#[derive(Debug, Default, Deserialize)]
struct RawPricing {
    #[serde(default, deserialize_with = "deserialize_price")]
    prompt: f64,
    #[serde(default, deserialize_with = "deserialize_price")]
    completion: f64,
    #[serde(default, deserialize_with = "deserialize_price")]
    request: f64,
}

/// OpenRouter reports prices as strings ("0", "0.0000001") but also accepts
/// numbers, and emits explicit `null` for prices it no longer quotes (e.g.
/// `"request"`). Tolerate all three.
fn deserialize_price<'de, D>(de: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Price {
        Number(f64),
        Text(String),
        Null,
    }

    match Price::deserialize(de)? {
        Price::Number(n) => Ok(n),
        Price::Text(s) => s
            .trim()
            .parse()
            .map_err(|_| D::Error::custom("invalid price")),
        Price::Null => Ok(0.0),
    }
}

impl From<RawModel> for ModelInfo {
    fn from(raw: RawModel) -> Self {
        let is_free = raw.id.ends_with(":free")
            || raw.id == "openrouter/free"
            || (raw.pricing.prompt == 0.0
                && raw.pricing.completion == 0.0
                && raw.pricing.request == 0.0);
        ModelInfo {
            id: raw.id,
            name: raw.name,
            description: raw.description,
            context_length: raw.context_length,
            is_free,
            prompt_price: raw.pricing.prompt,
            completion_price: raw.pricing.completion,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            },
            {
                "id": "deepseek/deepseek-r1:free",
                "name": "DeepSeek R1 (free)",
                "context_length": 163840,
                "pricing": { "prompt": 0, "completion": 0, "request": 0 }
            }
        ]
    }"#;

    #[test]
    fn parses_model_list_and_marks_free() {
        let parsed: RawModelsResponse = serde_json::from_str(MODELS_FIXTURE).unwrap();
        let models: Vec<ModelInfo> = parsed.data.into_iter().map(ModelInfo::from).collect();
        assert_eq!(models.len(), 3);

        let gpt = &models[0];
        assert!(!gpt.is_free);
        assert_eq!(gpt.prompt_price, 0.0000025);

        let llama = &models[1];
        assert!(llama.is_free);
        assert_eq!(llama.context_length, Some(131072));
        assert_eq!(llama.name, "Llama 3.3 70B Instruct (free)");

        let deepseek = &models[2];
        assert!(deepseek.is_free);
    }

    #[test]
    fn marks_openrouter_free_router_as_free() {
        let raw = RawModel {
            id: "openrouter/free".to_string(),
            name: String::new(),
            description: String::new(),
            context_length: None,
            pricing: RawPricing::default(),
        };
        let info = ModelInfo::from(raw);
        assert!(info.is_free);
    }

    #[test]
    fn requires_api_key() {
        let config = Config {
            openrouter_api_key: None,
            ..Config::default()
        };
        assert!(OpenRouterProvider::from_config(&config).is_err());
    }

    #[test]
    fn tolerates_null_prices_like_the_live_api() {
        let raw = RawModel {
            id: "meta-llama/llama-3.3-70b-instruct:free".to_string(),
            name: String::new(),
            description: String::new(),
            context_length: Some(131072),
            pricing: RawPricing {
                prompt: 0.0,
                completion: 0.0,
                request: 0.0,
            },
        };
        let info = ModelInfo::from(raw);
        assert!(info.is_free);

        let body = r#"{
            "data": [{
                "id": "openai/gpt-4o",
                "name": "GPT-4o",
                "pricing": {
                    "prompt": "0.0000025",
                    "completion": "0.00001",
                    "request": null,
                    "input_cache_read": null,
                    "input_cache_write": null
                }
            }]
        }"#;
        let parsed: RawModelsResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.data.len(), 1);
        let info = ModelInfo::from(parsed.data.into_iter().next().unwrap());
        assert!(!info.is_free);
        assert_eq!(info.prompt_price, 0.0000025);
    }

    #[test]
    fn for_model_listing_never_requires_a_key() {
        let config = Config {
            openrouter_api_key: None,
            ..Config::default()
        };
        let provider = OpenRouterProvider::for_model_listing(&config);
        assert!(!provider.is_available());
        assert_eq!(provider.base_url, OPENROUTER_DEFAULT_BASE_URL.to_string());

        let config = Config {
            openrouter_api_key: Some("sk-test".to_string()),
            ..Config::default()
        };
        let provider = OpenRouterProvider::for_model_listing(&config);
        assert!(provider.is_available());
    }
}
