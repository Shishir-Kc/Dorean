//! Terminal core: raw mode, alternate screen, kitty keyboard protocol,
//! bracketed paste, OSC 11 background detection, and size/resize handling.
//!
//! [`Terminal`] bundles the terminal state for the TUI. It is entered exactly
//! once before the app loop starts and left once on exit, so the user's shell
//! is always restored even if the app returns an error.

use std::io::{self, Write};

use crossterm::cursor;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{execute, queue};

use crate::error::DoreanError;

/// A bracket between `enter` and `leave` that restores the terminal even when
/// dropped without an explicit call.
pub struct Terminal {
    entered: bool,
}

impl Terminal {
    /// Switch the terminal into TUI mode: raw input, alternate screen, hidden
    /// cursor, bracketed paste, kitty keyboard protocol (best-effort).
    pub fn enter() -> Result<Terminal, DoreanError> {
        terminal::enable_raw_mode()?;
        let mut tty = io::stdout();
        execute!(
            tty,
            terminal::EnterAlternateScreen,
            cursor::Hide,
            cursor::MoveTo(0, 0),
            EnableBracketedPaste,
        )?;
        // Kitty keyboard protocol: nicer modifier/escape handling. Best-effort:
        // terminals that don't support it simply ignore the push.
        if terminal::supports_keyboard_enhancement().unwrap_or(false) {
            let _ = queue!(
                tty,
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                        | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                )
            );
        }
        tty.flush()?;
        Ok(Terminal { entered: true })
    }

    /// Restore the terminal to its pre-TUI state. Idempotent.
    pub fn leave(&mut self) -> Result<(), DoreanError> {
        if !self.entered {
            return Ok(());
        }
        self.entered = false;
        // Best-effort: errors here are swallowed so a failing restore still
        // lets the app unwind normally.
        let mut tty = io::stdout();
        let _ = queue!(
            tty,
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste,
            cursor::Show,
            terminal::LeaveAlternateScreen,
        );
        let _ = tty.flush();
        let _ = terminal::disable_raw_mode();
        Ok(())
    }

    /// The current terminal size as (columns, rows).
    pub fn size() -> (u16, u16) {
        terminal::size().unwrap_or((80, 24))
    }

    /// Clear the screen (used for the `/clear` command while in alt screen).
    pub fn clear() -> Result<(), DoreanError> {
        execute!(io::stdout(), Clear(ClearType::All), cursor::MoveTo(0, 0))?;
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

/// Query the terminal's background color via OSC 11 and return it as RGB,
/// or `None` if the terminal doesn't answer in time or isn't a color terminal.
///
/// The query is written right after raw mode is enabled and read directly from
/// the tty fd with a short timeout, before any other event reader is started.
pub fn query_background_color(timeout_ms: u64) -> Option<(u8, u8, u8)> {
    use std::os::unix::io::AsRawFd;
    use std::time::{Duration, Instant};

    let mut stdout = io::stdout();
    // OSC 11 query terminated with BEL (also send ST form as a fallback).
    let _ = stdout.write_all(b"\x1b]11;?\x07");
    let _ = stdout.flush();

    let fd = io::stdin().as_raw_fd();
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut buf = [0u8; 64];
    let mut bytes = Vec::new();

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, remaining.as_millis() as i32) };
        if rc <= 0 {
            return None;
        }
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            return None;
        }
        bytes.extend_from_slice(&buf[..n as usize]);
        // The response ends with ST (ESC \) or BEL; stop at the first terminator.
        if bytes
            .iter()
            .position(|&b| b == 0x07 || b == b'\\')
            .is_some()
        {
            break;
        }
    }

    parse_osc11(&bytes)
}

/// Parse an OSC 11 response into RGB. Accepts both `rgb:RRRR/GGGG/BBBB` (the
/// standard form, 4 hex digits per channel) and `rgba:...` prefixes, plus the
/// odd terminal that replies with 2-digit hex channels.
fn parse_osc11(bytes: &[u8]) -> Option<(u8, u8, u8)> {
    let text = String::from_utf8_lossy(bytes);
    // Locate the OSC 11 response anywhere in the buffer (the terminal may
    // answer amid other output).
    let start = text.find("\x1b]11;")? + 5;
    let end = text[start..]
        .find('\x1b')
        .map(|i| start + i)
        .or_else(|| text[start..].find('\x07').map(|i| start + i))
        .unwrap_or(text.len());
    let body = &text[start..end];
    let body = body
        .strip_prefix("rgb:")
        .or_else(|| body.strip_prefix("rgba:"))?;

    let mut parts = body.split('/');
    let r = parse_hex_channel(parts.next()?)?;
    let g = parse_hex_channel(parts.next()?)?;
    let b = parse_hex_channel(parts.next()?)?;
    Some((r, g, b))
}

/// Parse a 2- or 4-digit hex channel into a byte (4-digit = 16-bit → top byte).
fn parse_hex_channel(s: &str) -> Option<u8> {
    if s.len() == 4 {
        u16::from_str_radix(s, 16).ok().map(|v| (v >> 8) as u8)
    } else if s.len() == 2 {
        u8::from_str_radix(s, 16).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rgb_response() {
        let resp = b"\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\";
        assert_eq!(parse_osc11(resp), Some((0x1e, 0x1e, 0x1e)));
    }

    #[test]
    fn parses_bel_terminated_2digit() {
        let resp = b"\x1b]11;rgb:ff/00/80\x07";
        assert_eq!(parse_osc11(resp), Some((0xff, 0x00, 0x80)));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_osc11(b"hello"), None);
        assert_eq!(parse_osc11(b"\x1b]11;rgb:xx/yy/zz\x1b\\"), None);
    }

    #[test]
    fn ignores_surrounding_noise() {
        let resp = b"prefix\x1b]11;rgb:1010/2020/3030\x1b\\suffix";
        assert_eq!(parse_osc11(resp), Some((0x10, 0x20, 0x30)));
    }
}
