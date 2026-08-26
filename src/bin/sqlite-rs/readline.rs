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
}

impl Readline {
    pub fn new() -> io::Result<Self> {
        Ok(Readline {
            history: History::new(),
            tty: is_tty(),
        })
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
        redraw(prompt, &editor);

        loop {
            let byte = term::read_byte().map_err(ReadlineError::Io)?;
            let Some(byte) = byte else {
                return Err(ReadlineError::Eof);
            };
            match byte {
                b'\r' | b'\n' => {
                    term::write_flush("\r\n").map_err(ReadlineError::Io)?;
                    return Ok(editor.as_str());
                }
                0x04 if editor.len() == 0 => return Err(ReadlineError::Eof), // Ctrl-D on empty line
                0x03 => return Err(ReadlineError::Interrupted),              // Ctrl-C
                0x7f | 0x08 => editor.delete_before(),                       // Backspace
                0x01 => editor.move_home(),                                  // Ctrl-A
                0x05 => editor.move_end(),                                   // Ctrl-E
                0x0b => editor.delete_to_end(),                              // Ctrl-K
                0x15 => editor.delete_to_home(),                             // Ctrl-U
                0x09 => self.apply_completion(&mut editor, schemas),         // Tab
                0x1b => {
                    if let Some(action) = read_escape_sequence()? {
                        apply_escape_action(&mut editor, action, &mut self.history);
                    }
                }
                b if (0x20..0x7f).contains(&b) => editor.insert(b as char),
                b if b >= 0x80 => {
                    // Start of a multi-byte UTF-8 sequence: buffer bytes
                    // until they decode, then insert the char.
                    if let Some(c) = read_utf8_continuation(b)? {
                        editor.insert(c);
                    }
                }
                _ => {} // other control bytes ignored
            }
            redraw(prompt, &editor);
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

/// Reads the bytes following an ESC (`\x1b`) that make up a recognized
/// arrow/home/end/delete sequence. Returns `None` for anything else
/// (e.g. a lone Escape keypress, or an unrecognized sequence).
fn read_escape_sequence() -> Result<Option<EscapeAction>, ReadlineError> {
    let Some(b1) = term::read_byte().map_err(ReadlineError::Io)? else {
        return Ok(None);
    };
    if b1 != b'[' && b1 != b'O' {
        return Ok(None);
    }
    let Some(b2) = term::read_byte().map_err(ReadlineError::Io)? else {
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
            let _ = term::read_byte().map_err(ReadlineError::Io)?;
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

fn read_utf8_continuation(first: u8) -> Result<Option<char>, ReadlineError> {
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
        match term::read_byte().map_err(ReadlineError::Io)? {
            Some(b) => buf.push(b),
            None => return Ok(None),
        }
    }
    Ok(std::str::from_utf8(&buf)
        .ok()
        .and_then(|s| s.chars().next()))
}

fn redraw(prompt: &str, editor: &LineEditor) {
    let line = editor.as_str();
    let highlighted = highlight::highlight(&line);
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
