//! Screen buffer and draw primitives with incremental (diffed) redraw.
//!
//! The app draws each frame into a [`Screen`]; [`Screen::flush`] compares it
//! with the previously flushed frame and emits only the cells that changed,
//! using minimal style transitions. This keeps the alt-screen render flicker
//! free without re-sending every cell on every frame.

use std::io::{self, Write};

use crossterm::cursor;
use crossterm::queue;
use crossterm::style::{
    Attribute, Color, Print, SetAttribute, SetBackgroundColor, SetForegroundColor,
};

use crate::tui::theme::Theme;

/// Font attributes, kept as plain booleans for cheap equality.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Attrs {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub reversed: bool,
}

impl Attrs {
    pub const fn none() -> Self {
        Attrs {
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            reversed: false,
        }
    }

    pub const fn bold() -> Self {
        Attrs {
            bold: true,
            ..Attrs::none()
        }
    }

    pub const fn dim() -> Self {
        Attrs {
            dim: true,
            ..Attrs::none()
        }
    }

    pub const fn italic() -> Self {
        Attrs {
            italic: true,
            ..Attrs::none()
        }
    }

    pub const fn underline() -> Self {
        Attrs {
            underline: true,
            ..Attrs::none()
        }
    }

    pub const fn reversed() -> Self {
        Attrs {
            reversed: true,
            ..Attrs::none()
        }
    }
}

/// One character cell of the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
}

impl Cell {
    /// A blank cell using a solid background (paint with the theme bg).
    pub fn blank(bg: Color) -> Self {
        Cell {
            ch: ' ',
            fg: Color::Reset,
            bg,
            attrs: Attrs::none(),
        }
    }

    /// A blank cell that inherits the terminal's default background.
    pub fn reset() -> Self {
        Cell {
            ch: ' ',
            fg: Color::Reset,
            bg: Color::Reset,
            attrs: Attrs::none(),
        }
    }
}

impl Screen {
    /// A blank [`Cell`] painted with a solid background.
    pub fn blank_cell(bg: Color) -> Cell {
        Cell::blank(bg)
    }
}

/// A full-screen buffer of cells plus an optional cursor position.
pub struct Screen {
    pub width: usize,
    pub height: usize,
    cells: Vec<Cell>,
    pub cursor: Option<(usize, usize)>,
}

impl Screen {
    /// Build a blank screen. `bg` paints the whole surface (theme background).
    pub fn new(width: usize, height: usize, bg: Color) -> Self {
        let cell = Cell::blank(bg);
        Screen {
            width,
            height,
            cells: vec![cell; width * height],
            cursor: None,
        }
    }

    /// A blank screen using the terminal's default colors.
    pub fn default_blank(width: usize, height: usize) -> Self {
        let cell = Cell::reset();
        Screen {
            width,
            height,
            cells: vec![cell; width * height],
            cursor: None,
        }
    }

    pub fn in_bounds(&self, x: usize, y: usize) -> bool {
        x < self.width && y < self.height
    }

    pub fn get(&self, x: usize, y: usize) -> Option<Cell> {
        if !self.in_bounds(x, y) {
            return None;
        }
        Some(self.cells[y * self.width + x])
    }

    pub fn put(&mut self, x: usize, y: usize, cell: Cell) {
        if self.in_bounds(x, y) {
            self.cells[y * self.width + x] = cell;
        }
    }

    pub fn put_char(&mut self, x: usize, y: usize, ch: char, fg: Color, bg: Color, attrs: Attrs) {
        self.put(x, y, Cell { ch, fg, bg, attrs });
    }

    /// Draw a horizontal run of characters with a single style.
    pub fn put_str(&mut self, x: usize, y: usize, text: &str, fg: Color, bg: Color, attrs: Attrs) {
        for (cx, ch) in (x..).zip(text.chars()) {
            self.put_char(cx, y, ch, fg, bg, attrs);
        }
    }

    /// Draw text with a per-character style callback (for syntax highlighting).
    pub fn put_spans(&mut self, x: usize, y: usize, spans: &[(String, Attrs, Color)], bg: Color) {
        let mut cx = x;
        for (text, attrs, fg) in spans {
            for ch in text.chars() {
                self.put_char(cx, y, ch, *fg, bg, *attrs);
                cx += 1;
            }
        }
    }

    pub fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, cell: Cell) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.put(xx, yy, cell);
            }
        }
    }

    pub fn hline(&mut self, x: usize, y: usize, len: usize, ch: char, fg: Color, bg: Color) {
        for i in 0..len {
            self.put_char(x + i, y, ch, fg, bg, Attrs::none());
        }
    }

    pub fn vline(&mut self, x: usize, y: usize, len: usize, ch: char, fg: Color, bg: Color) {
        for i in 0..len {
            self.put_char(x, y + i, ch, fg, bg, Attrs::none());
        }
    }

    /// Draw a box border with the theme's border color.
    pub fn box_border(&mut self, x: usize, y: usize, w: usize, h: usize, theme: &Theme) {
        if w < 2 || h < 2 {
            return;
        }
        let fg = theme.border;
        let bg = theme.bg;
        self.hline(x + 1, y, w - 2, '─', fg, bg);
        self.hline(x + 1, y + h - 1, w - 2, '─', fg, bg);
        self.vline(x, y + 1, h - 2, '│', fg, bg);
        self.vline(x + w - 1, y + 1, h - 2, '│', fg, bg);
        self.put_char(x, y, '┌', fg, bg, Attrs::none());
        self.put_char(x + w - 1, y, '┐', fg, bg, Attrs::none());
        self.put_char(x, y + h - 1, '└', fg, bg, Attrs::none());
        self.put_char(x + w - 1, y + h - 1, '┘', fg, bg, Attrs::none());
    }

    pub fn set_cursor(&mut self, x: usize, y: usize) {
        self.cursor = Some((x, y));
    }

    /// Truncate text to fit `width`, appending a marker when cut.
    pub fn clip(text: &str, width: usize) -> String {
        let mut chars: Vec<char> = text.chars().collect();
        if chars.len() <= width {
            return text.to_string();
        }
        if width == 0 {
            return String::new();
        }
        if width == 1 {
            return "…".to_string();
        }
        chars.truncate(width - 1);
        let mut out: String = chars.into_iter().collect();
        out.push('…');
        out
    }

    /// Write this frame, emitting only cells that differ from the previous
    /// frame (passed in `prev`). The previous frame is updated in place so the
    /// next call diffs against the just-emitted state. Generic over the
    /// writer so tests can capture the ANSI byte stream (e.g. into a `vt100`
    /// parser).
    pub fn flush<W: Write>(&self, stdout: &mut W, prev: &mut Screen) -> io::Result<()> {
        prev.resize_like(self);
        let mut last_style: Option<(Color, Color, Attrs)> = None;

        for y in 0..self.height {
            for x in 0..self.width {
                let cell = self.cells[y * self.width + x];
                let old = prev.cells[y * self.width + x];
                if cell == old {
                    continue;
                }
                let style = (cell.fg, cell.bg, cell.attrs);
                if last_style != Some(style) {
                    self.emit_style(stdout, style)?;
                    last_style = Some(style);
                }
                queue!(stdout, cursor::MoveTo(x as u16, y as u16), Print(cell.ch))?;
            }
        }

        // Reset style for anything drawn after the frame (e.g. the status
        // bar's leftover glyphs on resize) and place the cursor.
        if let Some((cx, cy)) = self.cursor {
            queue!(
                stdout,
                cursor::MoveTo(cx as u16, cy as u16),
                SetAttribute(Attribute::Reset),
            )?;
        }

        *prev = self.clone();
        stdout.flush()
    }

    fn emit_style<W: Write>(
        &self,
        stdout: &mut W,
        (fg, bg, attrs): (Color, Color, Attrs),
    ) -> io::Result<()> {
        queue!(
            stdout,
            SetForegroundColor(fg),
            SetBackgroundColor(bg),
            SetAttribute(Attribute::Reset),
        )?;
        if attrs.bold {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        if attrs.dim {
            queue!(stdout, SetAttribute(Attribute::Dim))?;
        }
        if attrs.italic {
            queue!(stdout, SetAttribute(Attribute::Italic))?;
        }
        if attrs.underline {
            queue!(stdout, SetAttribute(Attribute::Underlined))?;
        }
        if attrs.reversed {
            queue!(stdout, SetAttribute(Attribute::Reverse))?;
        }
        Ok(())
    }

    fn resize_like(&mut self, other: &Screen) {
        if self.width == other.width && self.height == other.height {
            return;
        }
        *self = Screen::new(other.width, other.height, other.cells[0].bg);
    }
}

impl Clone for Screen {
    fn clone(&self) -> Self {
        Screen {
            width: self.width,
            height: self.height,
            cells: self.cells.clone(),
            cursor: self.cursor,
        }
    }
}

/// Greedy word-wrap of text to a width. Returns the wrapped lines. A word
/// longer than the width is hard-broken. Preserves a trailing newline as an
/// empty last line (for blank lines inside code blocks).
pub fn word_wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return text.lines().map(|_| String::new()).collect();
    }
    let mut lines = Vec::new();
    let mut current = String::new();

    for raw in text.split('\n') {
        let line = raw.trim_end();
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.is_empty() {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            lines.push(String::new());
            continue;
        }
        for word in words {
            // Break over-long words into chunks.
            let mut chunk = word.to_string();
            while chunk.chars().count() > width {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                let mut chars: Vec<char> = chunk.chars().collect();
                let head: String = chars.drain(..width).collect();
                chunk = chars.into_iter().collect();
                lines.push(head);
            }
            if current.chars().count() + 1 + chunk.chars().count() > width {
                lines.push(std::mem::take(&mut current));
            }
            if current.is_empty() {
                current.push_str(&chunk);
            } else {
                current.push(' ');
                current.push_str(&chunk);
            }
        }
        lines.push(std::mem::take(&mut current));
    }
    lines
}

/// Wrap a sequence of styled spans, breaking only at whitespace boundaries
/// between spans. Returns wrapped lines of spans. Long spans are not broken;
/// callers should pre-split very long spans.
pub fn wrap_spans(
    spans: &[(String, Attrs, Color)],
    width: usize,
) -> Vec<Vec<(String, Attrs, Color)>> {
    let mut lines: Vec<Vec<(String, Attrs, Color)>> = Vec::new();
    let mut current: Vec<(String, Attrs, Color)> = Vec::new();
    let mut current_len = 0usize;

    for (text, attrs, fg) in spans {
        for (i, piece) in text.split('\n').enumerate() {
            if i > 0 {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
            }
            let piece_len = piece.chars().count();
            let separator = usize::from(!current.is_empty());
            if current_len + separator + piece_len > width && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
            }
            current.push((piece.to_string(), *attrs, *fg));
            current_len += piece_len;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color::Rgb { r, g, b }
    }

    #[test]
    fn wraps_words() {
        let lines = word_wrap("hello brave new world", 10);
        assert_eq!(lines, vec!["hello", "brave new", "world"]);
    }

    #[test]
    fn hard_breaks_long_words() {
        let lines = word_wrap("supercalifragilistic", 6);
        assert_eq!(lines, vec!["superc", "alifra", "gilist", "ic"]);
    }

    #[test]
    fn preserves_blank_lines() {
        let lines = word_wrap("a\n\nb", 10);
        assert_eq!(lines, vec!["a", "", "b"]);
    }

    #[test]
    fn clip_truncates() {
        assert_eq!(Screen::clip("hello", 10), "hello");
        assert_eq!(Screen::clip("hello world", 6), "hello…");
        assert_eq!(Screen::clip("hi", 1), "…");
    }

    #[test]
    fn put_and_get_round_trip() {
        let mut screen = Screen::new(5, 3, rgb(1, 1, 1));
        screen.put_char(2, 1, 'x', Color::White, Color::Black, Attrs::bold());
        let cell = screen.get(2, 1).unwrap();
        assert_eq!(cell.ch, 'x');
        assert!(cell.attrs.bold);
        assert!(screen.get(9, 9).is_none());
    }

    #[test]
    fn out_of_bounds_puts_are_ignored() {
        let mut screen = Screen::new(2, 2, Color::Reset);
        screen.put_str(0, 0, "toolong", Color::White, Color::Black, Attrs::none());
        assert_eq!(screen.get(0, 0).unwrap().ch, 't');
        assert_eq!(screen.get(1, 0).unwrap().ch, 'o');
        assert!(screen.get(2, 0).is_none());
    }

    #[test]
    fn resize_like_matches_dimensions() {
        let mut screen = Screen::new(1, 1, Color::Reset);
        let other = Screen::new(10, 5, rgb(2, 2, 2));
        screen.resize_like(&other);
        assert_eq!(screen.width, 10);
        assert_eq!(screen.height, 5);
    }
}
