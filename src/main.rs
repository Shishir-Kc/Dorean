use clap::Parser;
use tokio::sync::broadcast;

use dorean::agent::events::AgentEvent;
use dorean::agent::stack::StackChoice;
use dorean::agent::{AbortHandle, Orchestrator, RunOptions};
use dorean::cli::Cli;
use dorean::config::Config;
use dorean::error::{DoreanError, install_panic_handler};

fn main() {
    install_panic_handler();
    let cli = Cli::parse();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("dorean: failed to start async runtime: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = runtime.block_on(run(cli)) {
        eprintln!("dorean: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), DoreanError> {
    let mut config = Config::load()?.apply_env();
    if let Some(provider) = cli.provider {
        config.provider = provider;
    }
    if let Some(model) = cli.model.clone() {
        config.model = Some(model);
    }
    if let Some(mode) = cli.permission {
        config.permission_mode = Some(mode);
    }

    match &cli.message {
        Some(message) => {
            // Non-interactive runs default to Allow; the TUI defaults to Ask.
            config
                .permission_mode
                .get_or_insert(dorean::permissions::PermissionMode::Allow);
            let cwd = std::env::current_dir()?;

            let abort = AbortHandle::new();
            let abort_for_signal = abort.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    abort_for_signal.abort();
                }
            });

            if cli.orchestrate {
                run_orchestrated(&config, message, &cli, &cwd, abort).await
            } else {
                let options = RunOptions {
                    message,
                    print: cli.print,
                    resume: cli.resume,
                    cwd,
                    abort,
                };
                let summary = dorean::agent::run_once(&config, &options).await?;
                if !cli.print && !summary.response.is_empty() {
                    println!("{}", summary.response);
                }
                if summary.aborted {
                    eprintln!("dorean: aborted after {} turn(s)", summary.turns);
                }
                Ok(())
            }
        }
        None => {
            if cli.print {
                return Err(DoreanError::Message(
                    "-p/--print streams a response, so it requires -m/--message".to_string(),
                ));
            }
            if cli.orchestrate || cli.stack.is_some() || cli.auto_stack {
                return Err(DoreanError::Message(
                    "--orchestrate requires -m/--message (the interactive orchestrator \
                     arrives with the chat UI)"
                        .to_string(),
                ));
            }
            dorean::tui::run(&config, cli.resume).await
        }
    }
}

/// Orchestrated pipeline: pick stack, write the plan, run sub-agents in
/// parallel, print events as they stream.
async fn run_orchestrated(
    config: &Config,
    message: &str,
    cli: &Cli,
    cwd: &std::path::Path,
    abort: AbortHandle,
) -> Result<(), DoreanError> {
    let (tx, _) = broadcast::channel::<AgentEvent>(256);
    let mut rx = tx.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            eprintln!("{}", event.render());
        }
    });

    let choice = if let Some(stack) = &cli.stack {
        StackChoice::Named(stack.clone())
    } else if cli.auto_stack {
        StackChoice::Auto
    } else {
        StackChoice::Prompt
    };

    let orchestrator = Orchestrator::new(
        config.clone(),
        cwd.to_path_buf(),
        abort,
        tx,
        cli.resume,
        config.sub_agent_rounds,
    );

    let summary = orchestrator.run(message, &choice, None).await?;
    print!("{}", dorean::agent::orchestrator::render_summary(&summary));
    Ok(())
}
