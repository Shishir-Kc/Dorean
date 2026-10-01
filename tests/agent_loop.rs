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
        permission_mode: Some(dorean::permissions::PermissionMode::Allow),
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
async fn stalled_stream_fails_instead_of_hanging_forever() {
    let server = MockServer::start().await;
    // Stall headers+body past any reasonable wait: without an idle watchdog
    // this test would hang as long as the production TUI did.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(FINAL_BODY)
                .set_delay(std::time::Duration::from_secs(5)),
        )
        .mount(&server)
        .await;

    let dir = temp_dir("stall");
    let mut agent = dorean::agent::AgentLoop::with_cwd(&config(&server), &dir).unwrap();
    agent.set_stream_idle_timeout(std::time::Duration::from_millis(100));
    let err = agent.run("hi").await.unwrap_err();
    assert!(err.to_string().contains("stalled"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
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

#[tokio::test]
async fn abort_mid_stream_keeps_partial_text() {
    // Raw TCP mock: one text delta immediately, then silence (never DONE).
    // This is the production wedge: without abort-aware polling the loop
    // would hang here forever and never notice the abort.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 8192];
        let _ = socket.read(&mut buf).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n")
            .await
            .unwrap();
        socket
            .write_all(b"data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n")
            .await
            .unwrap();
        socket.flush().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    });

    let dir = temp_dir("abort-mid");
    let abort = AbortHandle::new();
    let config = Config {
        provider: Provider::OpenRouter,
        base_url: Some(format!("http://{addr}")),
        openrouter_api_key: Some("sk-test".to_string()),
        permission_mode: Some(dorean::permissions::PermissionMode::Allow),
        ..Config::default()
    };
    let options = RunOptions {
        message: "tell me",
        print: false,
        resume: false,
        cwd: dir.clone(),
        abort: abort.clone(),
    };
    let later_abort = abort.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        later_abort.abort();
    });
    // Abort lands within ~1s of the stall and the partial text survives.
    let summary = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        dorean::agent::run_once(&config, &options),
    )
    .await
    .expect("abort must resolve promptly on a stalled stream")
    .unwrap();
    assert!(summary.aborted);
    assert_eq!(summary.turns, 1);
    assert_eq!(summary.response, "hello");

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn abort_during_tool_execution_stops_next_turn() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("\"tools\""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"bash\",\"arguments\":\"{\\\"command\\\":\\\"sleep 2\\\"}\"}}]},\"finish_reason\":null}]}\n\n\
             data: [DONE]\n\n",
        ))
        .mount(&server)
        .await;

    let dir = temp_dir("abort-tool");
    let abort = AbortHandle::new();
    let options = RunOptions {
        message: "run it",
        print: false,
        resume: false,
        cwd: dir.clone(),
        abort: abort.clone(),
    };
    let later_abort = abort.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        later_abort.abort();
    });
    // Abort while the tool is still running; the tool finishes, then the next
    // turn boundary honors the abort.
    let summary = dorean::agent::run_once(&config(&server), &options)
        .await
        .unwrap();
    assert!(summary.aborted);
    assert_eq!(summary.turns, 1);
    assert!(summary.response.is_empty());

    let _ = fs::remove_dir_all(&dir);
}
