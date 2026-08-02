//! The multiline text input with history, wrapping, bracketed-paste support
//! and word navigation. Cursor is tracked as a byte offset; visual lines are
//! recomputed on demand (hard wrap at the input width).

use std::collections::VecDeque;

/// A snapshot of the buffer taken before an editing step (undo/redo).
#[derive(Debug, Clone, PartialEq, Eq)]
struct UndoEntry {
    text: String,
    cursor: usize,
}

/// The multiline input editor.
#[derive(Debug, Clone, Default)]
pub struct Input {
    text: String,
    /// Byte offset of the cursor.
    cursor: usize,
    /// Submitted lines (oldest first), bounded by [`Input::HISTORY_MAX`].
    history: VecDeque<String>,
    /// When navigating history, the text is temporarily replaced; this is the
    /// index into `history` (0 = oldest). `None` = editing normally.
    history_index: Option<usize>,
    /// Preserved draft while browsing history.
    draft: String,
    /// Undo/redo stacks of editing snapshots (Ctrl+Z / Ctrl+Y).
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
}

impl Input {
    pub const HISTORY_MAX: usize = 200;
    const UNDO_MAX: usize = 200;

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Reset to empty.
    pub fn clear(&mut self) {
        if !self.text.is_empty() {
            self.snapshot();
        }
        self.text.clear();
        self.cursor = 0;
        self.history_index = None;
        self.draft.clear();
    }

    /// Replace the whole text (e.g. pasted command or set by a selector).
    pub fn set_text(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.len();
        self.history_index = None;
    }

    /// Place the cursor at a byte offset (clamped to the text length).
    pub fn set_cursor(&mut self, byte: usize) {
        self.cursor = byte.min(self.text.len());
    }

    fn char_index(&self) -> usize {
        self.text[..self.cursor].chars().count()
    }

    fn char_at(&self, byte: usize) -> Option<char> {
        self.text[byte..].chars().next()
    }

    // --- Editing -----------------------------------------------------------

    pub fn push_char(&mut self, c: char) {
        if c == '\r' {
            self.insert_str("\n");
            return;
        }
        self.insert_str(&c.to_string());
    }

    pub fn insert_str(&mut self, s: &str) {
        self.cancel_history();
        if !s.is_empty() {
            self.snapshot();
            self.text.insert_str(self.cursor, s);
            self.cursor += s.len();
        }
    }

    pub fn backspace(&mut self) {
        self.cancel_history();
        if self.cursor == 0 {
            return;
        }
        self.snapshot();
        let start = prev_boundary(&self.text, self.cursor);
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }

    pub fn delete_forward(&mut self) {
        self.cancel_history();
        if self.cursor >= self.text.len() {
            return;
        }
        self.snapshot();
        let end = next_boundary(&self.text, self.cursor);
        self.text.drain(self.cursor..end);
    }

    /// Delete the word immediately before the cursor (Ctrl+W).
    pub fn delete_word_left(&mut self) {
        self.cancel_history();
        self.snapshot();
        let end = self.cursor;
        let mut idx = end;
        while idx > 0
            && self.text[..idx]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace)
        {
            idx = prev_boundary(&self.text, idx);
        }
        while idx > 0
            && self.text[..idx]
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_whitespace())
        {
            idx = prev_boundary(&self.text, idx);
        }
        self.text.drain(idx..end);
        self.cursor = idx;
    }

    pub fn newline(&mut self) {
        self.cancel_history();
        self.insert_str("\n");
    }

    // --- Undo / redo -------------------------------------------------------

    /// Snapshot the buffer for undo. Skips no-op duplicates (e.g. backspace at
    /// a word boundary after delete_word_left) and bounds the stack.
    fn snapshot(&mut self) {
        if self
            .undo
            .last()
            .is_some_and(|entry| entry.text == self.text && entry.cursor == self.cursor)
        {
            return;
        }
        self.undo.push(UndoEntry {
            text: self.text.clone(),
            cursor: self.cursor,
        });
        if self.undo.len() > Self::UNDO_MAX {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Undo the last editing step (Ctrl+Z). Returns true when something was
    /// undone.
    pub fn undo(&mut self) -> bool {
        let Some(entry) = self.undo.pop() else {
            return false;
        };
        self.redo.push(UndoEntry {
            text: self.text.clone(),
            cursor: self.cursor,
        });
        self.text = entry.text;
        self.cursor = entry.cursor;
        true
    }

    /// Redo the last undone step (Ctrl+Y). Returns true when something was
    /// redone.
    pub fn redo(&mut self) -> bool {
        let Some(entry) = self.redo.pop() else {
            return false;
        };
        self.undo.push(UndoEntry {
            text: self.text.clone(),
            cursor: self.cursor,
        });
        self.text = entry.text;
        self.cursor = entry.cursor;
        true
    }

    // --- Motion ------------------------------------------------------------

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = prev_boundary(&self.text, self.cursor);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor = next_boundary(&self.text, self.cursor);
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.text.len();
    }

    pub fn move_word_left(&mut self) {
        let mut idx = self.cursor;
        // Skip trailing whitespace.
        while idx > 0
            && self.text[..idx]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace)
        {
            idx = prev_boundary(&self.text, idx);
        }
        // Skip the word.
        while idx > 0
            && self.text[..idx]
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_whitespace())
        {
            idx = prev_boundary(&self.text, idx);
        }
        self.cursor = idx;
    }

    pub fn move_word_right(&mut self) {
        let mut idx = self.cursor;
        // Skip leading whitespace.
        while idx < self.text.len() && self.char_at(idx).is_some_and(char::is_whitespace) {
            idx = next_boundary(&self.text, idx);
        }
        // Skip the word.
        while idx < self.text.len() && self.char_at(idx).is_some_and(|c| !c.is_whitespace()) {
            idx = next_boundary(&self.text, idx);
        }
        // Skip trailing whitespace so the cursor lands at the next word.
        while idx < self.text.len() && self.char_at(idx).is_some_and(char::is_whitespace) {
            idx = next_boundary(&self.text, idx);
        }
        self.cursor = idx;
    }

    /// Move the cursor up one visual line, or step back through history when
    /// already on the first visual line.
    pub fn move_up(&mut self, width: usize) {
        let (line, col) = self.cursor_line_col(width);
        if line > 0 {
            let lines = visual_lines(&self.text, width);
            let target = lines[line - 1];
            self.cursor = byte_for_char(&self.text, target.0 + col.min(target.1));
        } else {
            self.history_prev();
        }
    }

    /// Move the cursor down one visual line, or step forward through history.
    pub fn move_down(&mut self, width: usize) {
        let lines = visual_lines(&self.text, width);
        let (line, col) = self.cursor_line_col(width);
        if line + 1 < lines.len() {
            let target = lines[line + 1];
            self.cursor = byte_for_char(&self.text, target.0 + col.min(target.1));
        } else {
            self.history_next();
        }
    }

    // --- History -----------------------------------------------------------

    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        if self.history_index.is_none() {
            self.draft = self.text.clone();
            self.history_index = Some(self.history.len() - 1);
        } else {
            let idx = self.history_index.unwrap();
            if idx == 0 {
                return;
            }
            self.history_index = Some(idx - 1);
        }
        let idx = self.history_index.unwrap();
        if let Some(entry) = self.history.get(idx) {
            self.text = entry.clone();
            self.cursor = self.text.len();
        }
    }

    pub fn history_next(&mut self) {
        let Some(idx) = self.history_index else {
            return;
        };
        if idx + 1 < self.history.len() {
            self.history_index = Some(idx + 1);
            let entry = &self.history[idx + 1];
            self.text = entry.clone();
            self.cursor = self.text.len();
        } else {
            self.history_index = None;
            self.text = std::mem::take(&mut self.draft);
            self.cursor = self.text.len();
        }
    }

    /// Record a submitted line (skipping empty and duplicate entries).
    pub fn push_history(&mut self, text: String) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.back().is_some_and(|last| last == trimmed) {
            return;
        }
        self.history.push_back(trimmed.to_string());
        if self.history.len() > Self::HISTORY_MAX {
            self.history.pop_front();
        }
    }

    /// Submit: record the current text in history, then return it and clear the
    /// input.
    pub fn submit(&mut self) -> String {
        self.cancel_history();
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.push_history(text.clone());
        text
    }

    fn cancel_history(&mut self) {
        if self.history_index.is_some() {
            self.history_index = None;
            self.draft.clear();
        }
    }

    // --- Layout ------------------------------------------------------------

    /// The visual lines as (start_char_index, char_len) for the given width.
    pub fn visual_lines(&self, width: usize) -> Vec<(usize, usize)> {
        visual_lines(&self.text, width)
    }

    /// The (visual_line, column) of the cursor for the given width.
    pub fn cursor_line_col(&self, width: usize) -> (usize, usize) {
        let lines = visual_lines(&self.text, width);
        let ci = self.char_index();
        let mut acc = 0usize;
        for (i, (_, len)) in lines.iter().enumerate() {
            if ci <= acc + *len {
                return (i, ci - acc);
            }
            acc += *len;
        }
        (lines.len().saturating_sub(1), 0)
    }

    /// Whether the cursor sits on the last visual line (used for Down→history).
    pub fn cursor_on_last_line(&self, width: usize) -> bool {
        let (line, _) = self.cursor_line_col(width);
        line + 1 >= self.visual_lines(width).len()
    }
}

/// Split text into visual lines of at most `width` characters, breaking at
/// newlines. Returns (start_char_index, char_len) per line.
fn visual_lines(text: &str, width: usize) -> Vec<(usize, usize)> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line_start = 0usize; // char index
    let mut line_len = 0usize;
    for (ci, ch) in text.chars().enumerate() {
        if ch == '\n' {
            lines.push((line_start, line_len));
            line_start = ci + 1;
            line_len = 0;
        } else {
            line_len += 1;
            if line_len == width {
                lines.push((line_start, line_len));
                line_start = ci + 1;
                line_len = 0;
            }
        }
    }
    lines.push((line_start, line_len));
    lines
}

fn byte_for_char(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

fn prev_boundary(text: &str, byte: usize) -> usize {
    text[..byte]
        .char_indices()
        .next_back()
        .map(|(b, _)| b)
        .unwrap_or(0)
}

fn next_boundary(text: &str, byte: usize) -> usize {
    text[byte..]
        .char_indices()
        .nth(1)
        .map(|(b, _)| byte + b)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_and_moves() {
        let mut input = Input::default();
        for c in "hello".chars() {
            input.push_char(c);
        }
        input.move_home();
        input.move_right();
        input.push_char('a');
        assert_eq!(input.text(), "haello");
        input.backspace();
        assert_eq!(input.text(), "hello");
    }

    #[test]
    fn history_round_trips() {
        let mut input = Input::default();
        input.set_text("one".to_string());
        assert_eq!(input.submit(), "one");
        input.set_text("two".to_string());
        assert_eq!(input.submit(), "two");

        input.history_prev();
        assert_eq!(input.text(), "two");
        input.history_prev();
        assert_eq!(input.text(), "one");
        input.history_prev();
        assert_eq!(input.text(), "one"); // oldest
        input.history_next();
        assert_eq!(input.text(), "two");
        input.history_next();
        assert_eq!(input.text(), ""); // back to draft
    }

    #[test]
    fn editing_cancels_history() {
        let mut input = Input::default();
        input.set_text("a".to_string());
        input.submit();
        input.history_prev();
        input.push_char('x');
        assert_eq!(input.text(), "ax");
        input.history_next();
        assert_eq!(input.text(), "ax"); // no longer navigating
    }

    #[test]
    fn skips_empty_and_duplicate_history() {
        let mut input = Input::default();
        input.push_history("".to_string());
        input.push_history("hi".to_string());
        input.push_history("hi".to_string());
        assert_eq!(input.history.len(), 1);
    }

    #[test]
    fn visual_line_mapping() {
        let lines = visual_lines("abcdefghij", 4);
        assert_eq!(lines, vec![(0, 4), (4, 4), (8, 2)]);

        let input = Input::default();
        assert_eq!(visual_lines("ab\ncd", 10), vec![(0, 2), (3, 2)]);
        let _ = input;
    }

    #[test]
    fn word_motion() {
        let mut input = Input::default();
        input.set_text("foo bar baz".to_string());
        input.move_home();
        input.move_word_right();
        assert_eq!(input.text()[..input.cursor()].chars().count(), 4); // after "foo "
        input.move_word_right();
        assert_eq!(input.text()[..input.cursor()].chars().count(), 8); // after "bar "
        input.move_word_left();
        assert_eq!(input.text()[..input.cursor()].chars().count(), 4);
    }

    #[test]
    fn unicode_cursor_never_splits_chars() {
        let mut input = Input::default();
        input.set_text("héllo".to_string());
        input.move_home();
        input.move_right();
        input.move_right();
        input.insert_str("x");
        assert_eq!(input.text(), "héxllo");
    }

    #[test]
    fn undo_steps_back_through_edits() {
        let mut input = Input::default();
        for c in "hello".chars() {
            input.push_char(c);
        }
        input.backspace();
        assert_eq!(input.text(), "hell");
        assert!(input.undo()); // backspace
        assert_eq!(input.text(), "hello");
        assert!(input.undo()); // last typed 'o'
        assert_eq!(input.text(), "hell");
        assert!(input.undo()); // previous 'l'
        assert_eq!(input.text(), "hel");
        assert!(input.undo()); // 'e'
        assert!(input.undo()); // 'h'
        assert!(input.undo()); // the original empty buffer
        assert_eq!(input.text(), "");
        assert!(!input.undo()); // nothing left
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut input = Input::default();
        for c in "abc".chars() {
            input.push_char(c);
        }
        input.backspace(); // "ab"
        input.undo(); // "abc"
        assert_eq!(input.text(), "abc");
        assert!(input.redo()); // "ab"
        assert_eq!(input.text(), "ab");
        assert!(!input.redo()); // redo stack exhausted
        assert_eq!(input.text(), "ab");
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut input = Input::default();
        input.push_char('a');
        input.undo();
        input.push_char('b');
        assert!(!input.redo());
        assert_eq!(input.text(), "b");
    }

    #[test]
    fn undo_restores_cursor_position() {
        let mut input = Input::default();
        input.set_text("hello world".to_string());
        input.move_home();
        input.move_word_right(); // after "hello "
        input.insert_str("big ");
        let cursor = input.cursor();
        assert_eq!(input.text(), "hello big world");
        assert!(input.undo());
        assert_eq!(input.text(), "hello world");
        assert_eq!(input.cursor(), cursor - 4);
    }

    #[test]
    fn undo_does_not_capture_history_nav() {
        let mut input = Input::default();
        input.set_text("one".to_string());
        input.submit();
        input.history_prev();
        assert_eq!(input.text(), "one");
        assert!(!input.undo());
    }
}
