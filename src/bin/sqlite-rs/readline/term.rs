//! Raw-mode terminal handling and ANSI escape helpers (#558). Raw mode
//! disables line buffering/canonical processing so the line editor sees
//! every keypress (including arrows, which arrive as multi-byte escape
//! sequences) instead of a whole line at a time.

use std::io::{self, Read, Write};
use std::os::fd::BorrowedFd;

use sqlite_rs::sys::termios::{self, termios as Termios, SetArg};

/// stdin's file descriptor is always 0 on POSIX; borrowing it directly
/// (rather than through `io::stdin()`) avoids taking ownership of — and
/// so never risks closing — the process's actual stdin.
const STDIN_FD: i32 = 0;

/// Guard that restores the terminal's original mode on drop, so a panic
/// or early return never leaves the user's shell in raw mode.
pub struct RawMode {
    original: Termios,
}

impl RawMode {
    /// Puts stdin into raw mode. Returns `None` when stdin isn't a tty
    /// (piped input) — callers fall back to plain line reads in that
    /// case, same as the old `rustyline` behavior.
    pub fn enable() -> io::Result<Option<Self>> {
        let fd = unsafe { BorrowedFd::borrow_raw(STDIN_FD) };
        let original = match termios::tcgetattr_call(fd) {
            Ok(t) => t,
            Err(_) => return Ok(None), // not a tty
        };
        let mut raw = original;
        termios::cfmakeraw_call(&mut raw);
        termios::tcsetattr_call(fd, SetArg::TCSAFLUSH, &raw)?;
        Ok(Some(RawMode { original }))
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let fd = unsafe { BorrowedFd::borrow_raw(STDIN_FD) };
        termios::tcsetattr_call(fd, SetArg::TCSAFLUSH, &self.original).ok();
    }
}

/// Reads a single byte from stdin, blocking. Returns `None` on EOF.
pub fn read_byte() -> io::Result<Option<u8>> {
    let mut buf = [0u8; 1];
    loop {
        match io::stdin().read(&mut buf) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(buf[0])),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Writes `s` to stdout and flushes immediately — the line editor needs
/// every redraw to land before the next keypress is read.
pub fn write_flush(s: &str) -> io::Result<()> {
    let mut stdout = io::stdout();
    stdout.write_all(s.as_bytes())?;
    stdout.flush()
}

pub const RESET: &str = "\x1b[0m";
pub const BOLD_BLUE: &str = "\x1b[1;34m";
pub const GREEN: &str = "\x1b[32m";
pub const CYAN: &str = "\x1b[36m";
pub const GRAY: &str = "\x1b[90m";
pub const YELLOW: &str = "\x1b[33m";

/// Moves the cursor to column `col` (0-based) of the current line.
pub fn cursor_to_col(col: usize) -> String {
    format!("\r\x1b[{}C", col)
}

/// Clears from the cursor to the end of the line.
pub const CLEAR_TO_EOL: &str = "\x1b[K";
