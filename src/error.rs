use std::io::Write;

/// Central error type for dorean.
#[derive(Debug, thiserror::Error)]
pub enum DoreanError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("config error: {0}")]
    Config(String),
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("provider error: HTTP {status}: {message}")]
    Provider { status: u16, message: String },
    #[error("provider stream error: {0}")]
    Stream(String),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not implemented yet: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Message(String),
}

impl DoreanError {
    /// Whether a retryable request should be retried with backoff.
    /// Transient network failures, provider stream hiccups, and HTTP 429/5xx
    /// responses are retried; anything else is a hard failure.
    pub fn is_retryable(&self) -> bool {
        match self {
            DoreanError::Network(_) | DoreanError::Stream(_) => true,
            DoreanError::Provider { status, .. } => *status == 429 || *status >= 500,
            _ => false,
        }
    }
}

/// Install a panic hook that restores the terminal and prints a friendly message.
///
/// Replaces the default hook so a crash in the TUI leaves the terminal usable
/// (raw mode is disabled) and shows where the crash happened instead of a
/// raw panic dump. Best-effort: every terminal restore call is swallowed.
pub fn install_panic_handler() {
    std::panic::set_hook(Box::new(|info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "\n\x1b[31mdorean crashed\x1b[0m");
        if let Some(location) = info.location() {
            let _ = writeln!(
                stderr,
                "  at {}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            );
        }
        let payload = info.payload();
        if let Some(s) = payload.downcast_ref::<&str>() {
            let _ = writeln!(stderr, "  {s}");
        } else if let Some(s) = payload.downcast_ref::<String>() {
            let _ = writeln!(stderr, "  {s}");
        }
        let _ = writeln!(stderr, "\nSet RUST_BACKTRACE=1 for a full backtrace.");
    }));
}
