// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Hand-rolled line editor (#558), replacing `rustyline` (#551) so the
//! CLI binary depends on nothing but `sqlite_rs::sys` (vendored terminal
//! FFI, #563). Public surface mirrors what
//! `repl.rs` used from `rustyline::DefaultEditor`: [`Readline::new`],
//! [`Readline::read_line`], [`Readline::add_history_entry`],
//! [`Readline::load_history`]/[`Readline::save_history`].
//!
//! When stdin isn't a tty (piped scripts — every integration test in
//! this crate), [`term::RawMode::enable`] returns `None` and
//! `read_line` falls back to a plain buffered [`std::io::stdin`] line
//! read, matching the old `rustyline` behavior exactly.

mod completion;
mod highlight;
mod history;
mod line_editor;
mod term;

use std::io::{self, BufRead};
use std::path::Path;

use sqlite_rs::schema::TableSchema;

use history::History;
use line_editor::LineEditor;

pub use history::history_path;

/// Mirrors `rustyline::error::ReadlineError`'s three cases this crate
/// actually matches on.
#[derive(Debug)]
pub enum ReadlineError {
    Eof,
    Interrupted,
    Io(io::Error),
}

impl std::fmt::Display for ReadlineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadlineError::Eof => write!(f, "EOF"),
            ReadlineError::Interrupted => write!(f, "interrupted"),
            ReadlineError::Io(e) => write!(f, "{e}"),
        }
    }
}

pub struct Readline {
    history: History,
    tty: bool,
    color: bool,
}

impl Readline {
    pub fn new() -> io::Result<Self> {
        Ok(Readline {
            history: History::new(),
            tty: is_tty(),
            color: true,
        })
    }

    /// `.color on|off`: toggles ANSI syntax highlighting of the
    /// in-progress input line. Doesn't affect anything already printed.
    pub fn set_color(&mut self, enabled: bool) {
        self.color = enabled;
    }

    pub fn load_history(&mut self, path: &Path) {
        self.history.load(path);
    }

    pub fn save_history(&self, path: &Path) {
        self.history.save(path);
    }

    pub fn add_history_entry(&mut self, line: &str) {
        self.history.add(line);
    }

    /// Reads one line, with editing/history/completion/highlighting
    /// when stdin is a tty; a plain buffered read otherwise.
    /// `schemas` feeds tab completion of table/column names — pass an
    /// empty slice when none are available yet.
    pub fn read_line(
        &mut self,
        prompt: &str,
        schemas: &[TableSchema],
    ) -> Result<String, ReadlineError> {
        self.history.reset_cursor();
        if !self.tty {
            return read_line_plain(prompt);
        }
        match term::RawMode::enable() {
            Ok(Some(_guard)) => self.read_line_raw(prompt, schemas),
            Ok(None) => read_line_plain(prompt),
            Err(e) => Err(ReadlineError::Io(e)),
        }
    }

    fn read_line_raw(
        &mut self,
        prompt: &str,
        schemas: &[TableSchema],
    ) -> Result<String, ReadlineError> {
        let mut editor = LineEditor::new();
        redraw(prompt, &editor, self.color);

        loop {
            let byte = term::read_byte().map_err(ReadlineError::Io)?;
            let Some(byte) = byte else {
                return Err(ReadlineError::Eof);
            };
            match dispatch_byte(byte, &mut editor, &mut term::read_byte)? {
                Dispatch::Continue => {}
                Dispatch::Return => {
                    term::write_flush("\r\n").map_err(ReadlineError::Io)?;
                    return Ok(editor.as_str());
                }
                Dispatch::Eof => return Err(ReadlineError::Eof),
                Dispatch::Interrupted => return Err(ReadlineError::Interrupted),
                Dispatch::Tab => self.apply_completion(&mut editor, schemas),
                Dispatch::Escape(action) => {
                    apply_escape_action(&mut editor, action, &mut self.history)
                }
            }
            redraw(prompt, &editor, self.color);
        }
    }

    fn apply_completion(&mut self, editor: &mut LineEditor, schemas: &[TableSchema]) {
        let line = editor.as_str();
        let (start, mut candidates) = completion::complete(&line, editor.cursor(), schemas);
        if candidates.len() == 1 {
            let replacement = candidates.remove(0);
            let rest: String = line.chars().skip(editor.cursor()).collect();
            let prefix: String = line.chars().take(start).collect();
            editor.set(&format!("{prefix}{replacement}{rest}"));
        }
        // Multiple/no candidates: no-op for now (a future refinement
        // could print the list below the prompt).
    }
}

enum EscapeAction {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Delete,
}

/// What a single dispatched byte (already known not to be the start of
/// an escape sequence or UTF-8 continuation — those are resolved inside
/// [`dispatch_byte`] itself) means for the read loop. Kept separate from
/// directly mutating `editor`/calling I/O here so [`dispatch_byte`]
/// needs nothing but a byte, an editor, and a byte source — no `self`,
/// no real stdin/stdout — and so it's unit-testable with a synthetic
/// byte stream instead of a real terminal.
enum Dispatch {
    /// Handled in place (editor mutated, or an ignored control byte);
    /// nothing further for the caller to do but redraw.
    Continue,
    /// Enter/Return: caller flushes the trailing `\r\n` and returns the
    /// completed line.
    Return,
    Eof,
    Interrupted,
    /// Tab: caller has the `schemas`/`self` context needed for
    /// completion, which this free function deliberately doesn't.
    Tab,
    Escape(EscapeAction),
}

/// Resolves one input byte to a [`Dispatch`], reading further bytes from
/// `next_byte` only for multi-byte sequences (ESC-prefixed escape codes,
/// UTF-8 continuation bytes) — mirrors `read_line_raw`'s match exactly,
/// but as a free function taking an injectable byte source instead of
/// always calling [`term::read_byte`] directly, so it's testable without
/// a real tty.
fn dispatch_byte(
    byte: u8,
    editor: &mut LineEditor,
    next_byte: &mut impl FnMut() -> io::Result<Option<u8>>,
) -> Result<Dispatch, ReadlineError> {
    Ok(match byte {
        b'\r' | b'\n' => Dispatch::Return,
        0x04 if editor.len() == 0 => Dispatch::Eof, // Ctrl-D on empty line
        0x03 => Dispatch::Interrupted,              // Ctrl-C
        0x7f | 0x08 => {
            editor.delete_before(); // Backspace
            Dispatch::Continue
        }
        0x01 => {
            editor.move_home(); // Ctrl-A
            Dispatch::Continue
        }
        0x05 => {
            editor.move_end(); // Ctrl-E
            Dispatch::Continue
        }
        0x0b => {
            editor.delete_to_end(); // Ctrl-K
            Dispatch::Continue
        }
        0x15 => {
            editor.delete_to_home(); // Ctrl-U
            Dispatch::Continue
        }
        0x09 => Dispatch::Tab,
        0x1b => match read_escape_sequence(next_byte)? {
            Some(action) => Dispatch::Escape(action),
            None => Dispatch::Continue,
        },
        b if (0x20..0x7f).contains(&b) => {
            editor.insert(b as char);
            Dispatch::Continue
        }
        b if b >= 0x80 => {
            // Start of a multi-byte UTF-8 sequence: buffer bytes until
            // they decode, then insert the char.
            if let Some(c) = read_utf8_continuation(b, next_byte)? {
                editor.insert(c);
            }
            Dispatch::Continue
        }
        _ => Dispatch::Continue, // other control bytes ignored
    })
}

/// Reads the bytes following an ESC (`\x1b`) that make up a recognized
/// arrow/home/end/delete sequence. Returns `None` for anything else
/// (e.g. a lone Escape keypress, or an unrecognized sequence).
fn read_escape_sequence(
    next_byte: &mut impl FnMut() -> io::Result<Option<u8>>,
) -> Result<Option<EscapeAction>, ReadlineError> {
    let Some(b1) = next_byte().map_err(ReadlineError::Io)? else {
        return Ok(None);
    };
    if b1 != b'[' && b1 != b'O' {
        return Ok(None);
    }
    let Some(b2) = next_byte().map_err(ReadlineError::Io)? else {
        return Ok(None);
    };
    Ok(match b2 {
        b'A' => Some(EscapeAction::Up),
        b'B' => Some(EscapeAction::Down),
        b'C' => Some(EscapeAction::Right),
        b'D' => Some(EscapeAction::Left),
        b'H' => Some(EscapeAction::Home),
        b'F' => Some(EscapeAction::End),
        b'3' => {
            // Delete key: `\x1b[3~`.
            let _ = next_byte().map_err(ReadlineError::Io)?;
            Some(EscapeAction::Delete)
        }
        _ => None,
    })
}

fn apply_escape_action(editor: &mut LineEditor, action: EscapeAction, history: &mut History) {
    match action {
        EscapeAction::Up => {
            if let Some(entry) = history.prev() {
                editor.set(entry);
            }
        }
        EscapeAction::Down => {
            if let Some(entry) = history.next() {
                editor.set(entry);
            }
        }
        EscapeAction::Left => editor.move_left(),
        EscapeAction::Right => editor.move_right(),
        EscapeAction::Home => editor.move_home(),
        EscapeAction::End => editor.move_end(),
        EscapeAction::Delete => editor.delete_at(),
    }
}

fn read_utf8_continuation(
    first: u8,
    next_byte: &mut impl FnMut() -> io::Result<Option<u8>>,
) -> Result<Option<char>, ReadlineError> {
    let expected_len = if first & 0xE0 == 0xC0 {
        2
    } else if first & 0xF0 == 0xE0 {
        3
    } else if first & 0xF8 == 0xF0 {
        4
    } else {
        return Ok(None); // invalid leading byte
    };
    let mut buf = vec![first];
    for _ in 1..expected_len {
        match next_byte().map_err(ReadlineError::Io)? {
            Some(b) => buf.push(b),
            None => return Ok(None),
        }
    }
    Ok(std::str::from_utf8(&buf)
        .ok()
        .and_then(|s| s.chars().next()))
}

fn redraw(prompt: &str, editor: &LineEditor, color: bool) {
    let line = editor.as_str();
    let highlighted = if color {
        highlight::highlight(&line)
    } else {
        line.clone()
    };
    let out = format!(
        "\r{}{}{}{}",
        prompt,
        highlighted,
        term::CLEAR_TO_EOL,
        term::cursor_to_col(prompt.chars().count().saturating_add(editor.cursor()))
    );
    term::write_flush(&out).ok();
}

fn is_tty() -> bool {
    // Best-effort: raw-mode enable itself is the authoritative check
    // (`tcgetattr` fails on a non-tty); this just short-circuits before
    // touching the terminal at all when stdin is obviously piped.
    use std::os::fd::AsFd;
    sqlite_rs::sys::termios::is_tty(io::stdin().as_fd())
}

fn read_line_plain(prompt: &str) -> Result<String, ReadlineError> {
    term::write_flush(prompt).map_err(ReadlineError::Io)?;
    let mut line = String::new();
    let n = io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(ReadlineError::Io)?;
    if n == 0 {
        return Err(ReadlineError::Eof);
    }
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::arithmetic_side_effects
    )]

    use super::*;

    /// Builds a `next_byte` closure that yields `bytes` in order, then
    /// `Ok(None)` (simulated EOF) forever after — a synthetic stand-in
    /// for `term::read_byte`, so the escape/UTF-8 decoders and the
    /// dispatch table run against a scripted byte stream instead of a
    /// real tty.
    fn byte_source(bytes: &[u8]) -> impl FnMut() -> io::Result<Option<u8>> + '_ {
        let mut iter = bytes.iter().copied();
        move || Ok(iter.next())
    }

    #[test]
    fn escape_sequence_decodes_arrow_keys() {
        let mut src = byte_source(b"[A");
        assert!(matches!(
            read_escape_sequence(&mut src),
            Ok(Some(EscapeAction::Up))
        ));

        let mut src = byte_source(b"[B");
        assert!(matches!(
            read_escape_sequence(&mut src),
            Ok(Some(EscapeAction::Down))
        ));

        let mut src = byte_source(b"[C");
        assert!(matches!(
            read_escape_sequence(&mut src),
            Ok(Some(EscapeAction::Right))
        ));

        let mut src = byte_source(b"[D");
        assert!(matches!(
            read_escape_sequence(&mut src),
            Ok(Some(EscapeAction::Left))
        ));
    }

    #[test]
    fn escape_sequence_decodes_home_end_and_delete() {
        let mut src = byte_source(b"[H");
        assert!(matches!(
            read_escape_sequence(&mut src),
            Ok(Some(EscapeAction::Home))
        ));

        let mut src = byte_source(b"[F");
        assert!(matches!(
            read_escape_sequence(&mut src),
            Ok(Some(EscapeAction::End))
        ));

        // Delete key: `\x1b[3~` — the trailing `~` is consumed and
        // discarded, not itself inspected.
        let mut src = byte_source(b"[3~");
        assert!(matches!(
            read_escape_sequence(&mut src),
            Ok(Some(EscapeAction::Delete))
        ));
    }

    #[test]
    fn escape_sequence_accepts_ss3_prefix() {
        // Some terminals send `ESC O <letter>` (SS3) instead of `ESC [
        // <letter>` (CSI) for the same arrow keys.
        let mut src = byte_source(b"OA");
        assert!(matches!(
            read_escape_sequence(&mut src),
            Ok(Some(EscapeAction::Up))
        ));
    }

    #[test]
    fn escape_sequence_rejects_unrecognized_bytes() {
        // Neither CSI (`[`) nor SS3 (`O`) — a lone Escape keypress
        // followed by something else entirely.
        let mut src = byte_source(b"Zq");
        assert!(matches!(read_escape_sequence(&mut src), Ok(None)));

        // Recognized prefix, unrecognized final byte.
        let mut src = byte_source(b"[Z");
        assert!(matches!(read_escape_sequence(&mut src), Ok(None)));
    }

    #[test]
    fn escape_sequence_truncated_mid_sequence_is_none_not_err() {
        // A lone ESC with nothing following (e.g. Escape pressed right
        // before Ctrl-D/EOF) must not be treated as an error.
        let mut src = byte_source(b"");
        assert!(matches!(read_escape_sequence(&mut src), Ok(None)));

        // CSI prefix present, but the stream ends before the final byte.
        let mut src = byte_source(b"[");
        assert!(matches!(read_escape_sequence(&mut src), Ok(None)));
    }

    #[test]
    fn utf8_continuation_decodes_two_three_and_four_byte_sequences() {
        // 'é' (U+00E9) as UTF-8: 0xC3 0xA9.
        let mut src = byte_source(&[0xA9]);
        assert_eq!(read_utf8_continuation(0xC3, &mut src).unwrap(), Some('é'));

        // '€' (U+20AC) as UTF-8: 0xE2 0x82 0xAC.
        let mut src = byte_source(&[0x82, 0xAC]);
        assert_eq!(read_utf8_continuation(0xE2, &mut src).unwrap(), Some('€'));

        // '🦀' (U+1F980) as UTF-8: 0xF0 0x9F 0xA6 0x80.
        let mut src = byte_source(&[0x9F, 0xA6, 0x80]);
        assert_eq!(read_utf8_continuation(0xF0, &mut src).unwrap(), Some('🦀'));
    }

    #[test]
    fn utf8_continuation_rejects_invalid_leading_byte() {
        // 0x80..0xBF are continuation bytes, never valid as a *leading*
        // byte of a new sequence.
        let mut src = byte_source(&[]);
        assert_eq!(read_utf8_continuation(0x80, &mut src).unwrap(), None);
    }

    #[test]
    fn utf8_continuation_truncated_stream_is_none_not_err() {
        // A two-byte sequence's leading byte with nothing following
        // (e.g. a multi-byte character split across a `read()` boundary
        // right before EOF).
        let mut src = byte_source(&[]);
        assert_eq!(read_utf8_continuation(0xC3, &mut src).unwrap(), None);
    }

    #[test]
    fn utf8_continuation_rejects_malformed_continuation_bytes() {
        // Leading byte announces a 2-byte sequence, but the next byte
        // isn't a valid UTF-8 continuation byte at all — `from_utf8`
        // fails and this must report `None`, never panic.
        let mut src = byte_source(&[0x00]);
        assert_eq!(read_utf8_continuation(0xC3, &mut src).unwrap(), None);
    }

    #[test]
    fn dispatch_return_on_enter_or_newline() {
        let mut editor = LineEditor::new();
        let mut src = byte_source(&[]);
        assert!(matches!(
            dispatch_byte(b'\r', &mut editor, &mut src),
            Ok(Dispatch::Return)
        ));
        assert!(matches!(
            dispatch_byte(b'\n', &mut editor, &mut src),
            Ok(Dispatch::Return)
        ));
    }

    #[test]
    fn dispatch_eof_only_on_ctrl_d_with_empty_line() {
        let mut empty = LineEditor::new();
        let mut src = byte_source(&[]);
        assert!(matches!(
            dispatch_byte(0x04, &mut empty, &mut src),
            Ok(Dispatch::Eof)
        ));

        let mut nonempty = LineEditor::new();
        nonempty.insert('x');
        let mut src = byte_source(&[]);
        // Ctrl-D on a non-empty line is not EOF — real readline
        // implementations (and this one) only treat it as EOF when
        // there's nothing to delete.
        assert!(matches!(
            dispatch_byte(0x04, &mut nonempty, &mut src),
            Ok(Dispatch::Continue)
        ));
    }

    #[test]
    fn dispatch_interrupted_on_ctrl_c() {
        let mut editor = LineEditor::new();
        let mut src = byte_source(&[]);
        assert!(matches!(
            dispatch_byte(0x03, &mut editor, &mut src),
            Ok(Dispatch::Interrupted)
        ));
    }

    #[test]
    fn dispatch_tab_is_left_to_the_caller() {
        let mut editor = LineEditor::new();
        let mut src = byte_source(&[]);
        assert!(matches!(
            dispatch_byte(0x09, &mut editor, &mut src),
            Ok(Dispatch::Tab)
        ));
    }

    #[test]
    fn dispatch_editing_keys_mutate_the_editor_in_place() {
        let mut editor = LineEditor::new();
        editor.insert('a');
        editor.insert('b');
        editor.insert('c');
        let mut src = byte_source(&[]);

        // Backspace.
        assert!(matches!(
            dispatch_byte(0x7f, &mut editor, &mut src),
            Ok(Dispatch::Continue)
        ));
        assert_eq!(editor.as_str(), "ab");

        // Ctrl-A (home), then a printable char inserts at position 0.
        assert!(matches!(
            dispatch_byte(0x01, &mut editor, &mut src),
            Ok(Dispatch::Continue)
        ));
        assert!(matches!(
            dispatch_byte(b'z', &mut editor, &mut src),
            Ok(Dispatch::Continue)
        ));
        assert_eq!(editor.as_str(), "zab");
    }

    #[test]
    fn dispatch_ctrl_k_and_ctrl_u_clear_to_end_and_home() {
        let mut editor = LineEditor::new();
        editor.insert('a');
        editor.insert('b');
        editor.insert('c');
        let mut src = byte_source(&[]);

        editor.move_home();
        editor.move_right(); // cursor after 'a'
        assert!(matches!(
            dispatch_byte(0x0b, &mut editor, &mut src), // Ctrl-K
            Ok(Dispatch::Continue)
        ));
        assert_eq!(editor.as_str(), "a");

        editor.set("abc");
        editor.move_end();
        assert!(matches!(
            dispatch_byte(0x15, &mut editor, &mut src), // Ctrl-U
            Ok(Dispatch::Continue)
        ));
        assert_eq!(editor.as_str(), "");
    }

    #[test]
    fn dispatch_escape_byte_yields_decoded_action() {
        let mut editor = LineEditor::new();
        let mut src = byte_source(b"[C"); // Right arrow
        assert!(matches!(
            dispatch_byte(0x1b, &mut editor, &mut src),
            Ok(Dispatch::Escape(EscapeAction::Right))
        ));
    }

    #[test]
    fn dispatch_unrecognized_escape_sequence_is_continue_not_error() {
        let mut editor = LineEditor::new();
        let mut src = byte_source(b"Zq");
        assert!(matches!(
            dispatch_byte(0x1b, &mut editor, &mut src),
            Ok(Dispatch::Continue)
        ));
    }

    #[test]
    fn dispatch_multibyte_utf8_inserts_the_decoded_char() {
        let mut editor = LineEditor::new();
        let mut src = byte_source(&[0xA9]); // continuation of 'é'
        assert!(matches!(
            dispatch_byte(0xC3, &mut editor, &mut src),
            Ok(Dispatch::Continue)
        ));
        assert_eq!(editor.as_str(), "é");
    }

    #[test]
    fn dispatch_other_control_bytes_are_ignored() {
        let mut editor = LineEditor::new();
        let mut src = byte_source(&[]);
        // e.g. Ctrl-L (form feed) — not bound to anything.
        assert!(matches!(
            dispatch_byte(0x0c, &mut editor, &mut src),
            Ok(Dispatch::Continue)
        ));
        assert_eq!(editor.as_str(), "");
    }

    #[test]
    fn apply_escape_action_moves_and_edits() {
        let mut history = History::new();
        let mut editor = LineEditor::new();
        editor.set("abc");
        editor.move_end();

        apply_escape_action(&mut editor, EscapeAction::Left, &mut history);
        apply_escape_action(&mut editor, EscapeAction::Delete, &mut history);
        assert_eq!(editor.as_str(), "ab");

        apply_escape_action(&mut editor, EscapeAction::Home, &mut history);
        assert_eq!(editor.cursor(), 0);

        apply_escape_action(&mut editor, EscapeAction::End, &mut history);
        assert_eq!(editor.cursor(), 2);
    }

    #[test]
    fn apply_escape_action_up_and_down_navigate_history() {
        let mut history = History::new();
        history.add("first");
        history.add("second");
        let mut editor = LineEditor::new();

        apply_escape_action(&mut editor, EscapeAction::Up, &mut history);
        assert_eq!(editor.as_str(), "second");

        apply_escape_action(&mut editor, EscapeAction::Up, &mut history);
        assert_eq!(editor.as_str(), "first");

        apply_escape_action(&mut editor, EscapeAction::Down, &mut history);
        assert_eq!(editor.as_str(), "second");
    }
}
