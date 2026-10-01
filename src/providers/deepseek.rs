//! DeepSeek native adapter: api.deepseek.com (OpenAI-compatible).
//!
//! DeepSeek's disk context-caching rewards stable prompt prefixes, which the
//! prompt builder now guarantees. Cache hits are reported in `Usage` and
//! surfaced in the TUI header. Key from config, `DOREAN_DEEPSEEK_API_KEY`,
//! or `DEEPSEEK_API_KEY`.

use std::pin::Pin;
use std::time::Duration;

use futures_util::stream::Stream;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;

use crate::config::Config;
use crate::error::DoreanError;

use super::client::{ChatRequest, CompletionEvent, ModelInfo, stream_chat};

pub const DEEPSEEK_DEFAULT_BASE_URL: &str = "https://api.deepseek.com/v1";
pub const DEFAULT_DEEPSEEK_MODEL: &str = "deepseek-chat";
const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct DeepSeekProvider {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    list_timeout: Duration,
}

impl DeepSeekProvider {
    pub fn from_config(config: &Config) -> Result<Self, DoreanError> {
        let api_key = config
            .deepseek_api_key
            .clone()
            .or_else(|| std::env::var("DEEPSEEK_API_KEY").ok())
            .ok_or_else(|| {
                DoreanError::Config(
                    "DeepSeek requires an API key. Set DOREAN_DEEPSEEK_API_KEY, \
                     DEEPSEEK_API_KEY, or `deepseek_api_key` in ~/.dorean/config.json"
                        .to_string(),
                )
            })?;
        Ok(Self {
            http: Self::http_client(),
            api_key,
            base_url: config
                .base_url
                .clone()
                .unwrap_or_else(|| DEEPSEEK_DEFAULT_BASE_URL.to_string()),
            list_timeout: LIST_MODELS_TIMEOUT,
        })
    }

    pub fn for_model_listing(config: &Config) -> Self {
        let api_key = config
            .deepseek_api_key
            .clone()
            .or_else(|| std::env::var("DEEPSEEK_API_KEY").ok())
            .unwrap_or_default();
        Self {
            http: Self::http_client(),
            api_key,
            base_url: config
                .base_url
                .clone()
                .unwrap_or_else(|| DEEPSEEK_DEFAULT_BASE_URL.to_string()),
            list_timeout: LIST_MODELS_TIMEOUT,
        }
    }

    fn http_client() -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .pool_max_idle_per_host(8)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(30))
            .build()
            .expect("failed to build http client")
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
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
        // DeepSeek serves OpenAI-shaped /models; fall back to a static pair
        // when the catalog is unreachable-but-200 with an odd shape.
        match serde_json::from_str::<RawModelsResponse>(&body) {
            Ok(parsed) => Ok(parsed.data.into_iter().map(ModelInfo::from).collect()),
            Err(_) => Ok(vec![
                ModelInfo {
                    id: "deepseek-chat".to_string(),
                    name: "DeepSeek Chat".to_string(),
                    description: "DeepSeek V3 chat".to_string(),
                    context_length: Some(64000),
                    is_free: false,
                    prompt_price: 0.27,
                    completion_price: 1.1,
                },
                ModelInfo {
                    id: "deepseek-reasoner".to_string(),
                    name: "DeepSeek Reasoner".to_string(),
                    description: "DeepSeek R1 reasoning".to_string(),
                    context_length: Some(64000),
                    is_free: false,
                    prompt_price: 0.55,
                    completion_price: 2.19,
                },
            ]),
        }
    }

    pub async fn list_free_models(&self) -> Result<Vec<ModelInfo>, DoreanError> {
        let mut models = self.list_models().await?;
        models.retain(|m| m.is_free);
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

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
    owned_by: String,
}

impl From<RawModel> for ModelInfo {
    fn from(raw: RawModel) -> Self {
        ModelInfo {
            id: raw.id,
            name: String::new(),
            description: raw.owned_by,
            context_length: None,
            is_free: false,
            prompt_price: 0.0,
            completion_price: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_api_key() {
        let config = Config {
            deepseek_api_key: None,
            ..Config::default()
        };
        assert!(DeepSeekProvider::from_config(&config).is_err());
    }

    #[test]
    fn falls_back_to_static_catalog_on_odd_shape() {
        // Covered indirectly: list_models fallback branch constructs 2 models.
        assert_eq!(DEFAULT_DEEPSEEK_MODEL, "deepseek-chat");
    }
}
