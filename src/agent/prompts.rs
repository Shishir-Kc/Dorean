//! System prompt builder (cwd, OS, repo context, tool docs).

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::providers::client::ToolSpec;

use super::context::RepoContext;

/// Build the system prompt for the primary agent.
///
/// Layout is cache-aware: the stable prefix (identity, cwd, platform, rules,
/// tool docs) must not change across turns so providers can hit prompt-cache.
/// Volatile state (timestamp, git status) lives in the ephemeral suffix and in
/// the newest user message instead.
pub fn system_prompt(cwd: &Path, repo: &RepoContext, tools: &[ToolSpec]) -> String {
    let mut prompt = stable_prefix(cwd, tools);
    prompt.push_str(&ephemeral_suffix(repo));
    prompt
}

/// Build the system prompt for a sub-agent working under the orchestrator.
///
/// Includes the agent's identity, its owned paths (write confinement), the
/// SPEC it must implement, and the todo protocol (`todo` tool). Identity and
/// owned paths are part of the stable prefix; repo state is ephemeral.
pub fn agent_prompt(
    cwd: &Path,
    repo: &RepoContext,
    tools: &[ToolSpec],
    name: &str,
    role: &str,
    responsibilities: &[String],
    owned_paths: &[std::path::PathBuf],
) -> String {
    let mut prompt = stable_prefix(cwd, tools);
    prompt.push_str(&format!(
        "## Identity\n\
         You are the `{name}` sub-agent, role: {role}.\n\
         You are one of several agents working in parallel on the same goal, coordinated by an orchestrator.\n\
         Responsibilities:\n{}\n",
        responsibilities
            .iter()
            .map(|r| format!("- {r}\n"))
            .collect::<String>(),
    ));

    prompt.push_str(&format!(
        "## Owned paths\n\
         You may create and modify files ONLY under these paths (absolute, relative to the working directory):\n{}\n\
         Everything else is read-only for you. Read-only tools (read/glob/grep/list) may inspect the whole repo.\n\n",
        owned_paths
            .iter()
            .map(|p| format!("- {}\n", p.display()))
            .collect::<String>(),
    ));

    prompt.push_str(
        "## Plan and todos\n\
         - The orchestrator's plan lives in `.dorean/SPEC.md`; read it and follow it.\n\
         - Your task list lives in `.dorean/todos/<your name>.md`. Work through the todo items.\n\
         - When you finish a todo item, mark it done by calling the `todo` tool with `status: \"done\"`.\n\
         - Verify your work (build, tests) before marking a todo done.\n\
         - When all your todos are done, give a final summary.\n\n",
    );

    // stable_prefix already contains rules + tools; identity/owned/plan are the
    // only per-agent variance before the ephemeral suffix.
    prompt.push_str(&ephemeral_suffix(repo));
    prompt
}

/// Stable, cache-friendly prefix: identity + platform + rules + tool docs.
/// Must not contain timestamps, git status, or anything that changes per turn.
/// Tool docs are sorted by name so registry order never busts the cache.
fn stable_prefix(cwd: &Path, tools: &[ToolSpec]) -> String {
    let mut prompt = format!(
        "You are Dorean, an autonomous coding agent running on Linux.\n\
         You work directly in the user's repository and get things done:\n\
         inspect the code, make edits, run builds and tests, and report results.\n\n\
         - Working directory: {}\n\
         - Platform: {} ({})\n\
         - Shell: sh (POSIX)\n\n",
        cwd.display(),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    prompt.push_str(&rules_section());
    prompt.push_str(&tools_section(tools));
    prompt
}

/// Volatile suffix: timestamp (hour-rounded for cache stability) + repo state.
/// Kept after the stable prefix so providers can prefix-cache everything above.
fn ephemeral_suffix(repo: &RepoContext) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Round to the hour: per-second timestamps bust prompt caches every turn.
    let hour = now / 3600 * 3600;
    let mut text = format!("## Session\n- Current unix time (hour-rounded): {hour}\n\n");
    text.push_str(&git_section(repo));
    text
}

fn git_section(repo: &RepoContext) -> String {
    if !repo.in_repo {
        return String::new();
    }
    let mut text = String::from("## Repository state\n");
    if let Some(branch) = &repo.branch {
        text.push_str(&format!("- Branch: {branch}\n"));
    }
    text.push_str(&format!(
        "- Tree: {}\n",
        if repo.is_clean { "clean" } else { "dirty" }
    ));
    if !repo.status.is_empty() {
        text.push_str("- Working-tree changes:\n");
        text.push_str(&format!("```\n{}\n```\n", repo.status));
    }
    if !repo.diff.is_empty() {
        text.push_str("- Recent diff summary:\n");
        text.push_str(&format!("```\n{}\n```\n", repo.diff));
    }
    text.push('\n');
    text
}

fn rules_section() -> String {
    "Rules:\n\
     - Investigate before acting; use `list`/`glob`/`grep`/`read` to understand the repo.\n\
     - Modify files with `write`/`edit` rather than shell redirection.\n\
     - Prefer `edit` for surgical changes; keep diffs small.\n\
     - Run tests or a build after making changes when one exists.\n\
     - When a command fails, read the output and adapt; retry rather than giving up.\n\
     - Paths are relative to the working directory unless absolute.\n\
     - When the task is complete, stop calling tools and give a concise final answer\n\
       summarizing what you changed and how to verify it.\n\n"
        .to_string()
}

fn tools_section(tools: &[ToolSpec]) -> String {
    let mut sorted: Vec<&ToolSpec> = tools.iter().collect();
    sorted.sort_by(|a, b| a.function.name.cmp(&b.function.name));
    let mut prompt = String::from("## Tools\n\n");
    for spec in sorted {
        prompt.push_str(&format_tool_doc(spec));
    }
    prompt.push_str(
        "To call a tool, emit a function tool call with arguments as a JSON object.\n\
         Tool results will be returned to you as tool messages; use them to decide the next step.\n",
    );
    prompt
}

fn format_tool_doc(spec: &ToolSpec) -> String {
    let params = spec
        .function
        .parameters
        .as_ref()
        .map(|p| serde_json::to_string_pretty(p).unwrap_or_else(|_| "{}".to_string()))
        .unwrap_or_else(|| "{}".to_string());
    format!(
        "### {name}\n{description}\nInput schema:\n```json\n{params}\n```\n\n",
        name = spec.function.name,
        description = spec.function.description,
    )
}
