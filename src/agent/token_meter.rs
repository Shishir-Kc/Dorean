//! Token metering: cheap local estimates plus provider-reported usage.
//!
//! The estimator (`estimate_tokens`) uses ~4 chars/token, accurate enough for
//! compaction thresholds and cost display without a tokenizer dependency.
//! Provider [`Usage`] (when streamed) is authoritative and accumulates in
//! [`TokenMeter`].

use crate::providers::client::{Message, Usage};

/// Rough chars-per-token for the estimator.
pub const CHARS_PER_TOKEN: usize = 4;

/// Estimate tokens in plain text.
pub fn estimate_tokens(text: &str) -> u64 {
    text.len().div_ceil(CHARS_PER_TOKEN) as u64
}

/// Estimate tokens for a full message list (content + small per-message overhead).
pub fn estimate_messages(messages: &[Message]) -> u64 {
    messages
        .iter()
        .map(|m| estimate_tokens(&m.content) + 8)
        .sum()
}

/// Running totals for one agent run.
#[derive(Debug, Clone, Default)]
pub struct TokenMeter {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub estimated_tokens: u64,
}

impl TokenMeter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold provider-reported usage into the totals.
    pub fn add_usage(&mut self, usage: &Usage) {
        self.prompt_tokens += usage.prompt_tokens;
        self.completion_tokens += usage.completion_tokens;
        if let Some(details) = &usage.prompt_tokens_details {
            self.cached_tokens += details.cached_tokens;
        }
    }

    /// Record a local estimate (used when the provider omits usage).
    pub fn add_estimate(&mut self, tokens: u64) {
        self.estimated_tokens += tokens;
    }

    pub fn total(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens + self.estimated_tokens
    }

    /// Fraction of prompt tokens served from cache (0.0-1.0).
    pub fn cache_hit_rate(&self) -> f64 {
        if self.prompt_tokens == 0 {
            0.0
        } else {
            self.cached_tokens as f64 / self.prompt_tokens as f64
        }
    }

    /// Rough USD cost given per-1M-token prices.
    pub fn cost_usd(&self, prompt_price: f64, completion_price: f64) -> f64 {
        self.prompt_tokens as f64 / 1_000_000.0 * prompt_price
            + self.completion_tokens as f64 / 1_000_000.0 * completion_price
    }

    pub fn summary(&self) -> String {
        format!(
            "{} tok ({} cached, {:.0}% hit)",
            self.total(),
            self.cached_tokens,
            self.cache_hit_rate() * 100.0
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimator_scales_with_length() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
    }

    #[test]
    fn meter_accumulates_and_rates_cache() {
        let mut meter = TokenMeter::new();
        meter.add_usage(&Usage {
            prompt_tokens: 1000,
            completion_tokens: 200,
            total_tokens: 1200,
            prompt_tokens_details: Some(crate::providers::client::PromptTokensDetails {
                cached_tokens: 900,
            }),
            completion_tokens_details: None,
        });
        assert_eq!(meter.total(), 1200);
        assert!((meter.cache_hit_rate() - 0.9).abs() < 1e-9);
        assert!(meter.cost_usd(1.0, 2.0) > 0.0);
    }
}
