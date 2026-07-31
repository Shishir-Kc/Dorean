//! End-to-end orchestration: stack → roster → SPEC → parallel sub-agents →
//! todo merge, driven by a mock provider.
//!
//! Each sub-agent makes two model turns against the mock server: the first
//! yields a `todo` tool call (marks its seeded todo done), the second yields a
//! final answer. Four agents run in parallel, so the mocks see 4+4 requests.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use serde_json::json;
use tokio::sync::broadcast;
use wiremock::matchers::{method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

use dorean::agent::events::AgentEvent;
use dorean::agent::orchestrator::Orchestrator;
use dorean::agent::stack::StackChoice;
use dorean::agent::{AbortHandle, manifest, spec, todos};
use dorean::config::{Config, Provider};

const GOAL: &str = "build a notes app";

/// Matches requests whose body does NOT contain the substring.
struct NotBodyContains(&'static str);

impl Match for NotBodyContains {
    fn matches(&self, request: &Request) -> bool {
        !String::from_utf8_lossy(&request.body).contains(self.0)
    }
}

/// Matches requests whose body contains the substring.
struct BodyContains(&'static str);

impl Match for BodyContains {
    fn matches(&self, request: &Request) -> bool {
        String::from_utf8_lossy(&request.body).contains(self.0)
    }
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dorean-orchestration-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn config(server: &MockServer) -> Config {
    Config {
        provider: Provider::OpenRouter,
        base_url: Some(server.uri()),
        openrouter_api_key: Some("sk-test".to_string()),
        ..Config::default()
    }
}

/// First model turn: call `todo` with status done for the seeded todo title.
fn todo_call_body() -> String {
    let args = json!({ "status": "done", "title": format!("Work toward the user's goal: {GOAL}") });
    let chunk = json!({
        "id": "c1",
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": { "name": "todo", "arguments": args.to_string() }
                }]
            },
            "finish_reason": null
        }]
    });
    format!("data: {}\n\ndata: [DONE]\n\n", chunk)
}

/// Second model turn: final answer, no tool calls.
fn final_body() -> String {
    let chunk = json!({
        "id": "c1",
        "choices": [{
            "delta": { "content": "Done: my part is complete.", "role": "assistant" },
            "finish_reason": null
        }]
    });
    format!("data: {}\n\ndata: [DONE]\n\n", chunk)
}

async fn mount_mocks(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(NotBodyContains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_string(todo_call_body()))
        .expect(4)
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(BodyContains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_string(final_body()))
        .expect(4)
        .mount(server)
        .await;
}

#[tokio::test]
async fn orchestration_runs_parallel_agents_and_merges_todos() {
    let server = MockServer::start().await;
    mount_mocks(&server).await;

    let dir = temp_dir();
    let (tx, mut rx) = broadcast::channel::<AgentEvent>(256);

    let orchestrator = Orchestrator::new(
        config(&server),
        dir.clone(),
        AbortHandle::new(),
        tx,
        false,
        5,
    );

    let summary = orchestrator
        .run(GOAL, &StackChoice::Named("full-stack".to_string()), None)
        .await
        .unwrap();

    // 4 agents ran in parallel, each finishing with its todo done.
    assert_eq!(summary.agents.len(), 4);
    for agent in &summary.agents {
        assert_eq!(agent.status(), "done", "{}", agent.name);
        assert_eq!(agent.todos_done, 1, "{}", agent.name);
        assert_eq!(agent.rounds, 1, "{}", agent.name);
    }
    server.verify().await;

    // Master todos were merged: 4 items, all done.
    let master = todos::read_master(&dir).unwrap();
    assert_eq!(master.len(), 4);
    assert!(master.iter().all(|t| t.status == todos::TodoStatus::Done));

    // Roster and SPEC were written.
    let roster = manifest::load_roster(&dir).unwrap().unwrap();
    assert_eq!(roster.len(), 4);
    assert!(spec::spec_path(&dir).exists());
    assert!(
        spec::spec_path(&dir).exists()
            && fs::read_to_string(spec::spec_path(&dir))
                .unwrap()
                .contains(GOAL)
    );

    // Per-agent sessions were persisted.
    for agent in &summary.agents {
        let path = dir
            .join(".dorean")
            .join("sessions")
            .join(format!("{}.jsonl", agent.name));
        assert!(path.exists(), "missing session for {}", agent.name);
    }

    // Events: plan created, each agent started/finished, todos updated.
    let mut events = HashSet::new();
    while let Ok(event) = rx.try_recv() {
        events.insert(match event {
            AgentEvent::PlanCreated { .. } => "plan",
            AgentEvent::AgentStarted { .. } => "started",
            AgentEvent::AgentFinished { .. } => "finished",
            AgentEvent::TodoUpdated {
                status: todos::TodoStatus::Done,
                ..
            } => "todo-done",
            _ => "other",
        });
    }
    assert!(events.contains("plan"));
    assert!(events.contains("started"));
    assert!(events.contains("finished"));
    assert!(events.contains("todo-done"));

    let _ = fs::remove_dir_all(&dir);
}
