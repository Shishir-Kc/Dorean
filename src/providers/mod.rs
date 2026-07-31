//! LLM provider layer: hosted OpenRouter free tier.
//!
//! The shared OpenAI-compatible streaming core lives in [`client`]; the
//! [`openrouter`] adapter wraps it.

pub mod client;
pub mod openrouter;

pub use openrouter::{DEFAULT_FREE_MODEL, OpenRouterProvider};
