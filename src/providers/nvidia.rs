//! NVIDIA adapter: hosted NIM APIs (build.nvidia.com).
//!
//! Base URL defaults to `https://integrate.api.nvidia.com/v1`. The API key is
//! taken from config, `DOREAN_NVIDIA_API_KEY`, or the ecosystem-standard
//! `NVIDIA_API_KEY` env var. The endpoint is OpenAI-compatible, so the shared
//! [`super::client`] core handles chat streaming; model listing is a plain
//! OpenAI-shaped `GET /v1/models` response.

use std::pin::Pin;
use std::time::Duration;

use futures_util::stream::Stream;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;

use crate::config::Config;
use crate::error::DoreanError;

use super::client::{ChatRequest, CompletionEvent, ModelInfo, stream_chat};

pub const NVIDIA_DEFAULT_BASE_URL: &str = "https://integrate.api.nvidia.com/v1";
/// Default NVIDIA model: Nemotron 3 Ultra, the ecosystem default for long
/// agentic work.
pub const DEFAULT_NVIDIA_MODEL: &str = "nvidia/nemotron-3-ultra-550b-a55b";
/// Ceiling for a single `GET /models` call so the model selector can never
/// hang on a stuck connection. Streaming chat is unaffected (connect-only cap).
const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(30);
/// Cap on establishing any TCP connection (applies to chat streaming too).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Provider for NVIDIA's OpenAI-compatible hosted NIM API.
#[derive(Clone)]
pub struct NvidiaProvider {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    list_timeout: Duration,
}

impl NvidiaProvider {
    /// Build a provider from config. Requires an API key.
    pub fn from_config(config: &Config) -> Result<Self, DoreanError> {
        let api_key = config
            .nvidia_api_key
            .clone()
            .or_else(|| std::env::var("NVIDIA_API_KEY").ok())
            .ok_or_else(|| {
                DoreanError::Config(
                    "NVIDIA requires an API key. Set DOREAN_NVIDIA_API_KEY, \
                     NVIDIA_API_KEY, or `nvidia_api_key` in ~/.dorean/config.json \
                     (get one at https://build.nvidia.com/settings/api-keys)"
                        .to_string(),
                )
            })?;

        Ok(NvidiaProvider {
            http: Self::http_client(),
            api_key,
            base_url: config
                .base_url
                .clone()
                .unwrap_or_else(|| NVIDIA_DEFAULT_BASE_URL.to_string()),
            list_timeout: LIST_MODELS_TIMEOUT,
        })
    }

    /// Build a provider for model *listing* only. The key is attached when
    /// present so the list works both pre- and post-auth; chat still requires
    /// [`Self::from_config`].
    pub fn for_model_listing(config: &Config) -> Self {
        let api_key = config
            .nvidia_api_key
            .clone()
            .or_else(|| std::env::var("NVIDIA_API_KEY").ok())
            .unwrap_or_default();
        NvidiaProvider {
            http: Self::http_client(),
            api_key,
            base_url: config
                .base_url
                .clone()
                .unwrap_or_else(|| NVIDIA_DEFAULT_BASE_URL.to_string()),
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
        NvidiaProvider {
            http: Self::http_client(),
            api_key: api_key.into(),
            base_url: base_url.into(),
            list_timeout: LIST_MODELS_TIMEOUT,
        }
    }

    pub fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            (
                AUTHORIZATION.as_str().to_string(),
                format!("Bearer {}", self.api_key),
            ),
            (
                CONTENT_TYPE.as_str().to_string(),
                "application/json".to_string(),
            ),
        ]
    }

    /// Fetch the full model list from `GET /models` (OpenAI standard shape).
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

    /// NVIDIA's hosted catalog is entirely free (rate-limited), so the model
    /// list *is* the free list, sorted by id.
    pub async fn list_free_models(&self) -> Result<Vec<ModelInfo>, DoreanError> {
        let mut models = self.list_models().await?;
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
    owned_by: String,
}

impl From<RawModel> for ModelInfo {
    fn from(raw: RawModel) -> Self {
        // NVIDIA's hosted API is free for every catalog model.
        ModelInfo {
            id: raw.id,
            name: raw.name,
            description: raw.owned_by,
            context_length: None,
            is_free: true,
            prompt_price: 0.0,
            completion_price: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODELS_FIXTURE: &str = r#"{
        "object": "list",
        "data": [
            { "id": "meta/llama-3.3-70b-instruct", "object": "model", "created": 735790403, "owned_by": "meta" },
            { "id": "nvidia/nemotron-3-super-120b-a12b", "object": "model", "created": 735790403, "owned_by": "nvidia" }
        ]
    }"#;

    #[test]
    fn parses_openai_shaped_model_list_as_free() {
        let parsed: RawModelsResponse = serde_json::from_str(MODELS_FIXTURE).unwrap();
        let models: Vec<ModelInfo> = parsed.data.into_iter().map(ModelInfo::from).collect();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "meta/llama-3.3-70b-instruct");
        assert!(models[0].is_free);
        assert!(models[1].is_free);
        assert_eq!(models[1].description, "nvidia");
        assert_eq!(models[0].context_length, None);
    }

    #[test]
    fn requires_api_key() {
        let config = Config {
            nvidia_api_key: None,
            ..Config::default()
        };
        assert!(NvidiaProvider::from_config(&config).is_err());
    }

    #[test]
    fn for_model_listing_never_requires_a_key() {
        let config = Config {
            nvidia_api_key: None,
            ..Config::default()
        };
        let provider = NvidiaProvider::for_model_listing(&config);
        assert!(!provider.is_available());
        assert_eq!(provider.base_url, NVIDIA_DEFAULT_BASE_URL.to_string());

        let config = Config {
            nvidia_api_key: Some("nvapi-test".to_string()),
            ..Config::default()
        };
        let provider = NvidiaProvider::for_model_listing(&config);
        assert!(provider.is_available());
    }

    #[test]
    fn tolerates_extra_fields_in_model_rows() {
        let body = r#"{
            "object": "list",
            "data": [
                { "id": "deepseek-ai/deepseek-r1", "object": "model", "created": 735790403, "owned_by": "deepseek-ai", "some_future_field": true }
            ]
        }"#;
        let parsed: RawModelsResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.data.len(), 1);
        let info = ModelInfo::from(parsed.data.into_iter().next().unwrap());
        assert!(info.is_free);
        assert_eq!(info.id, "deepseek-ai/deepseek-r1");
    }
}
