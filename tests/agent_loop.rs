//! End-to-end agent-loop tests: tool-call round-trip, resume, abort.

use std::fs;

use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

use dorean::agent::{AbortHandle, RunOptions};
use dorean::config::{Config, Provider};
use dorean::history;

/// Matches requests whose body contains the substring.
struct BodyContains(&'static str);

impl Match for BodyContains {
    fn matches(&self, request: &Request) -> bool {
        String::from_utf8_lossy(&request.body).contains(self.0)
    }
}

/// Matches requests whose body does NOT contain the substring.
struct NotBodyContains(&'static str);

impl Match for NotBodyContains {
    fn matches(&self, request: &Request) -> bool {
        !String::from_utf8_lossy(&request.body).contains(self.0)
    }
}

/// First model turn: an assistant tool call to `read` a file.
const TOOL_CALL_BODY: &str = concat!(
    "data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"notes.txt\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
    "data: [DONE]\n\n",
);

/// Second model turn: a final answer (no tool calls).
const FINAL_BODY: &str = concat!(
    "data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":\"Done: the file says hello world\",\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
    "data: [DONE]\n\n",
);

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("dorean-loop-{tag}-{}", std::process::id()));
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

/// First request is the fresh user prompt: it must send tools and have no
/// tool message yet. Second request carries the tool output back.
async fn mount_round_trip_mocks(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(NotBodyContains("\"role\":\"tool\""))
        .and(body_string_contains("\"tools\""))
        .respond_with(ResponseTemplate::new(200).set_body_string(TOOL_CALL_BODY))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(BodyContains("\"role\":\"tool\""))
        .and(body_string_contains("hello world"))
        .respond_with(ResponseTemplate::new(200).set_body_string(FINAL_BODY))
        .mount(server)
        .await;
}

#[tokio::test]
async fn agent_loop_runs_tool_round_trip() {
    let server = MockServer::start().await;
    mount_round_trip_mocks(&server).await;

    let dir = temp_dir("rt");
    fs::write(dir.join("notes.txt"), "hello world\n").unwrap();

    let options = RunOptions {
        message: "read the file",
        print: false,
        resume: false,
        cwd: dir.clone(),
        abort: AbortHandle::new(),
    };

    let summary = dorean::agent::run_once(&config(&server), &options)
        .await
        .unwrap();
    assert_eq!(summary.response, "Done: the file says hello world");
    assert_eq!(summary.turns, 2);

    // The session was persisted: user, assistant(tool call), tool, assistant(final).
    let latest = history::load_latest(&dir).unwrap().unwrap();
    assert_eq!(latest.messages.len(), 4);

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn resume_continues_last_session() {
    let server = MockServer::start().await;
    mount_round_trip_mocks(&server).await;

    let dir = temp_dir("resume");
    fs::write(dir.join("notes.txt"), "hello world\n").unwrap();

    let options = RunOptions {
        message: "first message",
        print: false,
        resume: false,
        cwd: dir.clone(),
        abort: AbortHandle::new(),
    };
    dorean::agent::run_once(&config(&server), &options)
        .await
        .unwrap();

    // Resuming reuses the stored conversation (which contains a tool message),
    // so the request matches the second mock and yields the final answer again.
    let options = RunOptions {
        message: "second message",
        print: false,
        resume: true,
        cwd: dir.clone(),
        abort: AbortHandle::new(),
    };
    let summary = dorean::agent::run_once(&config(&server), &options)
        .await
        .unwrap();
    assert_eq!(summary.response, "Done: the file says hello world");

    let latest = history::load_latest(&dir).unwrap().unwrap();
    assert_eq!(latest.messages.len(), 6);
    assert_eq!(latest.messages[4].content, "second message");

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn abort_stops_before_first_turn() {
    let server = MockServer::start().await;
    let dir = temp_dir("abort");
    let abort = AbortHandle::new();
    abort.abort();

    let options = RunOptions {
        message: "never mind",
        print: false,
        resume: false,
        cwd: dir.clone(),
        abort,
    };
    let summary = dorean::agent::run_once(&config(&server), &options)
        .await
        .unwrap();
    assert!(summary.aborted);
    assert_eq!(summary.turns, 0);
    assert!(summary.response.is_empty());

    let _ = fs::remove_dir_all(&dir);
}
