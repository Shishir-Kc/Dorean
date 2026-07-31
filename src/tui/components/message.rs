//! Message model for the chat view plus measuring and drawing.
//!
//! [`Message::measure`] returns the vertical height a message occupies for a
//! given width; [`Message::draw`] renders it. Both recompute from the same
//! markdown/highlight pass so they always agree, which keeps the virtualized
//! message list consistent under scrolling and resizing.

use crossterm::style::Color;

use crate::tui::diff::{DiffKind, annotate, looks_like_unified_diff};
use crate::tui::highlight::{Lang, highlight_line, normalize_lang};
use crate::tui::markdown::{Line as MdLine, LineKind, Style as MdStyle};
use crate::tui::render::{Attrs, Screen};
use crate::tui::theme::Theme;

/// The lifecycle of a tool call shown in the chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Ok,
    Error,
}

/// A tool call entry: header line, optional expandable output.
#[derive(Debug, Clone)]
pub struct ToolUi {
    /// The tool-call id (matches `StreamEvent::ToolResult`).
    pub id: String,
    pub name: String,
    pub summary: String,
    pub status: ToolStatus,
    pub output: String,
    /// Whether the output block is expanded (toggle with Enter on the entry).
    pub expanded: bool,
    /// Max lines of output shown when expanded.
    pub max_lines: usize,
}

/// What kind of message this is.
#[derive(Debug, Clone)]
pub enum MessageKind {
    User,
    Assistant,
    Agent { name: String },
    Tool(ToolUi),
    System,
}

/// One message in the conversation.
#[derive(Debug, Clone)]
pub struct Message {
    pub kind: MessageKind,
    /// Raw text (markdown for User/Assistant/Agent, plain for System).
    pub text: String,
    /// Reasoning text (reasoning models), shown dimmed above the content.
    pub reasoning: String,
    /// Whether the model is still streaming this message.
    pub streaming: bool,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Message {
            kind: MessageKind::User,
            text: text.into(),
            reasoning: String::new(),
            streaming: false,
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Message {
            kind: MessageKind::Assistant,
            text: text.into(),
            reasoning: String::new(),
            streaming: false,
        }
    }

    pub fn agent(name: impl Into<String>, text: impl Into<String>) -> Self {
        Message {
            kind: MessageKind::Agent { name: name.into() },
            text: text.into(),
            reasoning: String::new(),
            streaming: false,
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Message {
            kind: MessageKind::System,
            text: text.into(),
            reasoning: String::new(),
            streaming: false,
        }
    }

    /// Height in terminal rows for the given content width. `show_thinking`
    /// controls whether the reasoning block contributes rows (the `/thinking`
    /// toggle).
    pub fn measure(&self, width: usize, _theme: &Theme, show_thinking: bool) -> usize {
        match &self.kind {
            MessageKind::User | MessageKind::Assistant | MessageKind::Agent { .. } => {
                let mut height = 1; // header
                if show_thinking && !self.reasoning.is_empty() {
                    height += 1; // "◈ thinking" header
                    height +=
                        crate::tui::render::word_wrap(&self.reasoning, thinking_inner(width)).len();
                }
                let text = if self.text.is_empty() {
                    "\n".to_string()
                } else {
                    self.text.clone()
                };
                let inner = match &self.kind {
                    MessageKind::User | MessageKind::Agent { .. } => width.saturating_sub(2).max(1),
                    _ => width,
                };
                height
                    + crate::tui::markdown::render_markdown(&text, inner)
                        .len()
                        .max(1)
            }
            MessageKind::Tool(tool) => {
                let mut height = 1; // header
                if tool.expanded && !tool.output.is_empty() {
                    let n = tool.output.lines().count();
                    height += n.min(tool.max_lines);
                    if n > tool.max_lines {
                        height += 1; // "N more lines" row
                    }
                    height += 1; // closing row
                }
                height
            }
            MessageKind::System => 1,
        }
    }

    /// Draw the message starting at (x, y). Returns the number of rows used.
    pub fn draw(
        &self,
        screen: &mut Screen,
        x: usize,
        y: usize,
        width: usize,
        theme: &Theme,
        show_thinking: bool,
    ) -> usize {
        self.draw_region(screen, x, y, width, theme, 0, show_thinking)
    }

    /// Draw the message starting at its `skip`-th row (used by the virtualized
    /// list when the top of a message is scrolled off). Rows drawn always
    /// start at the top of the message, so the caller can place `y` above the
    /// viewport and let the screen clip. Returns the number of rows drawn.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_region(
        &self,
        screen: &mut Screen,
        x: usize,
        y: usize,
        width: usize,
        theme: &Theme,
        skip: usize,
        show_thinking: bool,
    ) -> usize {
        let mut used = 0usize;
        let mut rem = skip;
        match &self.kind {
            MessageKind::User | MessageKind::Assistant | MessageKind::Agent { .. } => {
                let (header, color, bubble_bg, bar) = match &self.kind {
                    MessageKind::User => (
                        "▸ you".to_string(),
                        theme.user,
                        theme.user_bg,
                        Some(theme.user),
                    ),
                    MessageKind::Assistant => {
                        ("▸ assistant".to_string(), theme.accent, theme.bg, None)
                    }
                    MessageKind::Agent { name } => (
                        format!("▸ @{name}"),
                        theme.agent,
                        theme.agent_bg,
                        Some(theme.agent),
                    ),
                    _ => unreachable!(),
                };
                if rem > 0 {
                    rem -= 1;
                } else {
                    let hx = if let Some(bar_color) = bar {
                        screen.put_char(x, y, '▌', bar_color, bubble_bg, Attrs::none());
                        x + 2
                    } else {
                        x
                    };
                    screen.put_str(hx, y, &header, color, bubble_bg, Attrs::bold());
                    screen.fill(
                        hx + header.chars().count(),
                        y,
                        width.saturating_sub(hx + header.chars().count()),
                        1,
                        Screen::blank_cell(bubble_bg),
                    );
                    used += 1;
                }

                if show_thinking && !self.reasoning.is_empty() {
                    let inner = thinking_inner(width);
                    if rem > 0 {
                        rem -= 1;
                    } else {
                        screen.put_str(
                            x + 2,
                            y + used,
                            "◈ thinking",
                            theme.dim,
                            bubble_bg,
                            Attrs::bold(),
                        );
                        screen.fill(
                            x + 2 + "◈ thinking".chars().count(),
                            y + used,
                            width.saturating_sub(x + 2 + "◈ thinking".chars().count()),
                            1,
                            Screen::blank_cell(bubble_bg),
                        );
                        used += 1;
                    }
                    for line in crate::tui::render::word_wrap(&self.reasoning, inner) {
                        if rem > 0 {
                            rem -= 1;
                            continue;
                        }
                        screen.put_str(
                            x + 4,
                            y + used,
                            &Screen::clip(&line, inner),
                            theme.dim,
                            bubble_bg,
                            Attrs::italic(),
                        );
                        used += 1;
                    }
                }

                if self.text.is_empty() {
                    if rem == 0 {
                        screen.put_char(x, y + used, ' ', theme.fg, bubble_bg, Attrs::none());
                        used += 1;
                    }
                    return used.max(1);
                }
                let inner = if bar.is_some() {
                    width.saturating_sub(2).max(1)
                } else {
                    width
                };
                let lines = crate::tui::markdown::render_markdown(&self.text, inner);
                let total = lines.len();
                for (i, line) in lines.iter().enumerate() {
                    if rem > 0 {
                        rem -= 1;
                        continue;
                    }
                    let is_last = i + 1 == total;
                    draw_md_line(
                        screen,
                        x,
                        y + used,
                        width,
                        theme,
                        line,
                        bubble_bg,
                        bar,
                        is_last && self.streaming,
                    );
                    used += 1;
                }
                used.max(1)
            }
            MessageKind::Tool(tool) => draw_tool_region(screen, x, y, width, theme, tool, skip),
            MessageKind::System => {
                if skip > 0 {
                    return 0;
                }
                screen.put_str(x, y, "·", theme.dim, theme.bg, Attrs::none());
                screen.put_str(x + 2, y, &self.text, theme.dim, theme.bg, Attrs::none());
                1
            }
        }
    }
}

/// Draw one rendered markdown line, highlighting code blocks. Exposed for the
/// SPEC/plan overlay, which renders `.dorean/SPEC.md` directly. `bg` is the
/// row background (a bubble tint for user/agent messages); `bar` is an
/// optional left border color that insets the content by two columns.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_md_line(
    screen: &mut Screen,
    x: usize,
    y: usize,
    width: usize,
    theme: &Theme,
    line: &MdLine,
    bg: Color,
    bar: Option<Color>,
    streaming: bool,
) {
    let (cx, cw) = if bar.is_some() {
        (x + 2, width.saturating_sub(2))
    } else {
        (x, width)
    };
    if let Some(color) = bar {
        screen.put_char(x, y, '▌', color, bg, Attrs::none());
    }
    match &line.kind {
        LineKind::Code(lang) => {
            let lang = normalize_lang(lang);
            let plain: String = line.spans.iter().map(|s| s.text.as_str()).collect();
            draw_code_line(screen, cx, y, cw, theme, lang, &plain);
            if streaming {
                draw_cursor(screen, cx + cw.saturating_sub(1), y, theme);
            }
        }
        LineKind::CodeFence => {
            let plain: String = line.spans.iter().map(|s| s.text.as_str()).collect();
            let filled = plain.trim_end();
            screen.put_str(cx, y, filled, theme.dim, theme.code_bg, Attrs::dim());
            fill_bg(screen, cx + filled.chars().count(), y, cw, theme.code_bg);
        }
        LineKind::HRule => {
            let plain: String = line.spans.iter().map(|s| s.text.as_str()).collect();
            screen.put_str(cx, y, &plain, theme.dim, bg, Attrs::none());
        }
        LineKind::Heading(level) => {
            let (attrs, color) = match level {
                1 => (Attrs::bold(), theme.accent),
                2 => (Attrs::bold(), theme.fg),
                _ => (Attrs::bold(), theme.fg),
            };
            draw_spans(screen, cx, y, cw, theme, &line.spans, color, attrs, bg);
        }
        LineKind::Normal | LineKind::List | LineKind::Quote | LineKind::TableSep => {
            draw_spans(
                screen,
                cx,
                y,
                cw,
                theme,
                &line.spans,
                theme.fg,
                Attrs::none(),
                bg,
            );
        }
    }
}

/// Draw a line of code with syntax highlighting over the code background.
fn draw_code_line(
    screen: &mut Screen,
    x: usize,
    y: usize,
    width: usize,
    theme: &Theme,
    lang: Lang,
    line: &str,
) {
    fill_bg(screen, x, y, width, theme.code_bg);
    let tokens = highlight_line(lang, line, theme);
    let mut cx = x;
    for (text, color) in tokens {
        if cx >= x + width {
            break;
        }
        let clip = Screen::clip(&text, x + width - cx);
        screen.put_str(cx, y, &clip, color, theme.code_bg, Attrs::none());
        cx += clip.chars().count();
    }
}

/// Draw styled spans for a markdown line, applying a base style.
#[allow(clippy::too_many_arguments)]
fn draw_spans(
    screen: &mut Screen,
    x: usize,
    y: usize,
    width: usize,
    theme: &Theme,
    spans: &[crate::tui::markdown::Span],
    base_color: Color,
    base_attrs: Attrs,
    bg: Color,
) {
    let mut cx = x;
    for span in spans {
        if cx >= x + width {
            break;
        }
        let (attrs, color) = style_visual(span.style, theme, base_color, base_attrs);
        let clip = Screen::clip(&span.text, x + width - cx);
        screen.put_str(cx, y, &clip, color, bg, attrs);
        cx += clip.chars().count();
    }
    if cx < x + width {
        // Fill the rest of the row so wrapped short lines don't leave styled
        // residue from the previous frame (the diff renderer only overwrites
        // changed cells).
        screen.fill(cx, y, x + width - cx, 1, Screen::blank_cell(bg));
    }
}

/// A cursor glyph for the last streamed line.
fn draw_cursor(screen: &mut Screen, x: usize, y: usize, theme: &Theme) {
    if x > 0 {
        screen.put_char(x, y, '▊', theme.fg, theme.code_bg, Attrs::none());
    }
}

/// Map a markdown style to concrete attributes and colors.
fn style_visual(
    style: MdStyle,
    theme: &Theme,
    base_color: Color,
    base_attrs: Attrs,
) -> (Attrs, Color) {
    match style {
        MdStyle::Bold => (
            Attrs {
                bold: true,
                ..base_attrs
            },
            base_color,
        ),
        MdStyle::Italic => (
            Attrs {
                italic: true,
                ..base_attrs
            },
            base_color,
        ),
        MdStyle::BoldItalic => (
            Attrs {
                bold: true,
                italic: true,
                ..base_attrs
            },
            base_color,
        ),
        MdStyle::Code => (base_attrs, theme.accent),
        MdStyle::Link => (
            Attrs {
                underline: true,
                ..base_attrs
            },
            theme.accent,
        ),
        MdStyle::Heading => (
            Attrs {
                bold: true,
                ..base_attrs
            },
            theme.accent,
        ),
        MdStyle::Dim => (
            Attrs {
                dim: true,
                ..base_attrs
            },
            theme.dim,
        ),
        MdStyle::Strike => (
            Attrs {
                dim: true,
                ..base_attrs
            },
            theme.dim,
        ),
        MdStyle::None => (base_attrs, base_color),
    }
}

/// Draw a tool call as a card (left bar + tinted panel), starting at its
/// `skip`-th row. Matches opencode's "trigger and content" panel: a muted
/// grayscale header with a status glyph, and the output inset beneath.
fn draw_tool_region(
    screen: &mut Screen,
    x: usize,
    y: usize,
    width: usize,
    theme: &Theme,
    tool: &ToolUi,
    skip: usize,
) -> usize {
    let mut used = 0usize;
    let mut rem = skip;

    let (icon, status_color) = match tool.status {
        ToolStatus::Running => ("…", theme.warning),
        ToolStatus::Ok => ("✓", theme.success),
        ToolStatus::Error => ("✗", theme.error),
    };
    let bar = if tool.status == ToolStatus::Error {
        theme.error
    } else {
        theme.tool_bar
    };

    // Header row.
    if rem > 0 {
        rem -= 1;
    } else {
        fill_bg(screen, x, y, width, theme.tool_bg);
        screen.put_char(x, y, '▌', bar, theme.tool_bg, Attrs::none());
        let header = format!("{} · {}", tool.name, tool.summary);
        let clipped = Screen::clip(&header, width.saturating_sub(6));
        screen.put_str(
            x + 2,
            y,
            &clipped,
            theme.tool_header,
            theme.tool_bg,
            Attrs::bold(),
        );
        let gx = x + width.saturating_sub(4);
        screen.put_str(gx, y, icon, status_color, theme.tool_bg, Attrs::bold());
        used += 1;
    }

    if tool.expanded && !tool.output.is_empty() {
        // Output block.
        let max = tool.max_lines.max(1);
        let count = tool.output.lines().count();
        let is_diff = looks_like_unified_diff(&tool.output);
        let annotations = if is_diff {
            Some(annotate(&tool.output))
        } else {
            None
        };
        let mut shown = 0usize;
        let mut truncated = false;
        for (i, raw) in tool.output.lines().enumerate() {
            if shown >= max {
                truncated = true;
                break;
            }
            if rem > 0 {
                rem -= 1;
            } else {
                fill_bg(screen, x, y + used, width, theme.tool_bg);
                screen.put_char(
                    x,
                    y + used,
                    '▌',
                    theme.tool_bar,
                    theme.tool_bg,
                    Attrs::none(),
                );
                let clip = Screen::clip(raw, width.saturating_sub(5));
                if let Some(ann) = &annotations {
                    let (color, attrs) = match ann[i].kind {
                        DiffKind::Added => (theme.success, Attrs::none()),
                        DiffKind::Removed => (theme.error, Attrs::none()),
                        DiffKind::Hunk => (theme.accent, Attrs::bold()),
                        DiffKind::Same => (theme.fg, Attrs::none()),
                    };
                    screen.put_str(x + 3, y + used, &clip, color, theme.tool_bg, attrs);
                } else {
                    screen.put_str(
                        x + 3,
                        y + used,
                        &clip,
                        theme.fg,
                        theme.tool_bg,
                        Attrs::dim(),
                    );
                }
                used += 1;
            }
            shown += 1;
        }
        if truncated {
            if rem > 0 {
                rem -= 1;
            } else {
                fill_bg(screen, x, y + used, width, theme.tool_bg);
                screen.put_char(
                    x,
                    y + used,
                    '▌',
                    theme.tool_bar,
                    theme.tool_bg,
                    Attrs::none(),
                );
                screen.put_str(
                    x + 3,
                    y + used,
                    &format!("… {} more lines", count - shown),
                    theme.dim,
                    theme.tool_bg,
                    Attrs::none(),
                );
                used += 1;
            }
        }

        // Closing row.
        if rem == 0 {
            fill_bg(screen, x, y + used, width, theme.tool_bg);
            screen.put_char(
                x,
                y + used,
                '▌',
                theme.tool_bar,
                theme.tool_bg,
                Attrs::none(),
            );
            used += 1;
        }
    }
    used.max(1)
}

fn fill_bg(screen: &mut Screen, x: usize, y: usize, width: usize, bg: Color) {
    let w = width.min(screen.width.saturating_sub(x));
    if w > 0 {
        screen.fill(x, y, w, 1, Screen::blank_cell(bg));
    }
}

/// Content width for the thinking block (indented 4 columns).
fn thinking_inner(width: usize) -> usize {
    width.saturating_sub(4).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::dark()
    }

    #[test]
    fn measure_matches_basic_text() {
        let msg = Message::assistant("hello world");
        assert_eq!(msg.measure(40, &theme(), true), 2); // header + one content line
    }

    #[test]
    fn measure_accounts_for_wrapping() {
        let msg = Message::assistant("one two three four five six seven");
        assert!(msg.measure(10, &theme(), true) > 1);
    }

    #[test]
    fn tool_measure_changes_with_expansion() {
        let tool = ToolUi {
            id: "c1".into(),
            name: "bash".into(),
            summary: "ls".into(),
            status: ToolStatus::Ok,
            output: "a\nb\nc\nd\n".into(),
            expanded: false,
            max_lines: 3,
        };
        let msg = Message {
            kind: MessageKind::Tool(tool.clone()),
            text: String::new(),
            reasoning: String::new(),
            streaming: false,
        };
        let collapsed = msg.measure(40, &theme(), true);
        let mut expanded = tool;
        expanded.expanded = true;
        let msg = Message {
            kind: MessageKind::Tool(expanded),
            text: String::new(),
            reasoning: String::new(),
            streaming: false,
        };
        let opened = msg.measure(40, &theme(), true);
        assert!(opened > collapsed);
        assert!(opened <= collapsed + 3 + 2 + 1);
    }

    #[test]
    fn tool_measure_counts_closing_row_when_expanded() {
        let tool = ToolUi {
            id: "c1".into(),
            name: "bash".into(),
            summary: "ls".into(),
            status: ToolStatus::Ok,
            output: "a\nb\nc\n".into(),
            expanded: true,
            max_lines: 3,
        };
        let msg = Message {
            kind: MessageKind::Tool(tool),
            text: String::new(),
            reasoning: String::new(),
            streaming: false,
        };
        assert_eq!(msg.measure(40, &theme(), true), 5); // header + 3 output + closing
    }

    #[test]
    fn tool_card_draws_bar_and_header() {
        let tool = ToolUi {
            id: "c1".into(),
            name: "bash".into(),
            summary: "ls".into(),
            status: ToolStatus::Ok,
            output: String::new(),
            expanded: false,
            max_lines: 3,
        };
        let msg = Message {
            kind: MessageKind::Tool(tool),
            text: String::new(),
            reasoning: String::new(),
            streaming: false,
        };
        let mut screen = Screen::new(20, 3, Color::Reset);
        msg.draw(&mut screen, 0, 0, 20, &theme(), true);
        let bar = screen.get(0, 0).unwrap();
        assert_eq!(bar.ch, '▌');
        let name = screen.get(2, 0).unwrap();
        assert_eq!(name.ch, 'b'); // "bash · ls"
        let cell = screen.get(10, 0).unwrap();
        assert_eq!(cell.bg, theme().tool_bg);
    }

    #[test]
    fn user_message_uses_bubble_background() {
        let msg = Message::user("hello");
        let mut screen = Screen::new(20, 3, Color::Reset);
        msg.draw(&mut screen, 0, 0, 20, &theme(), true);
        let bar = screen.get(0, 0).unwrap();
        assert_eq!(bar.ch, '▌');
        let cell = screen.get(10, 0).unwrap();
        assert_eq!(cell.bg, theme().user_bg);
    }

    #[test]
    fn user_bubble_measures_with_inner_width() {
        let long = "x".repeat(60);
        let user_h = Message::user(long.clone()).measure(20, &theme(), true);
        let asst_h = Message::assistant(long).measure(20, &theme(), true);
        // The bubble insets content by 2 columns, so it can wrap more rows.
        assert!(user_h >= asst_h);
    }

    #[test]
    fn thinking_counts_only_when_shown() {
        let mut msg = Message::assistant("answer");
        msg.reasoning = "step one\nstep two".to_string();
        let shown = msg.measure(40, &theme(), true);
        let hidden = msg.measure(40, &theme(), false);
        assert!(shown > hidden);
        // Header + 2 reasoning lines + 1 content line + message header.
        assert_eq!(shown, hidden + 1 + 2);
        assert_eq!(hidden, 2);
    }

    #[test]
    fn thinking_block_is_drawn_distinctly() {
        let mut msg = Message::assistant("answer");
        msg.reasoning = "hidden chain of thought".to_string();
        let mut screen = Screen::new(40, 4, Color::Reset);
        msg.draw(&mut screen, 0, 0, 40, &theme(), true);
        let header = screen.get(2, 1).unwrap();
        assert_eq!(header.ch, '◈');
        let cell = screen.get(4, 2).unwrap();
        assert_eq!(cell.ch, 'h'); // "hidden chain of thought"
    }
}
