use std::io::IsTerminal;

pub const MAGENTA: &str = "\x1b[35m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const CYAN: &str = "\x1b[1;36m";
pub const BLUE: &str = "\x1b[1;34m";
pub const DIM: &str = "\x1b[2m";
pub const RED_BOLD: &str = "\x1b[1;31m";

/// Color is only emitted to a real terminal -- piping `dig -t TODO`
/// into another tool (or a file) gets plain text, same convention
/// ripgrep follows, so downstream tools never have to strip ANSI codes.
pub fn enabled() -> bool {
    std::io::stdout().is_terminal()
}

pub fn paint(code: &str, text: &str) -> String {
    if enabled() {
        format!("{code}{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

/// The selected-row treatment used across all interactive lists: bold
/// white text on a blue background, wrapped around the whole
/// already-assembled plain line in one shot. Deliberately not combined
/// with per-field inline colors (used on unselected rows) -- nesting a
/// reset code inside this would cut the background short partway
/// through the line.
pub fn highlight_row(plain_text: &str) -> String {
    if enabled() {
        format!("\x1b[1;97;44m {plain_text} \x1b[0m")
    } else {
        format!("> {plain_text}")
    }
}
