//! A small markdown parser and renderer producing styled, wrapped lines.
//!
//! Supported: headings 1–3, unordered/numbered lists, blockquotes, thematic
//! breaks, fenced code blocks (language-aware, clipped to the viewport),
//! inline code, bold/italic/strikethrough, links, and basic tables. Output is
//! a flat list of [`Line`]s the message component draws with a [`Theme`].

use crate::tui::render::Screen;

/// Text style for a [`Span`]. Kept terminal-agnostic; the theme maps it to
/// colors and attributes at draw time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    #[default]
    None,
    Bold,
    Italic,
    BoldItalic,
    Code,
    Link,
    Heading,
    Dim,
    Strike,
}

/// A styled run of text within a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

/// The kind of a rendered line. Code blocks carry the fence language so the
/// component can syntax-highlight them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    Normal,
    Heading(u8),
    List,
    Quote,
    /// A fence delimiter line (```lang or ```).
    CodeFence,
    /// A code line, carrying the block's language.
    Code(String),
    HRule,
    TableSep,
}

/// One rendered line of markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    pub spans: Vec<Span>,
}

/// Render markdown source to wrapped lines that fit `width`.
pub fn render_markdown(text: &str, width: usize) -> Vec<Line> {
    let mut out = Vec::new();
    let mut code_lang: Option<String> = None;

    for raw in text.split('\n') {
        let trimmed = raw.trim_end();
        if code_lang.is_some() {
            if is_fence_close(trimmed) {
                out.push(Line {
                    kind: LineKind::CodeFence,
                    spans: vec![Span {
                        text: Screen::clip(trimmed, width),
                        style: Style::Dim,
                    }],
                });
                code_lang = None;
            } else {
                out.push(Line {
                    kind: LineKind::Code(code_lang.clone().unwrap_or_default()),
                    spans: vec![Span {
                        text: Screen::clip(trimmed, width),
                        style: Style::None,
                    }],
                });
            }
            continue;
        }

        if let Some(lang) = fence_open(trimmed) {
            code_lang = Some(lang);
            out.push(Line {
                kind: LineKind::CodeFence,
                spans: vec![Span {
                    text: Screen::clip(trimmed, width),
                    style: Style::Dim,
                }],
            });
            continue;
        }

        if let Some(level) = heading_level(trimmed) {
            let content = trimmed[level..].trim_start();
            out.extend(wrap_line(
                LineKind::Heading(level as u8),
                parse_inline(content),
                width,
            ));
            continue;
        }

        if is_hr(trimmed) {
            out.push(Line {
                kind: LineKind::HRule,
                spans: vec![Span {
                    text: "─".repeat(width.saturating_sub(1)),
                    style: Style::Dim,
                }],
            });
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix('>') {
            let content = rest.strip_prefix(' ').unwrap_or(rest).trim_end();
            let mut spans = vec![Span {
                text: "▍".to_string(),
                style: Style::Dim,
            }];
            if !content.is_empty() {
                spans.extend(parse_inline(content));
            }
            out.extend(wrap_line(LineKind::Quote, spans, width));
            continue;
        }

        if let Some(content) = list_item(trimmed) {
            let mut spans = vec![Span {
                text: format!("{} ", content_prefix(trimmed)),
                style: Style::Dim,
            }];
            spans.extend(parse_inline(content.trim_start()));
            out.extend(wrap_line(LineKind::List, spans, width));
            continue;
        }

        if trimmed.contains('|') {
            if is_table_separator(trimmed) {
                out.push(Line {
                    kind: LineKind::TableSep,
                    spans: vec![Span {
                        text: Screen::clip(trimmed, width),
                        style: Style::Dim,
                    }],
                });
            } else {
                out.extend(wrap_line(LineKind::Normal, parse_table_row(trimmed), width));
            }
            continue;
        }

        out.extend(wrap_line(LineKind::Normal, parse_inline(trimmed), width));
    }

    out
}

/// Wrap a line's spans to the width. Returns one [`Line`] per wrapped row so
/// long paragraphs become multiple rows; an empty source line still yields one
/// empty row (preserving paragraph spacing).
fn wrap_line(kind: LineKind, spans: Vec<Span>, width: usize) -> Vec<Line> {
    let pieces: Vec<(String, Style)> = spans.into_iter().map(|s| (s.text, s.style)).collect();
    let rows = wrap_spans(&pieces, width);
    if rows.is_empty() {
        return vec![Line {
            kind,
            spans: Vec::new(),
        }];
    }
    rows.into_iter()
        .map(|row| Line {
            kind: kind.clone(),
            spans: row
                .into_iter()
                .map(|(text, style)| Span { text, style })
                .collect(),
        })
        .collect()
}

/// Greedy word-wrap of styled spans, breaking individual words across lines
/// (newlines in the source force a hard break). Words are re-joined with single
/// spaces, so the original whitespace runs are normalized.
fn wrap_spans(spans: &[(String, Style)], width: usize) -> Vec<Vec<(String, Style)>> {
    // Flatten into styled words. `space_after` remembers whether a word was
    // followed by whitespace in the source.
    struct Word {
        text: String,
        style: Style,
        space_after: bool,
    }
    let mut words: Vec<Word> = Vec::new();
    for (text, style) in spans {
        let mut word = String::new();
        let flush = |words: &mut Vec<Word>, word: &mut String, space_after: bool| {
            if !word.is_empty() {
                words.push(Word {
                    text: std::mem::take(word),
                    style: *style,
                    space_after,
                });
            }
        };
        for c in text.chars() {
            match c {
                '\n' => {
                    flush(&mut words, &mut word, false);
                    words.push(Word {
                        text: String::new(),
                        style: *style,
                        space_after: false,
                    });
                }
                c if c.is_whitespace() => {
                    if word.is_empty() {
                        // Whitespace at a span boundary: the previous word was
                        // already flushed at its span end, so mark it instead
                        // of dropping the gap between spans.
                        if let Some(last) = words.last_mut() {
                            last.space_after = true;
                        }
                    } else {
                        flush(&mut words, &mut word, true);
                    }
                }
                _ => word.push(c),
            }
        }
        flush(&mut words, &mut word, false);
    }

    let mut lines: Vec<Vec<(String, Style)>> = Vec::new();
    let mut current: Vec<(String, Style)> = Vec::new();
    let mut current_len = 0usize;
    let mut space_after = false;

    for Word {
        text,
        style,
        space_after: next_space,
    } in words
    {
        if text.is_empty() {
            // Hard line break from the source.
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
            }
            continue;
        }
        let word_len = text.chars().count();
        if current_len > 0 && current_len + usize::from(space_after) + word_len > width {
            lines.push(std::mem::take(&mut current));
            current_len = 0;
            space_after = false;
        }
        if space_after && !current.is_empty() {
            current.push((" ".to_string(), style));
            current_len += 1;
        }
        current.push((text, style));
        current_len += word_len;
        space_after = next_space;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

// --- Block detection -------------------------------------------------------

fn heading_level(line: &str) -> Option<usize> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if (1..=3).contains(&hashes) && line[hashes..].starts_with(' ') {
        Some(hashes)
    } else {
        None
    }
}

fn is_hr(line: &str) -> bool {
    if line.len() < 3 {
        return false;
    }
    let first = line.chars().next().unwrap();
    if first != '-' && first != '*' && first != '_' {
        return false;
    }
    line.chars().all(|c| c == first || c == ' ')
}

fn fence_open(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = if let Some(r) = trimmed.strip_prefix("```") {
        r
    } else {
        trimmed.strip_prefix("~~~")?
    };
    // A fence is only a delimiter if the remainder is just the language.
    if rest.is_empty() {
        return Some(String::new());
    }
    let lang = rest.trim();
    if lang
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '+' || c == '#')
    {
        Some(lang.to_string())
    } else {
        None
    }
}

fn is_fence_close(line: &str) -> bool {
    let trimmed = line.trim_start();
    (trimmed.starts_with("```") || trimmed.starts_with("~~~"))
        && trimmed.chars().all(|c| c == '`' || c == '~' || c == ' ')
}

fn list_item(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    for marker in ["-", "*", "+"] {
        if let Some(rest) = trimmed.strip_prefix(marker)
            && (rest.is_empty() || rest.starts_with(' '))
        {
            return Some(rest);
        }
    }
    // Numbered lists: "1." / "1)".
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 {
        let after = &trimmed[i..];
        if let Some(rest) = after.strip_prefix(". ") {
            return Some(rest);
        }
        if let Some(rest) = after.strip_prefix(") ") {
            return Some(rest);
        }
    }
    None
}

fn content_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix(['-', '*', '+'])
        && rest.starts_with(' ')
    {
        return &trimmed[..1];
    }
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && (trimmed[i..].starts_with(". ") || trimmed[i..].starts_with(") ")) {
        return &trimmed[..i + 1];
    }
    "•"
}

fn is_table_separator(line: &str) -> bool {
    let inner: String = line
        .trim()
        .trim_matches('|')
        .chars()
        .filter(|c| *c != '|')
        .collect();
    !inner.is_empty() && inner.chars().all(|c| c == '-' || c == ':' || c == ' ')
}

fn parse_table_row(line: &str) -> Vec<Span> {
    let trimmed = line.trim();
    let cells: Vec<&str> = trimmed
        .trim_matches('|')
        .split('|')
        .map(|c| c.trim())
        .collect();
    let mut spans = Vec::new();
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            spans.push(Span {
                text: " │ ".to_string(),
                style: Style::Dim,
            });
        }
        spans.extend(parse_inline(cell));
    }
    spans
}

// --- Inline parsing --------------------------------------------------------

/// Parse inline markdown into styled spans.
pub fn parse_inline(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;

    macro_rules! flush {
        () => {
            if !plain.is_empty() {
                spans.push(Span {
                    text: std::mem::take(&mut plain),
                    style: Style::None,
                });
            }
        };
    }

    while i < chars.len() {
        // Fenced inline code `` `code` ``.
        if chars[i] == '`' {
            let mut end = i + 1;
            while end < chars.len() && chars[end] != '`' {
                end += 1;
            }
            if end < chars.len() {
                flush!();
                let code: String = chars[i + 1..end].iter().collect();
                spans.push(Span {
                    text: code,
                    style: Style::Code,
                });
                i = end + 1;
                continue;
            }
            plain.push('`');
            i += 1;
            continue;
        }

        // Bold ** or __.
        if i + 1 < chars.len() && (chars[i] == '*' || chars[i] == '_') && chars[i] == chars[i + 1] {
            let delim = chars[i];
            let mut end = i + 2;
            while end + 1 < chars.len() && !(chars[end] == delim && chars[end + 1] == delim) {
                end += 1;
            }
            if end + 1 < chars.len() {
                flush!();
                let inner: String = chars[i + 2..end].iter().collect();
                push_inline_styled(&mut spans, &inner, Style::Bold);
                i = end + 2;
                continue;
            }
            plain.push(delim);
            plain.push(delim);
            i += 2;
            continue;
        }

        // Italic * or _.
        if chars[i] == '*' || chars[i] == '_' {
            let delim = chars[i];
            let mut end = i + 1;
            while end < chars.len() && chars[end] != delim {
                end += 1;
            }
            if end < chars.len() && end > i + 1 {
                flush!();
                let inner: String = chars[i + 1..end].iter().collect();
                push_inline_styled(&mut spans, &inner, Style::Italic);
                i = end + 1;
                continue;
            }
            plain.push(delim);
            i += 1;
            continue;
        }

        // Strikethrough ~~text~~.
        if i + 1 < chars.len() && chars[i] == '~' && chars[i + 1] == '~' {
            let mut end = i + 2;
            while end + 1 < chars.len() && !(chars[end] == '~' && chars[end + 1] == '~') {
                end += 1;
            }
            if end + 1 < chars.len() {
                flush!();
                let inner: String = chars[i + 2..end].iter().collect();
                push_inline_styled(&mut spans, &inner, Style::Strike);
                i = end + 2;
                continue;
            }
            plain.push('~');
            plain.push('~');
            i += 2;
            continue;
        }

        // Link [text](url).
        if chars[i] == '[' {
            let mut close = i + 1;
            while close < chars.len() && chars[close] != ']' {
                close += 1;
            }
            if close < chars.len() && close + 1 < chars.len() && chars[close + 1] == '(' {
                let mut end = close + 2;
                while end < chars.len() && chars[end] != ')' {
                    end += 1;
                }
                if end < chars.len() {
                    flush!();
                    let inner: String = chars[i + 1..close].iter().collect();
                    let url: String = chars[close + 2..end].iter().collect();
                    spans.push(Span {
                        text: inner,
                        style: Style::Link,
                    });
                    // Show the URL dimly after the label.
                    spans.push(Span {
                        text: format!(" ({url})"),
                        style: Style::Dim,
                    });
                    i = end + 1;
                    continue;
                }
            }
            plain.push('[');
            i += 1;
            continue;
        }

        plain.push(chars[i]);
        i += 1;
    }

    flush!();
    spans
}

/// Parse inline, apply a style to the parsed spans, and append.
fn push_inline_styled(out: &mut Vec<Span>, inner: &str, style: Style) {
    let nested = parse_inline(inner);
    if nested.is_empty() {
        out.push(Span {
            text: String::new(),
            style,
        });
        return;
    }
    for span in nested {
        let combined = match (span.style, style) {
            (Style::Bold, Style::Italic) | (Style::Italic, Style::Bold) => Style::BoldItalic,
            (Style::None, s) => s,
            (s, _) => s,
        };
        out.push(Span {
            text: span.text,
            style: combined,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(line: &Line) -> String {
        line.spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn headings_and_code_blocks() {
        let lines = render_markdown("# Title\n\n```rust\nfn main() {}\n```\n", 60);
        assert_eq!(lines[0].kind, LineKind::Heading(1));
        assert_eq!(lines[1].kind, LineKind::Normal); // blank line
        assert_eq!(lines[2].kind, LineKind::CodeFence);
        assert_eq!(lines[3].kind, LineKind::Code("rust".to_string()));
        assert_eq!(lines[4].kind, LineKind::CodeFence);
    }

    #[test]
    fn inline_styles() {
        let spans = parse_inline("**bold** and `code` and *it*");
        let styles: Vec<Style> = spans.iter().map(|s| s.style).collect();
        assert_eq!(
            styles,
            vec![
                Style::Bold,
                Style::None,
                Style::Code,
                Style::None,
                Style::Italic
            ]
        );
    }

    #[test]
    fn link_renders_label_and_url() {
        let spans = parse_inline("[docs](https://example.com)");
        assert_eq!(
            text_of(&Line {
                kind: LineKind::Normal,
                spans: spans.clone()
            }),
            "docs (https://example.com)"
        );
        assert_eq!(spans[0].style, Style::Link);
        assert_eq!(spans[1].style, Style::Dim);
    }

    #[test]
    fn lists_and_quotes() {
        let lines = render_markdown("- one\n1. two\n> quote\n", 40);
        assert_eq!(lines[0].kind, LineKind::List);
        assert!(text_of(&lines[0]).starts_with("-"));
        assert_eq!(lines[1].kind, LineKind::List);
        assert_eq!(lines[2].kind, LineKind::Quote);
        assert!(text_of(&lines[2]).contains("quote"));
    }

    #[test]
    fn hr_and_tables() {
        let lines = render_markdown("---\n| a | b |\n|---|---|\n| 1 | 2 |\n", 40);
        assert_eq!(lines[0].kind, LineKind::HRule);
        assert_eq!(lines[1].kind, LineKind::Normal);
        assert!(text_of(&lines[1]).contains("│"));
        assert_eq!(lines[2].kind, LineKind::TableSep);
    }

    #[test]
    fn wraps_long_paragraph() {
        let lines = render_markdown("hello brave new world", 10);
        assert_eq!(lines.len(), 3);
        assert!(text_of(&lines[0]).starts_with("hello"));
    }

    #[test]
    fn spaces_survive_span_boundaries() {
        let lines = render_markdown("fix the **bug** in `main.rs`", 60);
        assert_eq!(text_of(&lines[0]), "fix the bug in main.rs");
        let lines = render_markdown("**bold** and `code`", 40);
        assert_eq!(text_of(&lines[0]), "bold and code");
    }

    #[test]
    fn code_is_clipped_not_wrapped() {
        let lines = render_markdown("```\naaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n```\n", 8);
        assert_eq!(lines[1].kind, LineKind::Code(String::new()));
        assert!(text_of(&lines[1]).chars().count() <= 8);
    }

    #[test]
    fn fence_with_language_and_closer() {
        let lines = render_markdown("```python\nprint(1)\n```\n", 40);
        assert_eq!(lines[1].kind, LineKind::Code("python".to_string()));
    }
}
