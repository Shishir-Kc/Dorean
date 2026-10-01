use clap::Parser;

use crate::config::Provider;
use crate::permissions::PermissionMode;

/// Command-line interface for dorean.
#[derive(Debug, Parser)]
#[command(
    name = "dorean",
    version,
    about = "A pi/opencode-style coding agent for the terminal",
    after_help = "Run `dorean -m \"...\"` for a one-shot prompt, add `--orchestrate` to fan \
                  the goal out to parallel sub-agents, or run plain `dorean` to open the chat TUI."
)]
pub struct Cli {
    /// Run a single non-interactive prompt instead of opening the TUI.
    #[arg(short = 'm', long = "message", value_name = "MSG")]
    pub message: Option<String>,

    /// Resume the most recent session.
    #[arg(short = 'c', long = "continue")]
    pub resume: bool,

    /// Override the provider from config (`openrouter` | `nvidia` | `deepseek` | `local` | `generic`).
    #[arg(long = "provider", value_name = "PROVIDER")]
    pub provider: Option<Provider>,

    /// Override the model from config (e.g. `meta-llama/llama-3.3-70b-instruct:free`).
    #[arg(long = "model", value_name = "MODEL")]
    pub model: Option<String>,

    /// Print the response as it streams, with no decorations.
    #[arg(short = 'p', long = "print")]
    pub print: bool,

    /// Fan the message out to parallel sub-agents (orchestration pipeline).
    #[arg(long = "orchestrate")]
    pub orchestrate: bool,

    /// Stack to build when orchestrating (full-stack | backend | frontend | cli).
    #[arg(long = "stack", value_name = "STACK")]
    pub stack: Option<String>,

    /// Pick the default stack without prompting.
    #[arg(long = "auto-stack")]
    pub auto_stack: bool,

    /// Tool approval policy: allow | ask | deny.
    #[arg(long = "permission", value_name = "MODE")]
    pub permission: Option<PermissionMode>,

    /// List models from every provider (with per-provider errors) and exit.
    /// Diagnostics for `/model`: shows exactly what each catalog returned.
    #[arg(long = "list-models")]
    pub list_models: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_non_interactive_prompt() {
        let cli = Cli::try_parse_from(["dorean", "-m", "fix the bug"]).unwrap();
        assert_eq!(cli.message.as_deref(), Some("fix the bug"));
        assert!(!cli.resume && !cli.print);
    }

    #[test]
    fn parses_resume_and_model() {
        let cli = Cli::try_parse_from([
            "dorean",
            "-c",
            "--model",
            "meta-llama/llama-3.3-70b-instruct:free",
        ])
        .unwrap();
        assert!(cli.resume);
        assert_eq!(
            cli.model.as_deref(),
            Some("meta-llama/llama-3.3-70b-instruct:free")
        );
    }

    #[test]
    fn parses_provider_flag() {
        let cli = Cli::try_parse_from(["dorean", "--provider", "openrouter"]).unwrap();
        assert_eq!(cli.provider, Some(Provider::OpenRouter));
        assert!(Cli::try_parse_from(["dorean", "--provider", "bogus"]).is_err());
    }

    #[test]
    fn parses_print_short_flag() {
        let cli = Cli::try_parse_from(["dorean", "-p"]).unwrap();
        assert!(cli.print);
    }

    #[test]
    fn missing_message_is_ok() {
        // No args: launch the TUI. Parsing must succeed.
        assert!(Cli::try_parse_from(["dorean"]).is_ok());
    }

    #[test]
    fn rejects_unknown_flags() {
        assert!(Cli::try_parse_from(["dorean", "--bogus"]).is_err());
    }

    #[test]
    fn parses_orchestration_flags() {
        let cli = Cli::try_parse_from([
            "dorean",
            "-m",
            "build it",
            "--orchestrate",
            "--stack",
            "full-stack",
        ])
        .unwrap();
        assert!(cli.orchestrate);
        assert_eq!(cli.stack.as_deref(), Some("full-stack"));

        let cli = Cli::try_parse_from(["dorean", "-m", "x", "--auto-stack"]).unwrap();
        assert!(cli.auto_stack);
    }

    #[test]
    fn parses_permission_flag() {
        let cli = Cli::try_parse_from(["dorean", "-m", "x", "--permission", "allow"]).unwrap();
        assert_eq!(cli.permission, Some(PermissionMode::Allow));
        assert!(Cli::try_parse_from(["dorean", "--permission", "bogus"]).is_err());
    }

    #[test]
    fn parses_list_models_flag() {
        let cli = Cli::try_parse_from(["dorean", "--list-models"]).unwrap();
        assert!(cli.list_models);
        assert!(!Cli::try_parse_from(["dorean"]).unwrap().list_models);
    }
}
