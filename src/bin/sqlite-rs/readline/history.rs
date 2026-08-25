//! In-memory history ring plus best-effort persistence (#558).
//!
//! History file location: `$XDG_STATE_HOME/sqlite-rs/history` when
//! `$XDG_STATE_HOME` is set, else the plain dotfile
//! `~/.sqlite-rs_history`. Loading/saving is best-effort — a missing
//! `$HOME`, missing file, or unwritable path never blocks the session.

use std::fs;
use std::path::PathBuf;

pub struct History {
    entries: Vec<String>,
    /// `None` means "not currently navigating" (fresh line under edit).
    cursor: Option<usize>,
}

impl History {
    pub fn new() -> Self {
        History {
            entries: Vec::new(),
            cursor: None,
        }
    }

    pub fn add(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        // Don't duplicate the immediately preceding entry.
        if self.entries.last().map(String::as_str) != Some(line) {
            self.entries.push(line.to_string());
        }
        self.cursor = None;
    }

    /// Resets history navigation (called when a line is submitted).
    pub fn reset_cursor(&mut self) {
        self.cursor = None;
    }

    /// Moves back one entry (older), returning it, or `None` if already
    /// at the oldest entry.
    pub fn prev(&mut self) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }
        let next = match self.cursor {
            None => self.entries.len().saturating_sub(1),
            Some(i) => i.saturating_sub(1),
        };
        self.cursor = Some(next);
        self.entries.get(next).map(String::as_str)
    }

    /// Moves forward one entry (newer). Returns `Some("")` once past the
    /// newest entry (back to a fresh line), or `None` if not navigating.
    pub fn next(&mut self) -> Option<&str> {
        match self.cursor {
            None => None,
            Some(i) if i.saturating_add(1) >= self.entries.len() => {
                self.cursor = None;
                Some("")
            }
            Some(i) => {
                let next = i.saturating_add(1);
                self.cursor = Some(next);
                self.entries.get(next).map(String::as_str)
            }
        }
    }

    pub fn load(&mut self, path: &std::path::Path) {
        if let Ok(contents) = fs::read_to_string(path) {
            self.entries = contents.lines().map(String::from).collect();
        }
    }

    pub fn save(&self, path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            if fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        fs::write(path, self.entries.join("\n")).ok();
    }
}

/// See module docs: XDG state dir when set, else `~/.sqlite-rs_history`.
pub fn history_path() -> Option<PathBuf> {
    if let Some(xdg_state) = std::env::var_os("XDG_STATE_HOME") {
        if !xdg_state.is_empty() {
            return Some(PathBuf::from(xdg_state).join("sqlite-rs").join("history"));
        }
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".sqlite-rs_history"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prev_walks_back_from_newest() {
        let mut h = History::new();
        h.add("one");
        h.add("two");
        assert_eq!(h.prev(), Some("two"));
        assert_eq!(h.prev(), Some("one"));
        assert_eq!(h.prev(), Some("one")); // stays at oldest
    }

    #[test]
    fn next_returns_to_fresh_line() {
        let mut h = History::new();
        h.add("one");
        h.add("two");
        h.prev();
        h.prev();
        assert_eq!(h.next(), Some("two"));
        assert_eq!(h.next(), Some(""));
        assert_eq!(h.next(), None);
    }

    #[test]
    fn add_skips_consecutive_duplicate() {
        let mut h = History::new();
        h.add("select 1;");
        h.add("select 1;");
        assert_eq!(h.entries.len(), 1);
    }

    #[test]
    fn add_ignores_blank_lines() {
        let mut h = History::new();
        h.add("   ");
        assert!(h.entries.is_empty());
    }
}
