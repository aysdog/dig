mod color;
mod config;
mod content_search;
mod find;
mod keyboard;
mod matcher;
mod process;
mod walker;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_help();
        return;
    }

    if args[0] == "-config" || args[0] == "--config" {
        config::run_wizard();
        return;
    }

    let (whole_system, args) = strip_flag(args, &["-a", "--all"]);
    let (case_sensitive, args) = strip_flag(args, &["-c", "--case-sensitive"]);
    let (use_regex, args) = strip_flag(args, &["-e", "--regex"]);
    let (mode, words) = parse_mode(&args);

    match mode {
        Mode::Process => run_process_mode(),
        Mode::Files => {
            if words.is_empty() {
                eprintln!("usage: dig -f <word> [word...]");
                std::process::exit(1);
            }
            run_file_search(&words, case_sensitive, whole_system);
        }
        Mode::Text => {
            let (root_override, pattern_words) = split_optional_dir(words);
            if pattern_words.is_empty() {
                print_help();
                return;
            }
            run_content_search(
                &pattern_words.join(" "),
                root_override,
                whole_system,
                use_regex,
                case_sensitive,
            );
        }
    }
}

/// Pulls any of `aliases` out of the argument list, wherever they
/// appear, so flags can combine in any order (`dig -a -c foo`, `dig -c
/// -a foo`, both work the same).
fn strip_flag(args: Vec<String>, aliases: &[&str]) -> (bool, Vec<String>) {
    let mut found = false;
    let rest: Vec<String> = args
        .into_iter()
        .filter(|a| {
            if aliases.contains(&a.as_str()) {
                found = true;
                false
            } else {
                true
            }
        })
        .collect();
    (found, rest)
}

/// The default search root: your current directory, like ripgrep --
/// not your whole home directory. Pass `-a`/`--all` to search the
/// entire computer instead.
fn search_root(whole_system: bool) -> io::Result<PathBuf> {
    if whole_system {
        Ok(PathBuf::from("/"))
    } else {
        std::env::current_dir()
    }
}

/// If the last word is an existing directory, treat it as an explicit
/// root override and drop it from the pattern words -- lets `dig TODO
/// ~/work` search a specific tree regardless of -a or cwd.
fn split_optional_dir(words: Vec<String>) -> (Option<PathBuf>, Vec<String>) {
    if let Some(last) = words.last() {
        let candidate = PathBuf::from(last);
        if candidate.is_dir() {
            let mut rest = words;
            rest.pop();
            return (Some(candidate), rest);
        }
    }
    (None, words)
}

enum Mode {
    Process,
    Files,
    Text,
}

/// No flag means text search -- that's the default, everyday case,
/// same as running plain `rg`. `-f` searches filenames instead, `-p`
/// shows processes.
fn parse_mode(args: &[String]) -> (Mode, Vec<String>) {
    if args.is_empty() {
        return (Mode::Text, Vec::new());
    }
    match args[0].as_str() {
        "-p" | "--proc" => (Mode::Process, args[1..].to_vec()),
        "-f" | "--files" => (Mode::Files, args[1..].to_vec()),
        _ => (Mode::Text, args.to_vec()),
    }
}

fn print_help() {
    println!("dig - search file contents, find files fast, or check what's eating your system");
    println!();
    println!("usage:");
    println!("  dig <pattern> [directory]     search file contents (default, like rg)");
    println!("  dig -f <word> [word...]       search filenames instead, fuzzy by default");
    println!("  dig -p                        show running processes, grouped by category");
    println!();
    println!("examples:");
    println!("  dig TODO                  search file contents for \"TODO\" in this directory");
    println!("  dig TODO ~/work           search file contents inside a specific directory");
    println!("  dig -a TODO               search file contents across the whole computer");
    println!("  dig -c TODO               case-sensitive text search (default is case-insensitive)");
    println!("  dig -e '\\d{{3,}}'           search using a real regex instead of a literal string");
    println!("  dig -f resume             fuzzy filename search -- typos are fine");
    println!("  dig -f -c resume          exact, case-sensitive filename search");
    println!();
    println!("content search respects .gitignore automatically, the same as ripgrep.");
    println!();
    println!("interactive controls (content and file search):");
    println!("  j/k move   n next batch   c copy path   q back");
    println!("  content search also: o open in editor");
    println!("  file search also:    f reveal in file manager");
    println!();
    println!("interactive controls (process mode):");
    println!("  j/k move   x kill process   q back");
    println!("  the list refreshes on its own every couple of seconds");
    println!();
    println!("dig -config    set your default code editor and file manager");
}

// --- content search mode ---

fn run_content_search(
    pattern: &str,
    root_override: Option<PathBuf>,
    whole_system: bool,
    use_regex: bool,
    case_sensitive: bool,
) {
    let root = match root_override {
        Some(r) => r,
        None => match search_root(whole_system) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("dig: can't determine search directory: {e}");
                std::process::exit(1);
            }
        },
    };

    let receiver = match content_search::search(&root, pattern, use_regex, case_sensitive) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("dig: invalid pattern: {e}");
            std::process::exit(1);
        }
    };

    match keyboard::RawMode::enable() {
        Ok(_raw) => interactive_content_ui(receiver),
        Err(_) => content_search_fallback(receiver),
    }
}

/// Live j/k/o/c/n/q control. Results stream in from the background
/// search and the list grows in place instead of blocking on a "please
/// wait" screen.
/// Rows reserved for the header/footer chrome around the match list
/// (search indicator, "Showing X-Y of Z" line, blank spacers, controls,
/// status line) -- kept as a flat margin so the list itself never
/// prints more than what will actually fit on screen.
const CONTENT_UI_RESERVED_ROWS: usize = 6;

/// Groups consecutive same-path matches under one header when
/// rendering (like ripgrep), so a file with many hits costs one header
/// row plus one row per match instead of repeating the full path every
/// time. Returns the index just past the last match that fits within
/// `available_rows`, starting from `start`. Always advances by at
/// least one match, even if it technically overflows, so a page is
/// never empty.
fn content_page_end(matches: &[content_search::Match], start: usize, available_rows: usize) -> usize {
    let mut rows_used = 0usize;
    let mut idx = start;
    let mut last_path: Option<&std::path::Path> = None;

    while idx < matches.len() {
        let m = &matches[idx];
        let is_new_file = last_path != Some(m.path.as_path());
        let cost = if is_new_file {
            if last_path.is_some() {
                3 // blank separator + header + match line
            } else {
                2 // header + match line, no leading blank for the first group
            }
        } else {
            1 // just the match line, header already shown
        };
        if rows_used + cost > available_rows {
            break;
        }
        rows_used += cost;
        last_path = Some(m.path.as_path());
        idx += 1;
    }

    if idx == start && start < matches.len() {
        idx = start + 1;
    }
    idx
}

fn interactive_content_ui(receiver: mpsc::Receiver<content_search::Match>) {
    let mut matches: Vec<content_search::Match> = Vec::new();
    let mut searching = true;
    let mut start = 0usize;
    let mut page_starts: Vec<usize> = Vec::new();
    let mut selected = 0usize;
    let mut status: Option<String> = None;

    let stdin = io::stdin();
    let mut lock = stdin.lock();

    loop {
        loop {
            match receiver.try_recv() {
                Ok(m) => matches.push(m),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    searching = false;
                    break;
                }
            }
        }

        if matches.is_empty() && !searching {
            print!("\x1b[2J\x1b[H");
            println!("no matches found");
            io::stdout().flush().ok();
            return;
        }

        let (term_rows, _) = keyboard::terminal_size();
        let available_rows = (term_rows as usize).saturating_sub(CONTENT_UI_RESERVED_ROWS).max(3);
        let end = content_page_end(&matches, start, available_rows);
        let page_len = end.saturating_sub(start);
        if selected >= page_len && page_len > 0 {
            selected = page_len - 1;
        }

        draw_content_page(&matches, start, end, selected, available_rows, searching, status.take());

        let key = match keyboard::read_key(&mut lock) {
            Ok(k) => k,
            Err(_) => break,
        };

        match key {
            keyboard::Key::Timeout => {}
            keyboard::Key::Char('j') | keyboard::Key::Down => {
                if selected + 1 < page_len {
                    selected += 1;
                }
            }
            keyboard::Key::Char('k') | keyboard::Key::Up => {
                if selected > 0 {
                    selected -= 1;
                }
            }
            keyboard::Key::Char('n') => {
                if end < matches.len() {
                    page_starts.push(start);
                    start = end;
                    selected = 0;
                } else {
                    status = Some(if searching {
                        "no more results yet -- still searching".to_string()
                    } else {
                        "no more results".to_string()
                    });
                }
            }
            keyboard::Key::Char('N') => {
                if let Some(prev) = page_starts.pop() {
                    start = prev;
                    selected = 0;
                }
            }
            keyboard::Key::Char('c') => {
                if let Some(m) = matches.get(start + selected) {
                    status = Some(match copy_to_clipboard(&m.path.to_string_lossy()) {
                        Ok(()) => "copied path to clipboard".to_string(),
                        Err(e) => format!("couldn't copy: {e}"),
                    });
                }
            }
            keyboard::Key::Char('o') => {
                if let Some(m) = matches.get(start + selected) {
                    status = Some(match open_in_editor(&m.path, m.line) {
                        Ok(editor) => format!("opened in {editor}"),
                        Err(e) => format!("couldn't open editor: {e}"),
                    });
                }
            }
            keyboard::Key::Char('q') | keyboard::Key::Escape => break,
            _ => {}
        }
    }

    print!("\x1b[2J\x1b[H");
    io::stdout().flush().ok();
}

fn draw_content_page(
    matches: &[content_search::Match],
    start: usize,
    end: usize,
    selected: usize,
    available_rows: usize,
    searching: bool,
    status: Option<String>,
) {
    print!("\x1b[2J\x1b[H");

    if matches.is_empty() && searching {
        println!("{}", color::paint(color::DIM, "searching..."));
        println!();
    }

    let mut last_path: Option<&std::path::Path> = None;
    for (i, m) in matches[start..end].iter().enumerate() {
        let is_new_file = last_path != Some(m.path.as_path());
        if is_new_file {
            if last_path.is_some() {
                println!();
            }
            println!("{}", color::paint(color::MAGENTA, &m.path.display().to_string()));
        }

        if i == selected {
            println!("{}", color::highlight_row(&format!("{}: {}", m.line, m.text)));
        } else {
            let line_str = color::paint(color::GREEN, &m.line.to_string());
            let body = highlight(&m.text, &m.spans, color::enabled());
            println!("  {line_str}: {body}");
        }
        last_path = Some(m.path.as_path());
    }

    let more = if end < matches.len() {
        "(more available, press n)"
    } else if searching {
        "(still searching...)"
    } else {
        ""
    };
    if !matches.is_empty() {
        println!();
        println!("Showing matches {}-{} of {} {more}", start + 1, end, matches.len());
    }
    println!("j/k move   o open in editor   c copy path   n next batch   q back");
    if let Some(s) = status {
        println!("{s}");
    }
    let _ = available_rows; // reserved for future use (e.g. debug display)
    io::stdout().flush().ok();
}

/// Numbered-menu fallback for platforms without raw terminal mode.
fn content_search_fallback(receiver: mpsc::Receiver<content_search::Match>) {
    let matches: Vec<content_search::Match> = receiver.into_iter().collect();

    if matches.is_empty() {
        println!("no matches found");
        return;
    }

    let shown = matches.len().min(find::MAX_RESULTS);
    for (i, m) in matches[..shown].iter().enumerate() {
        println!("{}. {}:{}: {}", i + 1, m.path.display(), m.line, m.text.trim());
    }
    if matches.len() > shown {
        println!("Showing first {shown} results...");
    }

    let selected = match prompt_selection(shown) {
        Some(i) => i,
        None => return,
    };

    println!("1. Copy path to clipboard");
    println!("2. Open in editor");
    println!("3. Cancel");
    print!("Choose an option: ");
    io::stdout().flush().ok();

    let m = &matches[selected];
    match read_line().trim() {
        "1" => match copy_to_clipboard(&m.path.to_string_lossy()) {
            Ok(()) => println!("path copied to clipboard"),
            Err(e) => eprintln!("dig: couldn't copy to clipboard: {e}"),
        },
        "2" => match open_in_editor(&m.path, m.line) {
            Ok(editor) => println!("opened in {editor}"),
            Err(e) => eprintln!("dig: couldn't open editor: {e}"),
        },
        _ => {}
    }
}

/// Wraps every matched span of `text` in color, left to right, or
/// returns it untouched if `colorize` is false. Falls back to plain
/// text on a bad UTF-8 boundary instead of risking a panic.
fn highlight(text: &str, spans: &[(usize, usize)], colorize: bool) -> String {
    if !colorize || spans.is_empty() {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len() + spans.len() * 12);
    let mut cursor = 0;

    for &(start, len) in spans {
        let (Some(before), Some(matched)) = (text.get(cursor..start), text.get(start..start + len))
        else {
            return text.to_string(); // bad boundary somewhere: safe fallback
        };
        out.push_str(before);
        out.push_str(color::RED_BOLD);
        out.push_str(matched);
        out.push_str("\x1b[0m");
        cursor = start + len;
    }

    match text.get(cursor..) {
        Some(rest) => out.push_str(rest),
        None => return text.to_string(),
    }

    out
}

/// Opens `path` at `line` in the user's editor: the configured editor
/// from `dig -config` if set, then `$EDITOR`, then the first of a few
/// common editors found on PATH. Returns the editor name used.
fn open_in_editor(path: &std::path::Path, line: usize) -> io::Result<String> {
    let cfg = config::load();
    let editor = cfg.code_editor.or_else(|| std::env::var("EDITOR").ok()).or_else(|| {
        ["code", "subl", "nvim", "vim", "nano"]
            .into_iter()
            .find(|e| command_exists(e))
            .map(str::to_string)
    });

    let Some(editor) = editor else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no editor found -- set one with 'dig -config', $EDITOR, or install code/subl/vim/nano",
        ));
    };

    launch_editor(&editor, path, line)?;
    Ok(editor)
}

fn command_exists(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Different editors take different syntax for "open at this line."
/// Unrecognized editors just get the bare path -- best effort.
fn launch_editor(editor: &str, path: &std::path::Path, line: usize) -> io::Result<()> {
    let name = std::path::Path::new(editor)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(editor);

    let mut cmd = Command::new(editor);
    match name {
        "code" | "code-insiders" => {
            cmd.arg("--goto").arg(format!("{}:{line}", path.display()));
        }
        "subl" | "sublime_text" => {
            cmd.arg(format!("{}:{line}", path.display()));
        }
        "vim" | "nvim" | "vi" | "nano" | "emacs" => {
            cmd.arg(format!("+{line}")).arg(path);
        }
        _ => {
            cmd.arg(path);
        }
    }
    cmd.status()?;
    Ok(())
}

// --- file search mode ---

fn run_file_search(words: &[String], case_sensitive: bool, whole_system: bool) {
    let root = match search_root(whole_system) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("dig: can't determine search directory: {e}");
            std::process::exit(1);
        }
    };

    let receiver = find::search_stream(&root, words, case_sensitive);

    match keyboard::RawMode::enable() {
        Ok(_raw) => interactive_file_ui(receiver),
        Err(_) => file_search_fallback(receiver),
    }
}

/// Live j/k/f/c/n/q control. Results stream in from the background
/// search and the list grows in place.
/// Rows reserved for header/footer chrome, same reasoning as content
/// search's version.
const FILE_UI_RESERVED_ROWS: usize = 6;

fn interactive_file_ui(receiver: mpsc::Receiver<find::Match>) {
    let mut matches: Vec<find::Match> = Vec::new();
    let mut searching = true;
    let mut page = 0usize;
    let mut selected = 0usize;
    let mut status: Option<String> = None;

    let stdin = io::stdin();
    let mut lock = stdin.lock();

    loop {
        let mut grew = false;
        loop {
            match receiver.try_recv() {
                Ok(m) => {
                    matches.push(m);
                    grew = true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    searching = false;
                    break;
                }
            }
        }
        if grew {
            matches.sort_by(|a, b| b.score.cmp(&a.score));
        }

        if matches.is_empty() && !searching {
            print!("\x1b[2J\x1b[H");
            println!("no matching files found");
            io::stdout().flush().ok();
            return;
        }

        let (term_rows, _) = keyboard::terminal_size();
        let available_rows = (term_rows as usize).saturating_sub(FILE_UI_RESERVED_ROWS).max(2);
        let page_size = (available_rows / 2).max(1); // each entry costs a content row + a blank spacer

        let max_page = matches.len().saturating_sub(1) / page_size;
        let start = page * page_size;
        let end = (start + page_size).min(matches.len());
        let page_len = end.saturating_sub(start);
        if selected >= page_len && page_len > 0 {
            selected = page_len - 1;
        }

        draw_file_page(&matches, start, end, selected, page, max_page, searching, status.take());

        let key = match keyboard::read_key(&mut lock) {
            Ok(k) => k,
            Err(_) => break,
        };

        match key {
            keyboard::Key::Timeout => {}
            keyboard::Key::Char('j') | keyboard::Key::Down => {
                if selected + 1 < page_len {
                    selected += 1;
                }
            }
            keyboard::Key::Char('k') | keyboard::Key::Up => {
                if selected > 0 {
                    selected -= 1;
                }
            }
            keyboard::Key::Char('n') => {
                if page < max_page {
                    page += 1;
                    selected = 0;
                } else {
                    status = Some(if searching {
                        "no more results yet -- still searching".to_string()
                    } else {
                        "no more results".to_string()
                    });
                }
            }
            keyboard::Key::Char('N') => {
                if page > 0 {
                    page -= 1;
                    selected = 0;
                }
            }
            keyboard::Key::Char('c') => {
                if let Some(m) = matches.get(start + selected) {
                    status = Some(match copy_to_clipboard(&m.path.to_string_lossy()) {
                        Ok(()) => "copied path to clipboard".to_string(),
                        Err(e) => format!("couldn't copy: {e}"),
                    });
                }
            }
            keyboard::Key::Char('f') => {
                if let Some(m) = matches.get(start + selected) {
                    status = Some(match reveal_in_file_manager(&m.path) {
                        Ok(()) => "opened in file manager".to_string(),
                        Err(e) => format!("couldn't open: {e}"),
                    });
                }
            }
            keyboard::Key::Char('q') | keyboard::Key::Escape => break,
            _ => {}
        }
    }

    print!("\x1b[2J\x1b[H");
    io::stdout().flush().ok();
}

fn draw_file_page(
    matches: &[find::Match],
    start: usize,
    end: usize,
    selected: usize,
    page: usize,
    max_page: usize,
    searching: bool,
    status: Option<String>,
) {
    print!("\x1b[2J\x1b[H");

    if matches.is_empty() && searching {
        println!("{}", color::paint(color::DIM, "searching..."));
        println!();
    }

    for (i, m) in matches[start..end].iter().enumerate() {
        let date = m
            .modified
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| format_date(d.as_secs()))
            .unwrap_or_else(|| "-".to_string());

        if i == selected {
            let plain = format!("{:<40} {:>8}   {}", m.name, find::human_size(m.size), date);
            println!("{}", color::highlight_row(&plain));
        } else {
            let name = color::paint(color::CYAN, &format!("{:<40}", m.name));
            let size = color::paint(color::YELLOW, &format!("{:>8}", find::human_size(m.size)));
            let date_c = color::paint(color::GREEN, &date);
            println!("  {name} {size}   {date_c}");
        }
        println!();
    }

    let more = if page < max_page {
        "(more available, press n)"
    } else if searching {
        "(still searching...)"
    } else {
        ""
    };
    if !matches.is_empty() {
        println!("Showing {}-{} of {} {more}", start + 1, end, matches.len());
        println!();
    }
    println!("j/k move   f reveal in file manager   c copy path   n next batch   q back");
    if let Some(s) = status {
        println!("{s}");
    }
    io::stdout().flush().ok();
}

/// Numbered-menu fallback for platforms without raw terminal mode.
fn file_search_fallback(receiver: mpsc::Receiver<find::Match>) {
    let mut matches: Vec<find::Match> = receiver.into_iter().collect();
    matches.sort_by(|a, b| b.score.cmp(&a.score));

    if matches.is_empty() {
        println!("no matching files found");
        return;
    }

    let shown = matches.len().min(find::MAX_RESULTS);
    for (i, m) in matches[..shown].iter().enumerate() {
        let date = m
            .modified
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| format_date(d.as_secs()))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{}. {:<40} {:>8}   {}",
            i + 1,
            m.name,
            find::human_size(m.size),
            date
        );
    }
    if matches.len() > shown {
        println!("Showing first {shown} results...");
    }

    let selected = match prompt_selection(shown) {
        Some(i) => i,
        None => return,
    };

    println!("1. Copy path to clipboard");
    println!("2. Reveal in file manager");
    println!("3. Cancel");
    print!("Choose an option: ");
    io::stdout().flush().ok();

    let path = &matches[selected].path;
    match read_line().trim() {
        "1" => match copy_to_clipboard(&path.to_string_lossy()) {
            Ok(()) => println!("path copied to clipboard"),
            Err(e) => eprintln!("dig: couldn't copy to clipboard: {e}"),
        },
        "2" => {
            if let Err(e) = reveal_in_file_manager(path) {
                eprintln!("dig: couldn't open file manager: {e}");
            }
        }
        _ => {}
    }
}

/// Sets the system clipboard via OSC 52, a terminal escape sequence
/// most modern terminals support natively -- no external binary needed.
fn copy_to_clipboard(text: &str) -> io::Result<()> {
    let mut tty = std::fs::OpenOptions::new().write(true).open("/dev/tty")?;
    write!(tty, "\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))?;
    Ok(())
}

/// Minimal base64 encoder so OSC 52 clipboard copy needs zero crates.
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(CHARS[(b0 >> 2) as usize] as char);
        out.push(CHARS[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            CHARS[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            CHARS[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Reveals `path` using the configured file manager from `dig
/// -config` if set, falling back to the FileManager1 D-Bus service (or
/// plain xdg-open) if none is set or the configured one fails.
fn reveal_in_file_manager(path: &std::path::Path) -> io::Result<()> {
    let parent = path.parent().unwrap_or(path);

    if let Some(fm) = config::load().file_manager {
        if Command::new(&fm).arg(parent).status().map(|s| s.success()).unwrap_or(false) {
            return Ok(());
        }
    }

    let uri = format!("file://{}", path.display());
    let status = Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.FileManager1",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
            &format!("array:string:{uri}"),
            "string:",
        ])
        .status();

    if let Ok(s) = status {
        if s.success() {
            return Ok(());
        }
    }

    Command::new("xdg-open").arg(parent).status()?;
    Ok(())
}

// --- process mode ---

fn run_process_mode() {
    let processes = process::list_processes();
    if processes.is_empty() {
        println!("no processes found");
        return;
    }

    let groups = process::group_by_category(processes, 5);
    if groups.is_empty() {
        println!("no processes found");
        return;
    }

    match keyboard::RawMode::enable() {
        Ok(_raw) => interactive_process_ui(groups),
        Err(_) => process_fallback(groups),
    }
}

/// Refreshes the process list every couple of seconds in the
/// background, so the view stays live (a killed process disappears on
/// its own, a new one shows up) without the interactive loop blocking
/// on the ~200ms CPU-sample window each fetch takes.
fn spawn_process_refresher() -> mpsc::Receiver<Vec<(process::Category, Vec<process::ProcInfo>)>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(2));
        let groups = process::group_by_category(process::list_processes(), 5);
        if tx.send(groups).is_err() {
            break; // UI exited, no one's listening anymore
        }
    });
    rx
}

fn interactive_process_ui(mut groups: Vec<(process::Category, Vec<process::ProcInfo>)>) {
    let mut selected = 0usize;
    let mut status: Option<String> = None;
    let refresh_rx = spawn_process_refresher();

    let stdin = io::stdin();
    let mut lock = stdin.lock();

    loop {
        while let Ok(fresh) = refresh_rx.try_recv() {
            let selected_pid = flat_process_at(&groups, selected).map(|p| p.pid);
            groups = fresh;
            if let Some(pid) = selected_pid {
                if let Some(idx) = find_process_index(&groups, pid) {
                    selected = idx;
                }
            }
        }

        let total: usize = groups.iter().map(|(_, list)| list.len()).sum();
        if total == 0 {
            print!("\x1b[2J\x1b[H");
            println!("no processes left to show");
            io::stdout().flush().ok();
            return;
        }
        if selected >= total {
            selected = total - 1;
        }

        draw_process_page(&groups, selected, status.take());

        let key = match keyboard::read_key(&mut lock) {
            Ok(k) => k,
            Err(_) => break,
        };

        match key {
            keyboard::Key::Timeout => {}
            keyboard::Key::Char('j') | keyboard::Key::Down => {
                if selected + 1 < total {
                    selected += 1;
                }
            }
            keyboard::Key::Char('k') | keyboard::Key::Up => {
                if selected > 0 {
                    selected -= 1;
                }
            }
            keyboard::Key::Char('x') => {
                if let Some(p) = flat_process_at(&groups, selected) {
                    let pid = p.pid;
                    let msg = confirm_and_kill(&mut lock, p);
                    if msg.starts_with("sent terminate signal") {
                        remove_process(&mut groups, pid);
                    }
                    status = Some(msg);
                }
            }
            keyboard::Key::Char('q') | keyboard::Key::Escape => break,
            _ => {}
        }
    }

    print!("\x1b[2J\x1b[H");
    io::stdout().flush().ok();
}

fn remove_process(groups: &mut [(process::Category, Vec<process::ProcInfo>)], pid: u32) {
    for (_, list) in groups.iter_mut() {
        if let Some(idx) = list.iter().position(|p| p.pid == pid) {
            list.remove(idx);
            return;
        }
    }
}

fn find_process_index(groups: &[(process::Category, Vec<process::ProcInfo>)], pid: u32) -> Option<usize> {
    let mut idx = 0;
    for (_, list) in groups {
        for p in list {
            if p.pid == pid {
                return Some(idx);
            }
            idx += 1;
        }
    }
    None
}

fn flat_process_at(groups: &[(process::Category, Vec<process::ProcInfo>)], index: usize) -> Option<&process::ProcInfo> {
    let mut remaining = index;
    for (_, list) in groups {
        if remaining < list.len() {
            return Some(&list[remaining]);
        }
        remaining -= list.len();
    }
    None
}

fn draw_process_page(groups: &[(process::Category, Vec<process::ProcInfo>)], selected: usize, status: Option<String>) {
    print!("\x1b[2J\x1b[H");

    let mut idx = 0;
    for (cat, procs) in groups {
        if procs.is_empty() {
            continue;
        }
        println!("{}", color::paint(color::BLUE, &format!("-- {} --", cat.label())));
        println!();
        for p in procs {
            if idx == selected {
                let plain = format!(
                    "{:<15} {:>6} MB   {:>5.1}%   PID {}",
                    p.name, p.mem_mb, p.cpu_percent, p.pid
                );
                println!("{}", color::highlight_row(&plain));
            } else {
                let name = color::paint(color::CYAN, &format!("{:<15}", p.name));
                let mem = color::paint(color::YELLOW, &format!("{:>6} MB", p.mem_mb));
                let cpu = color::paint(color::GREEN, &format!("{:>5.1}%", p.cpu_percent));
                let pid = color::paint(color::DIM, &format!("PID {}", p.pid));
                println!("  {name} {mem}   {cpu}   {pid}");
            }
            println!();
            idx += 1;
        }
    }

    println!("j/k move   x kill process   q back   (list refreshes automatically)");
    if let Some(s) = status {
        println!("{s}");
    }
    io::stdout().flush().ok();
}

/// Runs the kill guardrail and y/n confirmation, reading from the same
/// locked stdin the main loop uses, and returns a status line for the
/// next redraw.
fn confirm_and_kill<R: io::Read>(reader: &mut R, p: &process::ProcInfo) -> String {
    let self_pid = std::process::id();
    let parent_pid = parent_pid();

    if process::is_protected(p, self_pid, parent_pid) {
        return format!(
            "refusing to kill {} (PID {}) -- essential to the system or this session",
            p.name, p.pid
        );
    }

    print!("\x1b[2J\x1b[HKill \"{}\" (PID {})? (y/n): ", p.name, p.pid);
    io::stdout().flush().ok();

    if !wait_for_yes_no(reader) {
        return "cancelled".to_string();
    }

    match process::kill(p, self_pid, parent_pid) {
        Ok(()) => format!("sent terminate signal to PID {}", p.pid),
        Err(e) => format!("dig: {e}"),
    }
}

/// A confirmation prompt needs to wait for a real answer, not give up
/// on the first idle-poll timeout -- a person takes longer than that
/// to read a prompt and press a key. Keeps reading through timeouts
/// until an actual y/n/escape, with a generous cutoff in case the
/// terminal disappears entirely.
fn wait_for_yes_no<R: io::Read>(reader: &mut R) -> bool {
    const MAX_WAIT_TICKS: usize = 3000; // roughly five minutes, at ~100ms per tick
    let mut ticks = 0;

    loop {
        match keyboard::read_key(reader) {
            Ok(keyboard::Key::Char('y')) | Ok(keyboard::Key::Char('Y')) => return true,
            Ok(keyboard::Key::Char('n')) | Ok(keyboard::Key::Char('N')) | Ok(keyboard::Key::Escape) => {
                return false
            }
            Ok(keyboard::Key::Timeout) => {
                ticks += 1;
                if ticks >= MAX_WAIT_TICKS {
                    return false;
                }
            }
            Err(_) => return false,
            _ => {}
        }
    }
}

/// Numbered-menu fallback for platforms without raw terminal mode.
fn process_fallback(groups: Vec<(process::Category, Vec<process::ProcInfo>)>) {
    let flat: Vec<&process::ProcInfo> = groups.iter().flat_map(|(_, list)| list.iter()).collect();

    let mut i = 1;
    for (cat, procs) in &groups {
        if procs.is_empty() {
            continue;
        }
        println!("-- {} --", cat.label());
        for p in procs {
            println!(
                "{}. {:<15} {:>6} MB   {:>5.1}%   PID {}",
                i, p.name, p.mem_mb, p.cpu_percent, p.pid
            );
            i += 1;
        }
    }

    let selected = match prompt_selection(flat.len()) {
        Some(i) => i,
        None => return,
    };
    let p = flat[selected];

    let self_pid = std::process::id();
    let parent_pid = parent_pid();
    if process::is_protected(p, self_pid, parent_pid) {
        println!(
            "refusing to kill {} (PID {}) -- it's essential to the system or this session",
            p.name, p.pid
        );
        return;
    }

    print!("Kill \"{}\" (PID {})? (y/n): ", p.name, p.pid);
    io::stdout().flush().ok();
    let confirm = read_line().trim().to_lowercase();
    if confirm != "y" && confirm != "yes" {
        println!("cancelled");
        return;
    }
    match process::kill(p, self_pid, parent_pid) {
        Ok(()) => println!("sent terminate signal to PID {}", p.pid),
        Err(e) => eprintln!("dig: {e}"),
    }
}

#[cfg(unix)]
fn parent_pid() -> u32 {
    unsafe { libc_getppid() }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "getppid"]
    fn libc_getppid() -> u32;
}

#[cfg(not(unix))]
fn parent_pid() -> u32 {
    0
}

// --- shared helpers ---

fn prompt_selection(count: usize) -> Option<usize> {
    loop {
        print!("Enter number to select, or q to quit: ");
        io::stdout().flush().ok();
        let input = read_line();
        if input.is_empty() {
            return None;
        }
        let trimmed = input.trim();

        if trimmed == "q" {
            return None;
        }

        match trimmed.parse::<usize>() {
            Ok(n) if n >= 1 && n <= count => return Some(n - 1),
            _ => {
                print!("invalid choice, try again: ");
                io::stdout().flush().ok();
            }
        }
    }
}

fn read_line() -> String {
    let mut input = String::new();
    match io::stdin().read_line(&mut input) {
        Ok(0) => String::new(),
        Ok(_) => input,
        Err(_) => String::new(),
    }
}

/// Formats a Unix timestamp as YYYY-MM-DD without pulling in a date/time
/// crate -- spelled out by hand (Howard Hinnant's civil_from_days).
fn format_date(unix_secs: u64) -> String {
    const DAYS_PER_400Y: i64 = 146097;
    let days_since_epoch = (unix_secs / 86400) as i64;
    let z = days_since_epoch + 719468;
    let era = if z >= 0 { z } else { z - DAYS_PER_400Y + 1 } / DAYS_PER_400Y;
    let doe = z - era * DAYS_PER_400Y;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y_raw = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y_raw + 1 } else { y_raw };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_match(path: &str, line: usize) -> content_search::Match {
        content_search::Match {
            path: PathBuf::from(path),
            line,
            text: "some line content".to_string(),
            spans: vec![],
        }
    }

    #[test]
    fn content_page_never_exceeds_available_rows() {
        // 5 different files, one match each: header(1) + match(1) = 2
        // rows per file, plus a blank separator between files after
        // the first. With 7 rows available, only the first 2 files
        // fully fit (2 + 3 = 5); a 3rd file would need 3 more (total
        // 8), which exceeds 7, so it must stop at 2.
        let matches: Vec<_> = (0..5).map(|i| stub_match(&format!("file{i}.rs"), 1)).collect();
        let end = content_page_end(&matches, 0, 7);
        assert_eq!(end, 2);
    }

    #[test]
    fn content_page_groups_same_file_cheaply() {
        // 5 matches, all in the SAME file: header(1) + 5 match rows = 6 rows total.
        // With only 4 available rows, only 3 matches should fit (header + 3 lines = 4).
        let matches: Vec<_> = (1..=5).map(|line| stub_match("one_file.rs", line)).collect();
        let end = content_page_end(&matches, 0, 4);
        assert_eq!(end, 3);
    }

    #[test]
    fn content_page_always_shows_at_least_one_match() {
        // available_rows smaller than even one match needs -- must
        // still advance by 1, never return an empty/stuck page.
        let matches = vec![stub_match("file.rs", 1)];
        let end = content_page_end(&matches, 0, 0);
        assert_eq!(end, 1);
    }

    #[test]
    fn content_page_resumes_correctly_from_a_nonzero_start() {
        let matches: Vec<_> = (0..10).map(|i| stub_match(&format!("file{i}.rs"), 1)).collect();
        let first_end = content_page_end(&matches, 0, 7);
        let second_end = content_page_end(&matches, first_end, 7);
        assert!(second_end > first_end);
        assert!(second_end <= matches.len());
    }

    #[test]
    fn content_page_end_never_exceeds_total_matches() {
        let matches: Vec<_> = (0..3).map(|i| stub_match(&format!("file{i}.rs"), 1)).collect();
        let end = content_page_end(&matches, 0, 1000);
        assert_eq!(end, 3);
    }

    #[test]
    fn content_page_end_with_zero_matches_returns_zero() {
        let matches: Vec<content_search::Match> = vec![];
        assert_eq!(content_page_end(&matches, 0, 20), 0);
    }

    #[test]
    fn highlight_wraps_the_matched_region() {
        let out = highlight("hello TODO world", &[(6, 4)], true);
        assert_eq!(out, "hello \x1b[1;31mTODO\x1b[0m world");
    }

    #[test]
    fn highlight_returns_plain_text_when_colorize_is_false() {
        let out = highlight("hello TODO world", &[(6, 4)], false);
        assert_eq!(out, "hello TODO world");
    }

    #[test]
    fn highlight_falls_back_safely_on_out_of_range_offsets() {
        let out = highlight("short", &[(100, 4)], true);
        assert_eq!(out, "short");
    }

    #[test]
    fn highlight_falls_back_safely_on_bad_char_boundary() {
        let text = "café bar";
        let out = highlight(text, &[(4, 1)], true);
        assert_eq!(out, text);
    }

    #[test]
    fn highlight_wraps_multiple_spans_on_one_line() {
        let out = highlight("TODO fix TODO later", &[(0, 4), (9, 4)], true);
        assert_eq!(out, "\x1b[1;31mTODO\x1b[0m fix \x1b[1;31mTODO\x1b[0m later");
    }

    struct SlowKeyboard {
        timeouts_remaining: usize,
        final_byte: u8,
    }

    impl io::Read for SlowKeyboard {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.timeouts_remaining > 0 {
                self.timeouts_remaining -= 1;
                Ok(0)
            } else {
                buf[0] = self.final_byte;
                Ok(1)
            }
        }
    }

    #[test]
    fn wait_for_yes_no_survives_a_slow_human_pressing_y() {
        let mut kb = SlowKeyboard { timeouts_remaining: 20, final_byte: b'y' };
        assert!(wait_for_yes_no(&mut kb));
    }

    #[test]
    fn wait_for_yes_no_survives_a_slow_human_pressing_n() {
        let mut kb = SlowKeyboard { timeouts_remaining: 20, final_byte: b'n' };
        assert!(!wait_for_yes_no(&mut kb));
    }

    #[test]
    fn wait_for_yes_no_does_not_cancel_on_the_first_timeout() {
        let mut kb = SlowKeyboard { timeouts_remaining: 1, final_byte: b'y' };
        assert!(wait_for_yes_no(&mut kb));
    }

    #[test]
    fn wait_for_yes_no_ignores_irrelevant_keys_while_waiting() {
        struct Junk {
            sent_junk: bool,
        }
        impl io::Read for Junk {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if !self.sent_junk {
                    self.sent_junk = true;
                    buf[0] = b'z';
                } else {
                    buf[0] = b'y';
                }
                Ok(1)
            }
        }
        let mut junk = Junk { sent_junk: false };
        assert!(wait_for_yes_no(&mut junk));
    }

    #[test]
    fn find_process_index_locates_pid_across_categories() {
        let groups = vec![
            (
                process::Category::System,
                vec![process::ProcInfo { pid: 1, name: "init".into(), mem_mb: 1, cpu_percent: 0.0 }],
            ),
            (
                process::Category::Other,
                vec![
                    process::ProcInfo { pid: 42, name: "app".into(), mem_mb: 5, cpu_percent: 0.0 },
                    process::ProcInfo { pid: 99, name: "app2".into(), mem_mb: 5, cpu_percent: 0.0 },
                ],
            ),
        ];
        assert_eq!(find_process_index(&groups, 99), Some(2));
        assert_eq!(find_process_index(&groups, 12345), None);
    }

    #[test]
    fn flat_process_at_returns_none_instead_of_panicking_out_of_range() {
        let groups: Vec<(process::Category, Vec<process::ProcInfo>)> = vec![];
        assert!(flat_process_at(&groups, 0).is_none());
    }
}
