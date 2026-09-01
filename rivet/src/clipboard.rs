//! Putting a line of text on the clipboard of whoever is watching a run.
//!
//! The clipboard that matters is the one on the machine the person is looking
//! at, which is usually not the machine the flow is running on: EDA runs happen
//! on a compute server over ssh. So the terminal is asked to do the copying,
//! with the OSC 52 escape sequence, rather than a local clipboard tool being
//! driven directly — the terminal is running next to the clipboard, wherever
//! that is.
//!
//! Not every terminal implements it, and the ones that do not say nothing, so a
//! local helper is asked as well. Between them, one usually lands. Nothing here
//! can report that the text actually arrived: no terminal answers back.

use std::env;
use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;

/// Ask for `text` to be put on the clipboard, returning whether anything was
/// asked at all.
///
/// The escape sequence is written to stderr, where the display is, so callers
/// must already hold the display still — see
/// [`crate::progress::suspend`](crate::progress::suspend).
pub(crate) fn copy(text: &str) -> bool {
    ask_helper(text);
    ask_terminal(text)
}

/// Ask the terminal itself, with OSC 52.
fn ask_terminal(text: &str) -> bool {
    let sequence = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let mut stderr = std::io::stderr().lock();
    stderr
        .write_all(sequence.as_bytes())
        .and_then(|()| stderr.flush())
        .is_ok()
}

/// Ask a clipboard tool on this machine, if one looks likely.
///
/// On its own thread and with its result ignored: a clipboard tool that hangs —
/// an X display that has gone away is the usual way — must not take the display's
/// key handling down with it.
fn ask_helper(text: &str) {
    let helpers = helpers();
    if helpers.is_empty() {
        return;
    }
    let text = text.to_string();
    let _ = thread::Builder::new()
        .name("rivet-clipboard".into())
        .spawn(move || {
            for (program, args) in helpers {
                if run(program, args, &text) {
                    return;
                }
            }
        });
}

/// The clipboard tools worth trying here, best first.
///
/// Which display server is running says which of them can work at all; an X
/// display forwarded over ssh is still the watcher's own machine, so it is
/// worth asking.
fn helpers() -> Vec<(&'static str, &'static [&'static str])> {
    let mut helpers: Vec<(&str, &[&str])> = Vec::new();
    if env::var_os("WAYLAND_DISPLAY").is_some() {
        helpers.push(("wl-copy", &[]));
    }
    if env::var_os("DISPLAY").is_some() {
        helpers.push(("xclip", &["-selection", "clipboard"]));
        helpers.push(("xsel", &["--clipboard", "--input"]));
    }
    if cfg!(target_os = "macos") {
        helpers.push(("pbcopy", &[]));
    }
    helpers
}

/// Feed `text` to one clipboard tool, saying whether it took it.
fn run(program: &str, args: &[&str], text: &str) -> bool {
    // Not installed is the usual answer, and is not worth reporting.
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };

    // Dropped before waiting: these tools read until end of input.
    let written = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(text.as_bytes()).is_ok(),
        None => false,
    };
    let status = child.wait();
    written && status.map(|status| status.success()).unwrap_or(false)
}

/// Base64, which is how OSC 52 carries its payload.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut group = [0u8; 3];
        group[..chunk.len()].copy_from_slice(chunk);
        let bits = u32::from_be_bytes([0, group[0], group[1], group[2]]);
        for index in 0..4 {
            // One character per 6 bits, but only for the bits that came from a
            // byte that was actually there; the rest is padding.
            if index <= chunk.len() {
                out.push(ALPHABET[(bits >> (18 - 6 * index)) as usize & 0x3f] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads_every_length() {
        // The classic vectors: each remainder mod 3 pads differently.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_covers_the_whole_alphabet() {
        // 0xfb 0xff exercises the top of the alphabet, `+` and `/` included.
        assert_eq!(base64(&[0xfb, 0xff, 0xbf]), "+/+/");
        assert_eq!(
            base64(b"tail -F /build/decoder.par.out"),
            "dGFpbCAtRiAvYnVpbGQvZGVjb2Rlci5wYXIub3V0"
        );
    }
}
