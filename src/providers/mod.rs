//! LLM provider layer: hosted OpenRouter free tier, NVIDIA hosted NIM,
//! DeepSeek native, local Ollama, and generic OpenAI-compatible endpoints.
//!
//! The shared OpenAI-compatible streaming core lives in [`client`]; the
//! adapters wrap it. [`AnyProvider`] dispatches to the adapter selected by
//! [`Config::provider`].

use std::pin::Pin;

use futures_util::stream::Stream;

use crate::config::{Config, Provider};
use crate::error::DoreanError;

pub mod client;
pub mod deepseek;
pub mod generic;
pub mod local;
pub mod nvidia;
pub mod openrouter;

use client::{ChatRequest, CompletionEvent, ModelInfo};
pub use deepseek::{DEFAULT_DEEPSEEK_MODEL, DeepSeekProvider};
pub use generic::{DEFAULT_GENERIC_MODEL, GenericProvider};
pub use local::{DEFAULT_LOCAL_MODEL, LocalProvider};
pub use nvidia::{DEFAULT_NVIDIA_MODEL, NvidiaProvider};
pub use openrouter::{DEFAULT_FREE_MODEL, OpenRouterProvider};

/// Default model id for a provider when config has none set.
pub fn default_model(provider: Provider) -> &'static str {
    match provider {
        Provider::OpenRouter => DEFAULT_FREE_MODEL,
        Provider::Nvidia => DEFAULT_NVIDIA_MODEL,
        Provider::DeepSeek => DEFAULT_DEEPSEEK_MODEL,
        Provider::Local => DEFAULT_LOCAL_MODEL,
        Provider::Generic => DEFAULT_GENERIC_MODEL,
    }
}

/// Executor default: cheap model for routine sub-agent turns. Falls back to
/// the provider default when no explicit executor/planner is configured.
pub fn executor_model(config: &Config) -> String {
    config
        .executor_model
        .clone()
        .or_else(|| config.model.clone())
        .unwrap_or_else(|| default_model(config.provider).to_string())
}

/// The configured provider adapter, erased behind a small enum so the agent
/// loop and the TUI can hold one type regardless of backend. All adapters
/// speak the same OpenAI-compatible wire format.
#[derive(Clone)]
pub enum AnyProvider {
    OpenRouter(OpenRouterProvider),
    Nvidia(NvidiaProvider),
    DeepSeek(DeepSeekProvider),
    Local(LocalProvider),
    Generic(GenericProvider),
}

impl AnyProvider {
    /// Build the provider selected by `config.provider`. Requires that
    /// provider's API key (except local, which needs none).
    pub fn from_config(config: &Config) -> Result<Self, DoreanError> {
        match config.provider {
            Provider::OpenRouter => {
                OpenRouterProvider::from_config(config).map(AnyProvider::OpenRouter)
            }
            Provider::Nvidia => NvidiaProvider::from_config(config).map(AnyProvider::Nvidia),
            Provider::DeepSeek => DeepSeekProvider::from_config(config).map(AnyProvider::DeepSeek),
            Provider::Local => LocalProvider::from_config(config).map(AnyProvider::Local),
            Provider::Generic => GenericProvider::from_config(config).map(AnyProvider::Generic),
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
            Provider::DeepSeek => {
                AnyProvider::DeepSeek(DeepSeekProvider::for_model_listing(config))
            }
            Provider::Local => AnyProvider::Local(LocalProvider::for_model_listing(config)),
            Provider::Generic => AnyProvider::Generic(GenericProvider::for_model_listing(config)),
        }
    }

    pub fn is_available(&self) -> bool {
        match self {
            AnyProvider::OpenRouter(p) => p.is_available(),
            AnyProvider::Nvidia(p) => p.is_available(),
            AnyProvider::DeepSeek(p) => p.is_available(),
            AnyProvider::Local(p) => p.is_available(),
            AnyProvider::Generic(p) => p.is_available(),
        }
    }

    /// Fetch the full model list from `GET /models`.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, DoreanError> {
        match self {
            AnyProvider::OpenRouter(p) => p.list_models().await,
            AnyProvider::Nvidia(p) => p.list_models().await,
            AnyProvider::DeepSeek(p) => p.list_models().await,
            AnyProvider::Local(p) => p.list_models().await,
            AnyProvider::Generic(p) => p.list_models().await,
        }
    }

    /// Model list restricted to free models.
    pub async fn list_free_models(&self) -> Result<Vec<ModelInfo>, DoreanError> {
        match self {
            AnyProvider::OpenRouter(p) => p.list_free_models().await,
            AnyProvider::Nvidia(p) => p.list_free_models().await,
            AnyProvider::DeepSeek(p) => p.list_free_models().await,
            AnyProvider::Local(p) => p.list_free_models().await,
            AnyProvider::Generic(p) => p.list_free_models().await,
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
            AnyProvider::DeepSeek(p) => p.stream_chat(request),
            AnyProvider::Local(p) => p.stream_chat(request),
            AnyProvider::Generic(p) => p.stream_chat(request),
        }
    }
}

/// Aggregated catalog fetch across every provider.
pub struct FetchAll {
    /// One entry per (provider, model), free first, then provider, then id.
    pub tagged: Vec<(Provider, ModelInfo)>,
    /// Human-actionable one-liners for providers that failed.
    pub problems: Vec<String>,
}

/// Fetch the full model catalog from every provider in parallel (30s cap
/// each). Unreachable providers land in `problems` and are never fatal, so
/// `/model` shows whatever arrived instead of failing outright.
pub async fn fetch_all_models(config: &Config) -> FetchAll {
    let providers = [
        Provider::OpenRouter,
        Provider::Nvidia,
        Provider::DeepSeek,
        Provider::Local,
        Provider::Generic,
    ];
    let results = futures_util::future::join_all(providers.iter().map(|p| {
        let mut c = config.clone();
        c.provider = *p;
        let provider = AnyProvider::for_model_listing(&c);
        async move {
            let res =
                tokio::time::timeout(std::time::Duration::from_secs(30), provider.list_models())
                    .await;
            (*p, res)
        }
    }))
    .await;
    let mut tagged = Vec::new();
    let mut problems = Vec::new();
    for (p, res) in results {
        match res {
            Ok(Ok(models)) => {
                for m in models {
                    tagged.push((p, m));
                }
            }
            Ok(Err(e)) => problems.push(friendly_fetch_problem(p, &e, config)),
            Err(_) => problems.push(format!("{p}: timed out after 30s")),
        }
    }
    // Stable order so the picker doesn't reshuffle between fetches.
    tagged.sort_by(|a, b| {
        b.1.is_free
            .cmp(&a.1.is_free)
            .then(a.0.to_string().cmp(&b.0.to_string()))
            .then(a.1.id.cmp(&b.1.id))
    });
    FetchAll { tagged, problems }
}

/// Render a `fetch_all_models` result as a human-readable per-provider
/// report (one line per provider plus problems). Used by `--list-models` and
/// worth mirroring into bug reports when `/model` misbehaves.
pub fn format_fetch_report(fetched: &FetchAll) -> String {
    let mut per_provider: std::collections::BTreeMap<String, (usize, usize)> =
        std::collections::BTreeMap::new();
    for (provider, info) in &fetched.tagged {
        let entry = per_provider.entry(provider.to_string()).or_insert((0, 0));
        entry.0 += 1;
        if info.is_free {
            entry.1 += 1;
        }
    }
    let mut out = String::new();
    for name in ["openrouter", "nvidia", "deepseek", "local", "generic"] {
        match per_provider.get(name) {
            Some((total, free)) => {
                out.push_str(&format!("{name}: {total} models ({free} free)\n"));
            }
            None => {
                let reason = fetched
                    .problems
                    .iter()
                    .find(|p| p.starts_with(name))
                    .map(String::as_str)
                    .unwrap_or("no data");
                out.push_str(&format!("{name}: FAILED — {reason}\n"));
            }
        }
    }
    for problem in &fetched.problems {
        if !["openrouter", "nvidia", "deepseek", "local", "generic"]
            .iter()
            .any(|n| problem.starts_with(n))
        {
            out.push_str(&format!("note: {problem}\n"));
        }
    }
    out.push_str(&format!(
        "total: {} models from {} provider(s)\n",
        fetched.tagged.len(),
        per_provider.len()
    ));
    out
}

/// Human-actionable one-liner for a provider whose model list failed.
/// Connection-level failures on local/generic get setup hints instead of raw
/// transport errors; a custom `DOREAN_BASE_URL` is called out explicitly
/// because it reroutes the hosted providers' listings too.
pub fn friendly_fetch_problem(provider: Provider, error: &DoreanError, config: &Config) -> String {
    let text = error.to_string();
    let connection_failed = text.contains("refused")
        || text.contains("Failed to connect")
        || text.contains("timed out")
        || text.contains("timeout")
        || text.contains("dns error")
        || text.contains("certificate");
    let via = config
        .base_url
        .as_deref()
        .map(|u| format!(" (via DOREAN_BASE_URL={u})"))
        .unwrap_or_default();
    match provider {
        Provider::Local if connection_failed => {
            "local: ollama not running — start it with `ollama serve`".to_string()
        }
        Provider::Generic if config.base_url.is_none() => {
            "generic: set DOREAN_BASE_URL to your OpenAI-compatible endpoint".to_string()
        }
        _ => format!("{provider}: {text}{via}"),
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
        assert_eq!(default_model(Provider::DeepSeek), "deepseek-chat");
        assert_eq!(default_model(Provider::Local), "qwen2.5-coder:7b");
        assert_eq!(default_model(Provider::Generic), "default");
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

    #[test]
    fn local_needs_no_key() {
        let config = Config {
            provider: Provider::Local,
            ..Config::default()
        };
        assert!(matches!(
            AnyProvider::from_config(&config),
            Ok(AnyProvider::Local(_))
        ));
    }

    #[test]
    fn fetch_problems_point_at_ollama_and_base_url() {
        let config = Config::default();
        let refused = DoreanError::Message("connection refused".to_string());
        assert!(
            friendly_fetch_problem(Provider::Local, &refused, &config).contains("ollama serve")
        );
        assert!(
            friendly_fetch_problem(Provider::Generic, &refused, &config)
                .contains("DOREAN_BASE_URL")
        );
    }

    #[test]
    fn fetch_problems_call_out_custom_base_url() {
        let config = Config {
            base_url: Some("http://127.0.0.1:9".to_string()),
            ..Config::default()
        };
        let err = DoreanError::Provider {
            status: 500,
            message: "boom".to_string(),
        };
        let text = friendly_fetch_problem(Provider::OpenRouter, &err, &config);
        assert!(
            text.contains("DOREAN_BASE_URL=http://127.0.0.1:9"),
            "{text}"
        );
    }

    #[test]
    fn fetch_report_names_each_provider() {
        use crate::providers::client::ModelInfo;
        let fetched = FetchAll {
            tagged: vec![
                (
                    Provider::OpenRouter,
                    ModelInfo {
                        id: "a:free".to_string(),
                        name: String::new(),
                        description: String::new(),
                        context_length: None,
                        is_free: true,
                        prompt_price: 0.0,
                        completion_price: 0.0,
                    },
                ),
                (
                    Provider::Nvidia,
                    ModelInfo {
                        id: "b".to_string(),
                        name: String::new(),
                        description: String::new(),
                        context_length: None,
                        is_free: true,
                        prompt_price: 0.0,
                        completion_price: 0.0,
                    },
                ),
            ],
            problems: vec!["local: ollama not running".to_string()],
        };
        let report = format_fetch_report(&fetched);
        assert!(report.contains("openrouter: 1 models (1 free)"), "{report}");
        assert!(report.contains("local: FAILED"), "{report}");
        assert!(
            report.contains("total: 2 models from 2 provider(s)"),
            "{report}"
        );
    }
}
