//! Performance/efficiency regression gate (P0 bench harness).
//!
//! Guards the competitive wins without network access:
//! - stable prompt prefix across turns (cache-friendly; the hour-rounded
//!   timestamp and git state live only in the ephemeral suffix);
//! - parallel read-only tool batches complete with correct outputs;
//! - compaction + pruning shrink token estimates on long logs.

use dorean::agent::compact;
use dorean::agent::context::RepoContext;
use dorean::agent::prompts::system_prompt;
use dorean::agent::token_meter::{estimate_messages, estimate_tokens};
use dorean::agent::tools::{ToolContext, ToolRegistry};
use dorean::providers::client::Message;

fn test_specs() -> Vec<dorean::providers::client::ToolSpec> {
    ToolRegistry::builtin().specs()
}

#[test]
fn prompt_prefix_is_stable_across_repo_states() {
    let cwd = std::path::Path::new("/repo");
    let tools = test_specs();
    let clean = RepoContext {
        in_repo: true,
        branch: Some("main".to_string()),
        is_clean: true,
        status: String::new(),
        diff: String::new(),
        commit: Some("abc".to_string()),
    };
    let dirty = RepoContext {
        status: " M src/main.rs".to_string(),
        diff: "diff --git a/src/main.rs".to_string(),
        is_clean: false,
        ..clean.clone()
    };
    let a = system_prompt(cwd, &clean, &tools);
    let b = system_prompt(cwd, &clean, &tools);
    assert_eq!(a, b, "identical inputs must give byte-identical prompts");
    // Different repo states share a long stable prefix (cache hit zone).
    let c = system_prompt(cwd, &dirty, &tools);
    let common = a.bytes().zip(c.bytes()).take_while(|(x, y)| x == y).count();
    assert!(
        common > 2000,
        "stable prefix too short for caching: {common} bytes"
    );
    assert!(
        !a.contains("Current unix timestamp:"),
        "per-second timestamps bust caches"
    );
}

#[tokio::test]
async fn parallel_read_only_batch_returns_all_outputs() {
    let dir = std::env::temp_dir().join(format!("dorean-perf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "alpha").unwrap();
    std::fs::write(dir.join("b.txt"), "beta").unwrap();
    let registry = ToolRegistry::builtin();
    let ctx = ToolContext::new(&dir);
    let calls = [
        dorean::providers::client::ToolCall {
            id: "1".to_string(),
            name: "read".to_string(),
            arguments: serde_json::json!({"path": "a.txt"}),
        },
        dorean::providers::client::ToolCall {
            id: "2".to_string(),
            name: "read".to_string(),
            arguments: serde_json::json!({"path": "b.txt"}),
        },
        dorean::providers::client::ToolCall {
            id: "3".to_string(),
            name: "glob".to_string(),
            arguments: serde_json::json!({"pattern": "*.txt"}),
        },
    ];
    let outputs = futures_util::future::join_all(calls.iter().map(|c| registry.run(&ctx, c))).await;
    assert!(outputs[0].contains("alpha"));
    assert!(outputs[1].contains("beta"));
    assert!(outputs[2].contains("a.txt"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compaction_shrinks_long_logs() {
    let mut messages: Vec<Message> = Vec::new();
    for i in 0..30 {
        messages.push(Message::user(format!("turn {i} do something")));
        messages.push(Message::tool(format!("call-{i}"), "x".repeat(9000)));
    }
    let before = estimate_messages(&messages);
    compact::prune_tool_results(&mut messages);
    let pruned = estimate_messages(&messages);
    assert!(pruned < before, "pruning must shrink estimates");
    assert!(compact::needs_compaction(
        before,
        compact::DEFAULT_COMPACT_THRESHOLD / 10
    ));
    assert!(compact::compact_messages(
        &mut messages,
        compact::KEEP_TAIL_MESSAGES
    ));
    let after = estimate_messages(&messages);
    assert!(after < pruned, "compaction must shrink further");
    assert!(estimate_tokens("abcd") == 1);
}
