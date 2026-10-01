//! Local adapter: Ollama (`http://localhost:11434/v1`, OpenAI-compatible).
//!
//! Zero-latency offline path. No key required. Key from config only when a
//! remote Ollama needs one (`DOREAN_LOCAL_API_KEY`).

use std::pin::Pin;
use std::time::Duration;

use futures_util::stream::Stream;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;

use crate::config::Config;
use crate::error::DoreanError;

use super::client::{ChatRequest, CompletionEvent, ModelInfo, stream_chat};

pub const LOCAL_DEFAULT_BASE_URL: &str = "http://localhost:11434/v1";
pub const DEFAULT_LOCAL_MODEL: &str = "qwen2.5-coder:7b";
const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct LocalProvider {
    http: reqwest::Client,
    base_url: String,
    list_timeout: Duration,
}

impl LocalProvider {
    pub fn from_config(config: &Config) -> Result<Self, DoreanError> {
        Ok(Self {
            http: Self::http_client(),
            base_url: config
                .base_url
                .clone()
                .unwrap_or_else(|| LOCAL_DEFAULT_BASE_URL.to_string()),
            list_timeout: LIST_MODELS_TIMEOUT,
        })
    }

    pub fn for_model_listing(config: &Config) -> Self {
        Self {
            http: Self::http_client(),
            base_url: config
                .base_url
                .clone()
                .unwrap_or_else(|| LOCAL_DEFAULT_BASE_URL.to_string()),
            list_timeout: LIST_MODELS_TIMEOUT,
        }
    }

    fn http_client() -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .pool_max_idle_per_host(8)
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .expect("failed to build http client")
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            http: Self::http_client(),
            base_url: base_url.into(),
            list_timeout: LIST_MODELS_TIMEOUT,
        }
    }

    pub fn is_available(&self) -> bool {
        true
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, DoreanError> {
        let url = format!("{}/models", self.base_url);
        let response = self
            .http
            .get(&url)
            .timeout(self.list_timeout)
            .send()
            .await?;
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

    pub async fn list_free_models(&self) -> Result<Vec<ModelInfo>, DoreanError> {
        // Local models are all free (they run on your machine).
        let mut models = self.list_models().await?;
        for m in &mut models {
            m.is_free = true;
        }
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    pub fn stream_chat(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Stream<Item = Result<CompletionEvent, DoreanError>> + Send>> {
        let url = format!("{}/chat/completions", self.base_url);
        let headers = vec![(
            CONTENT_TYPE.as_str().to_string(),
            "application/json".to_string(),
        )];
        stream_chat(self.http.clone(), url, headers, request.to_json())
    }
}

#[derive(Debug, Deserialize)]
struct RawModelsResponse {
    data: Vec<RawModel>,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    id: String,
}

impl From<RawModel> for ModelInfo {
    fn from(raw: RawModel) -> Self {
        ModelInfo {
            id: raw.id,
            name: String::new(),
            description: "local".to_string(),
            context_length: None,
            is_free: true,
            prompt_price: 0.0,
            completion_price: 0.0,
        }
    }
}
