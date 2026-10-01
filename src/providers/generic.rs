//! Generic adapter: any OpenAI-compatible endpoint via `base_url`.
//!
//! Configure with `DOREAN_BASE_URL` (or `"base_url"` in config) plus an
//! optional key (`DOREAN_GENERIC_API_KEY` / `"generic_api_key"`). Model list
//! is OpenAI-shaped `GET /models`; chat is `POST /chat/completions`.

use std::pin::Pin;
use std::time::Duration;

use futures_util::stream::Stream;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};

use crate::config::Config;
use crate::error::DoreanError;

use super::client::{ChatRequest, CompletionEvent, ModelInfo, stream_chat};
use super::local::{LOCAL_DEFAULT_BASE_URL, LocalProvider as GenericLister};

pub const DEFAULT_GENERIC_MODEL: &str = "default";

const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct GenericProvider {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    list_timeout: Duration,
}

impl GenericProvider {
    pub fn from_config(config: &Config) -> Result<Self, DoreanError> {
        let base_url = config
            .base_url
            .clone()
            .unwrap_or_else(|| LOCAL_DEFAULT_BASE_URL.to_string());
        let api_key = config
            .generic_api_key
            .clone()
            .or_else(|| std::env::var("DOREAN_GENERIC_API_KEY").ok())
            .unwrap_or_default();
        Ok(Self {
            http: Self::http_client(),
            api_key,
            base_url,
            list_timeout: LIST_MODELS_TIMEOUT,
        })
    }

    pub fn for_model_listing(config: &Config) -> Self {
        let base_url = config
            .base_url
            .clone()
            .unwrap_or_else(|| LOCAL_DEFAULT_BASE_URL.to_string());
        let api_key = config
            .generic_api_key
            .clone()
            .or_else(|| std::env::var("DOREAN_GENERIC_API_KEY").ok())
            .unwrap_or_default();
        Self {
            http: Self::http_client(),
            api_key,
            base_url,
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

    pub fn is_available(&self) -> bool {
        !self.base_url.is_empty()
    }

    fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![(
            CONTENT_TYPE.as_str().to_string(),
            "application/json".to_string(),
        )];
        if !self.api_key.is_empty() {
            headers.push((
                AUTHORIZATION.as_str().to_string(),
                format!("Bearer {}", self.api_key),
            ));
        }
        headers
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, DoreanError> {
        // Reuse the OpenAI-shaped lister against our base URL.
        let lister = GenericLister::with_base_url(self.base_url.clone());
        // LocalProvider::with_base_url uses its own timeouts; call through the
        // shared client path for uniformity on errors.
        let _ = &self.list_timeout;
        lister.list_models().await
    }

    pub async fn list_free_models(&self) -> Result<Vec<ModelInfo>, DoreanError> {
        let mut models = self.list_models().await?;
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
