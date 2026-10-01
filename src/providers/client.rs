//! Shared OpenAI-compatible streaming client and SSE decoder.
//!
//! The OpenRouter adapter speaks the OpenAI Chat Completions format, so the
//! request/response types and the streaming plumbing live here once.
//! `stream_chat` returns a [`Stream`] of [`CompletionEvent`]s decoded from the
//! wire; caller decides how to consume them.

use std::pin::Pin;

use futures_util::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::error::DoreanError;

/// A message in a chat conversation, in OpenAI Chat Completions shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: content.into(),
            name: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        let content = content.into();
        Message {
            role: Role::Assistant,
            content,
            name: None,
            tool_call_id: None,
            tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        }
    }

    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Message {
            role: Role::Tool,
            content: content.into(),
            name: None,
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Message {
            role: Role::System,
            content: content.into(),
            name: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A completed tool call (used to round-trip assistant tool calls back in).
///
/// Serializes to the OpenAI wire shape `{id, type: "function", function:
/// {name, arguments: "<json string>"}}`; deserialization accepts the same.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

impl Serialize for ToolCall {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let mut state = serializer.serialize_struct("ToolCall", 3)?;
        state.serialize_field("id", &self.id)?;
        state.serialize_field("type", "function")?;
        state.serialize_field(
            "function",
            &ToolCallFunctionWire {
                name: &self.name,
                arguments: self.arguments.to_string(),
            },
        )?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for ToolCall {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawToolCall {
            #[serde(default)]
            id: String,
            #[serde(default)]
            function: Option<RawToolCallFunction>,
        }

        #[derive(Default, Deserialize)]
        struct RawToolCallFunction {
            #[serde(default)]
            name: String,
            #[serde(default)]
            arguments: String,
        }

        let raw = RawToolCall::deserialize(deserializer)?;
        let function = raw.function.unwrap_or_default();
        let arguments =
            serde_json::from_str(&function.arguments).unwrap_or(serde_json::Value::Null);
        Ok(ToolCall {
            id: raw.id,
            name: function.name,
            arguments,
        })
    }
}

#[derive(Serialize)]
struct ToolCallFunctionWire<'a> {
    name: &'a str,
    arguments: String,
}

/// A tool offered to the model, in OpenAI `tools` wire shape.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    #[serde(rename = "type")]
    pub tool_type: String,
    #[serde(rename = "function")]
    pub function: ToolFunctionSpec,
}

/// The `function` half of a [`ToolSpec`].
#[derive(Debug, Clone, Serialize)]
pub struct ToolFunctionSpec {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

impl ToolSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        ToolSpec {
            tool_type: "function".to_string(),
            function: ToolFunctionSpec {
                name: name.into(),
                description: description.into(),
                parameters: Some(parameters),
            },
        }
    }
}

/// A streaming request to `/chat/completions`.
#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub tools: Vec<ToolSpec>,
}

impl ChatRequest {
    pub fn to_json(&self) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": self.messages,
            "stream": true,
            // Ask providers to include final usage (tokens + cache hits) in the
            // stream. Ignored by servers that don't support it.
            "stream_options": { "include_usage": true },
        });
        if let Some(max_tokens) = self.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(temperature) = self.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }
        if !self.tools.is_empty() {
            body["tools"] = serde_json::json!(self.tools);
        }
        body
    }
}

/// Accumulate streamed [`ToolCallDelta`] fragments (grouped by index) into
/// completed [`ToolCall`]s, in the order the model emitted them.
pub fn accumulate_tool_calls(deltas: &[ToolCallDelta]) -> Vec<ToolCall> {
    use std::collections::BTreeMap;

    let mut by_index: BTreeMap<usize, (Option<String>, Option<String>, String)> = BTreeMap::new();
    for delta in deltas {
        let entry = by_index.entry(delta.index).or_default();
        if let Some(id) = &delta.id {
            entry.0 = Some(id.clone());
        }
        if let Some(name) = &delta.name {
            entry.1 = Some(name.clone());
        }
        if let Some(arguments) = &delta.arguments {
            entry.2.push_str(arguments);
        }
    }

    by_index
        .into_iter()
        .map(|(_, (id, name, args))| {
            let arguments = serde_json::from_str(&args).unwrap_or(serde_json::Value::Null);
            ToolCall {
                id: id.unwrap_or_default(),
                name: name.unwrap_or_default(),
                arguments,
            }
        })
        .collect()
}

/// Token usage reported by the provider in the final stream chunk.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub prompt_tokens_details: Option<PromptTokensDetails>,
    pub completion_tokens_details: Option<CompletionTokensDetails>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PromptTokensDetails {
    pub cached_tokens: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CompletionTokensDetails {
    pub reasoning_tokens: u64,
}

/// A fragment of a streamed tool call. `arguments` is a partial JSON string
/// that must be accumulated across deltas by `index`.
#[derive(Debug, Clone)]
pub struct ToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

/// One decoded event from a streaming chat completion.
#[derive(Debug, Clone)]
pub enum CompletionEvent {
    /// A fragment of the assistant's answer text.
    TextDelta(String),
    /// A fragment of the model's reasoning (reasoning models only).
    ReasoningDelta(String),
    /// A fragment of a streamed tool call; accumulate by [`ToolCallDelta::index`].
    ToolCallDelta(ToolCallDelta),
    /// Final usage, sent once before the stream ends.
    Usage(Usage),
    /// End of stream (`data: [DONE]`).
    Done,
}

/// Metadata about a model advertised by a provider.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub context_length: Option<u64>,
    pub is_free: bool,
    pub prompt_price: f64,
    pub completion_price: f64,
}

/// A single Server-Sent Event as defined by the SSE spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
    pub id: Option<String>,
}

/// What [`SseDecoder::push`] produced from the bytes it consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseChunk {
    Event(SseEvent),
    /// The `data: [DONE]` terminator.
    Done,
}

/// Incremental SSE decoder.
///
/// Feeds raw bytes (which may split events arbitrarily at the transport
/// level) and returns complete events once their terminating blank line
/// arrives. Per the SSE spec it ignores comment lines (`: ...`), joins
/// multi-line `data:` fields with `\n`, and accepts LF or CRLF line endings.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
    data_lines: Vec<String>,
    event: Option<String>,
    id: Option<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes and collect any complete events produced.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseChunk> {
        self.buffer.extend_from_slice(bytes);
        let mut chunks = Vec::new();
        while let Some(nl) = self.buffer.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=nl).collect();
            line.pop(); // strip \n
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.feed_line(&line, &mut chunks);
        }
        chunks
    }

    fn feed_line(&mut self, line: &[u8], chunks: &mut Vec<SseChunk>) {
        if line.is_empty() {
            self.dispatch(chunks);
            return;
        }
        if line.first() == Some(&b':') {
            return; // comment / keep-alive
        }
        let (field, value) = match line.iter().position(|&b| b == b':') {
            Some(i) => {
                let mut value = &line[i + 1..];
                if value.first() == Some(&b' ') {
                    value = &value[1..];
                }
                (&line[..i], value)
            }
            None => (line, &[][..]),
        };
        match field {
            b"data" => self
                .data_lines
                .push(String::from_utf8_lossy(value).into_owned()),
            b"event" => self.event = Some(String::from_utf8_lossy(value).into_owned()),
            b"id" => self.id = Some(String::from_utf8_lossy(value).into_owned()),
            _ => {} // ignore unknown fields
        }
    }

    fn dispatch(&mut self, chunks: &mut Vec<SseChunk>) {
        let data = self.data_lines.join("\n");
        self.data_lines.clear();
        let event = self.event.take();
        let id = self.id.take();
        if data.is_empty() {
            return; // keep-alive comment or stray blank line with no payload
        }
        if data == "[DONE]" {
            chunks.push(SseChunk::Done);
        } else {
            chunks.push(SseChunk::Event(SseEvent {
                event: event.unwrap_or_default(),
                data,
                id,
            }));
        }
    }
}

/// Stream a chat completion from a provider.
///
/// Returns a boxed stream that yields [`CompletionEvent`]s. Transport and
/// HTTP-status errors are surfaced as the first `Err` item so the stream can
/// be consumed uniformly.
pub fn stream_chat(
    http: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    body: serde_json::Value,
) -> Pin<Box<dyn Stream<Item = Result<CompletionEvent, DoreanError>> + Send>> {
    Box::pin(async_stream::stream! {
        let mut request = http.post(&url).json(&body);
        for (key, value) in headers {
            request = request.header(key, value);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(e) => {
                yield Err(DoreanError::Network(e));
                return;
            }
        };
        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            yield Err(DoreanError::Provider {
                status: status.as_u16(),
                message,
            });
            return;
        }

        let mut decoder = SseDecoder::new();
        let mut bytes = response.bytes_stream();
        while let Some(chunk) = bytes.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(e) => {
                    yield Err(DoreanError::Network(e));
                    return;
                }
            };
            for sse in decoder.push(&chunk) {
                match sse {
                    SseChunk::Done => {
                        yield Ok(CompletionEvent::Done);
                        return;
                    }
                    SseChunk::Event(event) => match parse_event(&event.data) {
                        Ok(events) => {
                            for event in events {
                                yield Ok(event);
                            }
                        }
                        Err(e) => {
                            yield Err(DoreanError::Stream(e));
                            return;
                        }
                    },
                }
            }
        }
        yield Ok(CompletionEvent::Done);
    })
}

/// Decode a single `data:` payload into [`CompletionEvent`]s, tolerating
/// OpenRouter's SSE quirks (error objects, empty chunks, usage-only chunks).
fn parse_event(data: &str) -> Result<Vec<CompletionEvent>, String> {
    let chunk: ChatChunk =
        serde_json::from_str(data).map_err(|e| format!("invalid stream chunk `{data}`: {e}"))?;
    let ChatChunk {
        choices,
        usage,
        error,
    } = chunk;

    if let Some(error) = error.or_else(|| choices.iter().find_map(|choice| choice.error.clone())) {
        let message = error
            .message
            .unwrap_or_else(|| "unknown provider error".to_string());
        return Err(message);
    }

    let mut events = Vec::new();
    if let Some(first) = choices.into_iter().next() {
        let delta = first.delta;
        if let Some(text) = delta.content.filter(|s| !s.is_empty()) {
            events.push(CompletionEvent::TextDelta(text));
        }
        if let Some(reasoning) = delta.reasoning.filter(|s| !s.is_empty()) {
            events.push(CompletionEvent::ReasoningDelta(reasoning));
        }
        for (i, call) in delta.tool_calls.into_iter().enumerate() {
            events.push(CompletionEvent::ToolCallDelta(ToolCallDelta {
                index: call.index.unwrap_or(i),
                id: call.id,
                name: call.function.as_ref().and_then(|f| f.name.clone()),
                arguments: call.function.as_ref().and_then(|f| f.arguments.clone()),
            }));
        }
    }
    if let Some(usage) = usage {
        events.push(CompletionEvent::Usage(usage));
    }

    Ok(events)
}

#[derive(Debug, Deserialize)]
struct ChatChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<Usage>,
    error: Option<ChunkError>,
}

#[derive(Debug, Default, Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    delta: ChunkDelta,
    error: Option<ChunkError>,
}

#[derive(Debug, Default, Deserialize)]
struct ChunkDelta {
    content: Option<String>,
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ChunkToolCall>,
}

#[derive(Debug, Deserialize)]
struct ChunkToolCall {
    index: Option<usize>,
    id: Option<String>,
    function: Option<ChunkToolCallFunction>,
}

#[derive(Debug, Deserialize)]
struct ChunkToolCallFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChunkError {
    message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(input: &str) -> Vec<SseChunk> {
        SseDecoder::new().push(input.as_bytes())
    }

    fn data_of(chunk: &SseChunk) -> &str {
        match chunk {
            SseChunk::Event(e) => &e.data,
            SseChunk::Done => "[DONE]",
        }
    }

    #[test]
    fn decodes_single_event() {
        let chunks = decode("data: hello world\n\n");
        assert_eq!(chunks.len(), 1);
        assert_eq!(data_of(&chunks[0]), "hello world");
    }

    #[test]
    fn decodes_multiple_events() {
        let chunks = decode("data: one\n\ndata: two\n\n");
        assert_eq!(chunks.len(), 2);
        assert_eq!(data_of(&chunks[0]), "one");
        assert_eq!(data_of(&chunks[1]), "two");
    }

    #[test]
    fn handles_crlf() {
        let chunks = decode("data: crlf\r\n\r\n");
        assert_eq!(chunks.len(), 1);
        assert_eq!(data_of(&chunks[0]), "crlf");
    }

    #[test]
    fn skips_comment_lines() {
        let chunks = decode(": OPENROUTER PROCESSING\n\ndata: real\n\n");
        assert_eq!(chunks.len(), 1);
        assert_eq!(data_of(&chunks[0]), "real");
    }

    #[test]
    fn joins_multiline_data_with_newline() {
        let chunks = decode("data: line1\ndata: line2\n\n");
        assert_eq!(chunks.len(), 1);
        assert_eq!(data_of(&chunks[0]), "line1\nline2");
    }

    #[test]
    fn handles_done_terminator() {
        let chunks = decode("data: [DONE]\n\n");
        assert_eq!(chunks, vec![SseChunk::Done]);
    }

    #[test]
    fn handles_events_split_across_pushes() {
        let mut decoder = SseDecoder::new();
        let mut all = Vec::new();
        for byte in b"data: split across pushes\n\n" {
            all.extend(decoder.push(&[*byte]));
        }
        assert_eq!(all.len(), 1);
        assert_eq!(data_of(&all[0]), "split across pushes");
    }

    #[test]
    fn parses_event_and_id_fields() {
        let chunks = decode("event: message\nid: 42\ndata: payload\n\n");
        match &chunks[0] {
            SseChunk::Event(e) => {
                assert_eq!(e.event, "message");
                assert_eq!(e.id.as_deref(), Some("42"));
                assert_eq!(e.data, "payload");
            }
            _ => panic!("expected event"),
        }
    }

    #[test]
    fn parses_content_delta() {
        let json = r#"{"id":"x","choices":[{"delta":{"content":"Hel","role":"assistant"},"finish_reason":null}]}"#;
        match parse_event(json).unwrap().as_slice() {
            [CompletionEvent::TextDelta(t)] => assert_eq!(t, "Hel"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_reasoning_delta() {
        let json = r#"{"choices":[{"delta":{"reasoning":"think step by step"}}]}"#;
        match parse_event(json).unwrap().as_slice() {
            [CompletionEvent::ReasoningDelta(r)] => assert_eq!(r, "think step by step"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_tool_call_delta() {
        let json = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"path\":"}}]}}]}"#;
        match parse_event(json).unwrap().as_slice() {
            [CompletionEvent::ToolCallDelta(delta)] => {
                assert_eq!(delta.index, 0);
                assert_eq!(delta.id.as_deref(), Some("call_1"));
                assert_eq!(delta.name.as_deref(), Some("read"));
                assert_eq!(delta.arguments.as_deref(), Some(r#"{"path":"#));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_usage_only_chunk() {
        let json = r#"{"id":"x","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15,"completion_tokens_details":{"reasoning_tokens":3}}}"#;
        match parse_event(json).unwrap().as_slice() {
            [CompletionEvent::Usage(u)] => {
                assert_eq!(u.total_tokens, 15);
                assert_eq!(
                    u.completion_tokens_details
                        .as_ref()
                        .unwrap()
                        .reasoning_tokens,
                    3
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_combined_delta_and_usage_chunk() {
        let json = r#"{"choices":[{"delta":{"content":"done"}}],"usage":{"total_tokens":9}}"#;
        match parse_event(json).unwrap().as_slice() {
            [CompletionEvent::TextDelta(_), CompletionEvent::Usage(_)] => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn surfaces_stream_error_event() {
        let json = r#"{"error":{"message":"upstream rate limited"},"choices":[{"delta":{},"finish_reason":"error"}]}"#;
        assert_eq!(parse_event(json).unwrap_err(), "upstream rate limited");
    }

    #[test]
    fn rejects_non_json_data() {
        assert!(parse_event("not json at all").is_err());
    }

    #[test]
    fn empty_chunk_yields_nothing() {
        assert!(
            parse_event(r#"{"id":"x","choices":[],"usage":null}"#)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn accumulates_tool_call_deltas_by_index() {
        let deltas = vec![
            ToolCallDelta {
                index: 0,
                id: Some("call_1".to_string()),
                name: Some("read".to_string()),
                arguments: Some(r#"{"path":""#.to_string()),
            },
            ToolCallDelta {
                index: 1,
                id: Some("call_2".to_string()),
                name: Some("bash".to_string()),
                arguments: Some(r#"{"command":"ls"}"#.to_string()),
            },
            ToolCallDelta {
                index: 0,
                id: None,
                name: None,
                arguments: Some(r#"src/main.rs"}"#.to_string()),
            },
        ];

        let calls = accumulate_tool_calls(&deltas);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].arguments["path"], "src/main.rs");
        assert_eq!(calls[1].id, "call_2");
        assert_eq!(calls[1].arguments["command"], "ls");
    }

    #[test]
    fn tool_call_serializes_in_openai_wire_shape() {
        let call = ToolCall {
            id: "call_1".to_string(),
            name: "read".to_string(),
            arguments: serde_json::json!({ "path": "src/main.rs" }),
        };
        let json = serde_json::to_value(&call).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "read",
                    "arguments": r#"{"path":"src/main.rs"}"#,
                }
            })
        );

        // And it round-trips through Deserialize.
        let back: ToolCall = serde_json::from_value(json).unwrap();
        assert_eq!(back.id, "call_1");
        assert_eq!(back.name, "read");
        assert_eq!(back.arguments["path"], "src/main.rs");
    }

    #[test]
    fn arbitrary_binary_garbage_never_panics() {
        let mut decoder = SseDecoder::new();
        let mut rng = 0x0005_eed2u64.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let mut bytes = vec![0u8; 64 * 1024];
        for b in &mut bytes {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *b = (rng >> 33) as u8;
        }
        let chunks = decoder.push(&bytes);
        // Garbage must not fabricate events or panic.
        assert!(
            chunks
                .iter()
                .all(|c| matches!(c, SseChunk::Event(e) if !e.data.is_empty()))
        );
        // And the decoder stays usable for a real event afterwards (the \n
        // closes any partial line the garbage left behind).
        let mut chunks = decoder.push(b"\ndata: after garbage\n\n");
        chunks.push(SseChunk::Done);
        assert!(chunks.len() <= 2);
        assert!(chunks.iter().any(|c| data_of(c) == "after garbage"));
    }

    #[test]
    fn unterminated_stream_emits_nothing() {
        let mut decoder = SseDecoder::new();
        let mut all = decoder.push(b"data: never finished\n");
        all.extend(decoder.push(b"event: message\n"));
        assert!(all.is_empty());
        // Closing the stream with a blank line flushes the pending event.
        let mut all = decoder.push(b"\n");
        all.extend(decoder.push(b"data: [DONE]\n\n"));
        assert_eq!(data_of(&all[0]), "never finished");
        assert_eq!(all[1], SseChunk::Done);
    }

    #[test]
    fn giant_data_line_survives() {
        let payload = "x".repeat(1024 * 1024);
        let chunks = decode(&format!("data: {payload}\n\n"));
        assert_eq!(chunks.len(), 1);
        assert_eq!(data_of(&chunks[0]).len(), 1024 * 1024);
    }

    #[test]
    fn corrupted_json_events_do_not_panic() {
        let samples = [
            "data: {\"choices\":[\n\n",
            "data: {\"choices\":[{\"delta\":{}}}\n\n",
            "data: \u{fffd}\u{fffd} broken utf8\n\n",
            "data: [DONE]\ndata: trailing\n\n",
        ];
        for sample in samples {
            for chunk in decode(sample) {
                match chunk {
                    SseChunk::Event(e) => {
                        let _ = parse_event(&e.data);
                    }
                    SseChunk::Done => {}
                }
            }
        }
    }

    #[test]
    fn chat_request_includes_tools_when_present() {
        let request = ChatRequest {
            model: "m".to_string(),
            messages: vec![Message::user("hi")],
            max_tokens: None,
            temperature: None,
            tools: vec![ToolSpec::new(
                "read",
                "Read a file",
                serde_json::json!({ "type": "object" }),
            )],
        };
        let json = request.to_json();
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["function"]["name"], "read");
    }
}
