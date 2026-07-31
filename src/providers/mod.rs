//! LLM provider layer: hosted OpenRouter free tier and NVIDIA hosted NIM APIs.
//!
//! The shared OpenAI-compatible streaming core lives in [`client`]; the
//! [`openrouter`] and [`nvidia`] adapters wrap it. [`AnyProvider`] dispatches
//! to the adapter selected by [`Config::provider`].

use std::pin::Pin;

use futures_util::stream::Stream;

use crate::config::{Config, Provider};
use crate::error::DoreanError;

pub mod client;
pub mod nvidia;
pub mod openrouter;

use client::{ChatRequest, CompletionEvent, ModelInfo};
pub use nvidia::{DEFAULT_NVIDIA_MODEL, NvidiaProvider};
pub use openrouter::{DEFAULT_FREE_MODEL, OpenRouterProvider};

/// Default model id for a provider when config has none set.
pub fn default_model(provider: Provider) -> &'static str {
    match provider {
        Provider::OpenRouter => DEFAULT_FREE_MODEL,
        Provider::Nvidia => DEFAULT_NVIDIA_MODEL,
    }
}

/// The configured provider adapter, erased behind a small enum so the agent
/// loop and the TUI can hold one type regardless of backend. Both adapters
/// speak the same OpenAI-compatible wire format.
#[derive(Clone)]
pub enum AnyProvider {
    OpenRouter(OpenRouterProvider),
    Nvidia(NvidiaProvider),
}

impl AnyProvider {
    /// Build the provider selected by `config.provider`. Requires that
    /// provider's API key.
    pub fn from_config(config: &Config) -> Result<Self, DoreanError> {
        match config.provider {
            Provider::OpenRouter => {
                OpenRouterProvider::from_config(config).map(AnyProvider::OpenRouter)
            }
            Provider::Nvidia => NvidiaProvider::from_config(config).map(AnyProvider::Nvidia),
        }
    }

    /// Build a provider for model *listing* only. Never errors on a missing
    /// key; the key is attached when present.
    pub fn for_model_listing(config: &Config) -> Self {
        match config.provider {
            Provider::OpenRouter => {
                AnyProvider::OpenRouter(OpenRouterProvider::for_model_listing(config))
            }
            Provider::Nvidia => AnyProvider::Nvidia(NvidiaProvider::for_model_listing(config)),
        }
    }

    pub fn is_available(&self) -> bool {
        match self {
            AnyProvider::OpenRouter(p) => p.is_available(),
            AnyProvider::Nvidia(p) => p.is_available(),
        }
    }

    /// Fetch the full model list from `GET /models`.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, DoreanError> {
        match self {
            AnyProvider::OpenRouter(p) => p.list_models().await,
            AnyProvider::Nvidia(p) => p.list_models().await,
        }
    }

    /// Model list restricted to free models.
    pub async fn list_free_models(&self) -> Result<Vec<ModelInfo>, DoreanError> {
        match self {
            AnyProvider::OpenRouter(p) => p.list_free_models().await,
            AnyProvider::Nvidia(p) => p.list_free_models().await,
        }
    }

    /// Stream a chat completion.
    pub fn stream_chat(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Stream<Item = Result<CompletionEvent, DoreanError>> + Send>> {
        match self {
            AnyProvider::OpenRouter(p) => p.stream_chat(request),
            AnyProvider::Nvidia(p) => p.stream_chat(request),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_models_are_provider_aware() {
        assert_eq!(default_model(Provider::OpenRouter), "openrouter/free");
        assert_eq!(
            default_model(Provider::Nvidia),
            "nvidia/nemotron-3-ultra-550b-a55b"
        );
    }

    #[test]
    fn dispatcher_requires_the_active_providers_key() {
        let config = Config {
            provider: Provider::Nvidia,
            openrouter_api_key: Some("sk-test".to_string()),
            nvidia_api_key: None,
            ..Config::default()
        };
        assert!(AnyProvider::from_config(&config).is_err());

        let config = Config {
            provider: Provider::Nvidia,
            openrouter_api_key: None,
            nvidia_api_key: Some("nvapi-test".to_string()),
            ..Config::default()
        };
        assert!(matches!(
            AnyProvider::from_config(&config),
            Ok(AnyProvider::Nvidia(_))
        ));
    }
}
