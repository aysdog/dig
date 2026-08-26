use std::io::{self, Read};

#[derive(PartialEq, Debug, Clone, Copy)]
pub enum Key {
    Up,
    Down,
    Char(char),
    Enter,
    Escape,
    /// No key arrived within the poll window. Lets the caller redraw
    /// with fresh results instead of blocking forever.
    Timeout,
}

/// Parses one keypress from any byte source, or Timeout if nothing
/// arrives. Generic over `Read` so this parsing -- the part most
/// likely to have an off-by-one in an escape sequence -- can be unit
/// tested against a plain buffer instead of a real terminal.
pub fn read_key<R: Read>(reader: &mut R) -> io::Result<Key> {
    let mut buf = [0u8; 1];
    let n = reader.read(&mut buf)?;
    if n == 0 {
        return Ok(Key::Timeout);
    }

    match buf[0] {
        b'\n' | b'\r' => Ok(Key::Enter),
        0x1b => {
            // Arrow keys arrive as the 3-byte sequence ESC '[' 'A'/'B'.
            // If no second byte arrives in time (a lone Escape
            // keypress, or the timeout firing mid-sequence), report
            // Escape rather than blocking.
            let mut next = [0u8; 1];
            let n2 = reader.read(&mut next)?;
            if n2 == 0 || next[0] != b'[' {
                return Ok(Key::Escape);
            }
            let mut arrow = [0u8; 1];
            let n3 = reader.read(&mut arrow)?;
            if n3 == 0 {
                return Ok(Key::Escape);
            }
            match arrow[0] {
                b'A' => Ok(Key::Up),
                b'B' => Ok(Key::Down),
                _ => Ok(Key::Escape),
            }
        }
        c => Ok(Key::Char(c as char)),
    }
}

/// Puts the terminal into raw mode (no line buffering, no echo) and
/// switches to the alternate screen buffer until dropped, then
/// restores both -- even on panic, so a crash never leaves the
/// terminal broken or sitting on the wrong screen.
///
/// The alternate screen matters: without it, every redraw (we clear
/// and repaint roughly 10 times a second while idle) still pushes a
/// full copy of the screen into the terminal's scrollback. Scroll up,
/// or copy from a tool that captures scrollback, and you'd see the
/// same frame duplicated dozens of times. vim, htop, and fzf all use
/// the alternate screen for exactly this reason -- it's a separate
/// buffer that's thrown away on exit, so scrollback only ever holds
/// what was there before the program started.
///
/// VMIN=0/VTIME=1 gives each read a ~100ms timeout instead of blocking
/// forever, which is what lets the UI redraw while idle.
#[cfg(unix)]
pub struct RawMode {
    original: libc::termios,
}

#[cfg(unix)]
impl RawMode {
    pub fn enable() -> io::Result<Self> {
        use std::io::Write;
        use std::os::unix::io::AsRawFd;
        let fd = io::stdin().as_raw_fd();
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut termios) != 0 {
                return Err(io::Error::last_os_error());
            }
            let original = termios;
            termios.c_lflag &= !(libc::ICANON | libc::ECHO);
            termios.c_cc[libc::VMIN] = 0;
            termios.c_cc[libc::VTIME] = 1; // ~100ms
            if libc::tcsetattr(fd, libc::TCSANOW, &termios) != 0 {
                return Err(io::Error::last_os_error());
            }
            print!("\x1b[?1049h"); // enter alternate screen
            io::stdout().flush().ok();
            Ok(RawMode { original })
        }
    }
}

#[cfg(unix)]
impl Drop for RawMode {
    fn drop(&mut self) {
        use std::io::Write;
        use std::os::unix::io::AsRawFd;
        print!("\x1b[?1049l"); // leave alternate screen, restore what was there before
        io::stdout().flush().ok();
        let fd = io::stdin().as_raw_fd();
        unsafe {
            libc::tcsetattr(fd, libc::TCSANOW, &self.original);
        }
    }
}

// Not implemented for non-Unix targets -- Windows console input needs
// a different API entirely. Falls back to a numbered menu instead.
#[cfg(not(unix))]
pub struct RawMode;

#[cfg(not(unix))]
impl RawMode {
    pub fn enable() -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "raw keyboard mode isn't implemented on this platform yet",
        ))
    }
}

/// The terminal's actual size in (rows, cols), via the same ioctl call
/// every real terminal tool uses for this. Falls back to a
/// conservative 24x80 if it can't be determined (not a real terminal,
/// or the call fails for some other reason) -- printing a fixed amount
/// of content regardless of the terminal's real size is what caused
/// results to overflow the visible screen in the first place.
#[cfg(unix)]
pub fn terminal_size() -> (u16, u16) {
    #[repr(C)]
    struct WinSize {
        rows: u16,
        cols: u16,
        _x: u16,
        _y: u16,
    }
    unsafe {
        let mut size: WinSize = std::mem::zeroed();
        let fd = std::os::unix::io::AsRawFd::as_raw_fd(&io::stdout());
        if libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) == 0 && size.rows > 0 && size.cols > 0 {
            (size.rows, size.cols)
        } else {
            (24, 80)
        }
    }
}

#[cfg(not(unix))]
pub fn terminal_size() -> (u16, u16) {
    (24, 80)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_plain_char() {
        let mut c = Cursor::new(vec![b'j']);
        assert_eq!(read_key(&mut c).unwrap(), Key::Char('j'));
    }

    #[test]
    fn parses_enter_from_newline() {
        let mut c = Cursor::new(vec![b'\n']);
        assert_eq!(read_key(&mut c).unwrap(), Key::Enter);
    }

    #[test]
    fn parses_enter_from_carriage_return() {
        let mut c = Cursor::new(vec![b'\r']);
        assert_eq!(read_key(&mut c).unwrap(), Key::Enter);
    }

    #[test]
    fn parses_up_arrow_escape_sequence() {
        let mut c = Cursor::new(vec![0x1b, b'[', b'A']);
        assert_eq!(read_key(&mut c).unwrap(), Key::Up);
    }

    #[test]
    fn parses_down_arrow_escape_sequence() {
        let mut c = Cursor::new(vec![0x1b, b'[', b'B']);
        assert_eq!(read_key(&mut c).unwrap(), Key::Down);
    }

    #[test]
    fn lone_escape_byte_reports_escape() {
        let mut c = Cursor::new(vec![0x1b]);
        assert_eq!(read_key(&mut c).unwrap(), Key::Escape);
    }

    #[test]
    fn escape_followed_by_unrecognized_sequence_reports_escape() {
        let mut c = Cursor::new(vec![0x1b, b'[', b'Z']);
        assert_eq!(read_key(&mut c).unwrap(), Key::Escape);
    }

    #[test]
    fn uppercase_and_lowercase_keys_are_distinct() {
        let mut c = Cursor::new(vec![b'X']);
        assert_eq!(read_key(&mut c).unwrap(), Key::Char('X'));
        let mut c2 = Cursor::new(vec![b'x']);
        assert_eq!(read_key(&mut c2).unwrap(), Key::Char('x'));
    }

    #[test]
    fn empty_read_reports_timeout_not_error() {
        let mut c = Cursor::new(Vec::<u8>::new());
        assert_eq!(read_key(&mut c).unwrap(), Key::Timeout);
    }

    #[test]
    fn escape_with_no_bytes_following_is_escape_not_error() {
        let mut c = Cursor::new(vec![0x1b]);
        assert_eq!(read_key(&mut c).unwrap(), Key::Escape);
    }
}
