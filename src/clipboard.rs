//! Clipboard access for the `/copy` command.
//!
//! Tries the OSC 52 escape sequence first (works over ssh/tmux and in most
//! modern terminals, and needs no external tool), then falls back to
//! `wl-copy` (Wayland), `xclip`, or `xsel` (X11) when available.

use std::io::{self, Write};
use std::process::{Command, Stdio};

/// Copy `text` to the system clipboard. Returns an error when neither OSC 52
/// nor any clipboard tool is available.
pub fn copy_text(text: &str) -> Result<(), String> {
    if copy_osc52(text) {
        return Ok(());
    }
    for tool in [
        &["wl-copy"][..],
        &["xclip", "-selection", "clipboard"][..],
        &["xsel", "--clipboard", "--input"][..],
    ] {
        if pipe_to(tool, text) {
            return Ok(());
        }
    }
    Err(
        "no clipboard available (terminal rejects OSC 52 and wl-copy/xclip/xsel are missing)"
            .to_string(),
    )
}

/// OSC 52: `ESC ] 52 ; <selection> ; <base64> BEL`. The empty selection is
/// the system clipboard. Writing this while inside the alternate screen is
/// fine — it is a control sequence, not screen output. Returns whether the
/// terminal accepted it (accepted is optimistic: a reply is only one
/// transport, and many terminals accept without acking).
fn copy_osc52(text: &str) -> bool {
    let mut stdout = io::stdout();
    let encoded = base64_encode(text.as_bytes());
    let sequence = format!("\x1b]52;c;{encoded}\x07");
    stdout.write_all(sequence.as_bytes()).is_ok() && stdout.flush().is_ok()
}

/// Pipe `text` into the given command's stdin; true when it exited 0.
fn pipe_to(cmd: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let written = child
        .stdin
        .take()
        .map(|mut stdin| stdin.write_all(text.as_bytes()).is_ok())
        .unwrap_or(false);
    if !written {
        return false;
    }
    child.wait().map(|status| status.success()).unwrap_or(false)
}

/// Minimal base64 encoder (RFC 4648, no padding lines).
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_encodes_standard_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_handles_binary_and_unicode() {
        assert_eq!(base64_encode(&[0xff, 0x00, 0xfe]), "/wD+");
        assert_eq!(base64_encode("héllo".as_bytes()), "aMOpbGxv");
    }
}
