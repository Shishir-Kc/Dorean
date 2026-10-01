//! Context compaction: prune stale tool output, then summarize when needed.
//!
//! Mirrors the Codex/Claude/DeepSeek-harness shape without a network round
//! trip on the hot path:
//! 1. [`prune_tool_results`] snips old tool messages to a tail budget.
//! 2. [`needs_compaction`] decides from a token estimate vs a threshold.
//! 3. [`compact_messages`] replaces the head with a deterministic summary
//!    block, always keeping the trailing window (never splits the last
//!    assistant tool-call / tool-result pair).

use crate::providers::client::{Message, Role};

/// Keep at most this many chars per old tool message.
pub const PRUNE_PER_MESSAGE_CHARS: usize = 8_000;
/// Always keep at least this many trailing messages verbatim.
pub const KEEP_TAIL_MESSAGES: usize = 10;
/// Default compaction threshold (estimated tokens).
pub const DEFAULT_COMPACT_THRESHOLD: u64 = 100_000;

/// Snip old tool outputs to a per-message budget. The trailing window is left
/// intact so the model keeps full fidelity on recent work.
pub fn prune_tool_results(messages: &mut [Message]) {
    if messages.len() <= KEEP_TAIL_MESSAGES {
        return;
    }
    let split = messages.len() - KEEP_TAIL_MESSAGES;
    for msg in &mut messages[..split] {
        if msg.role == Role::Tool && msg.content.len() > PRUNE_PER_MESSAGE_CHARS {
            let mut snipped = msg.content[..PRUNE_PER_MESSAGE_CHARS].to_string();
            snipped.push_str("\n… [older tool output snipped; tail kept verbatim]");
            msg.content = snipped;
        }
    }
}

/// Whether estimated tokens exceed the threshold.
pub fn needs_compaction(estimated_tokens: u64, threshold: u64) -> bool {
    estimated_tokens >= threshold
}

/// Compact the head of `messages` into a summary block, keeping the tail.
/// Returns true when a compaction was applied.
pub fn compact_messages(messages: &mut Vec<Message>, keep_tail: usize) -> bool {
    if messages.len() <= keep_tail + 2 {
        return false;
    }
    let split = messages.len() - keep_tail;
    // Never split an assistant tool-call / tool-result pair: walk the split
    // back over trailing tool messages whose assistant call would be cut.
    let mut cut = split;
    while cut > 0 && messages[cut].role == Role::Tool {
        cut -= 1;
    }
    if cut == 0 {
        return false;
    }
    let dropped = cut;
    let mut turns = 0usize;
    let mut tools = 0usize;
    for m in &messages[..dropped] {
        match m.role {
            Role::User => turns += 1,
            Role::Tool => tools += 1,
            _ => {}
        }
    }
    let summary = format!(
        "[compaction: earlier context summarized — {turns} user turn(s), {tools} tool result(s) \
         condensed. Key decisions and file changes above are preserved in working-tree state; \
         continue from the conversation below.]"
    );
    let tail = messages[cut..].to_vec();
    messages.clear();
    messages.push(Message::user(summary));
    messages.extend(tail);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_msg(content: &str) -> Message {
        Message::tool("call_1", content)
    }

    #[test]
    fn prunes_old_tool_output_keeps_tail() {
        let mut msgs: Vec<Message> = (0..12).map(|i| tool_msg(&"x".repeat(9000 + i))).collect();
        prune_tool_results(&mut msgs);
        assert!(msgs[0].content.contains("snipped"));
        assert_eq!(msgs[11].content.len(), 9011);
    }

    #[test]
    fn threshold_check() {
        assert!(needs_compaction(100_000, 100_000));
        assert!(!needs_compaction(99_999, 100_000));
    }

    #[test]
    fn compacts_head_keeps_tail() {
        let mut msgs = vec![
            Message::user("first"),
            tool_msg("out1"),
            Message::user("second"),
            tool_msg("out2"),
            Message::user("third"),
        ];
        assert!(compact_messages(&mut msgs, 2));
        assert!(msgs[0].content.starts_with("[compaction:"));
        // cut walks back over the trailing tool message so the assistant
        // tool-call / result pair is never split: summary + 3 tail.
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs.last().unwrap().content, "third");
    }

    #[test]
    fn refuses_to_compact_tiny_logs() {
        let mut msgs = vec![Message::user("hi")];
        assert!(!compact_messages(&mut msgs, 10));
    }
}
