use crate::walker::build_walker;
use memchr::memmem;
use memmap2::Mmap;
use std::borrow::Cow;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

/// Long generated/minified lines (a single-line SVG path, for example)
/// would otherwise flood the terminal with one giant unreadable row.
const MAX_LINE_LEN: usize = 240;
const CONTEXT_BEFORE: usize = 40;

/// Caps total matches across the whole search so one pathological file
/// (a huge minified bundle matching a common character, say) can't
/// stall the walk or flood memory. Real searches never get close to
/// this -- pagination would need dozens of screens either way.
const MAX_TOTAL_MATCHES: usize = 2000;

pub struct Match {
    pub path: PathBuf,
    pub line: usize,
    /// The line, possibly truncated around the match if it was
    /// unreasonably long. Spans are adjusted to still point correctly
    /// into whatever text ends up here.
    pub text: String,
    pub spans: Vec<(usize, usize)>,
}

/// Either a fast literal substring search (the common case) or a real
/// regex, selected by the caller.
#[derive(Clone)]
enum Matcher {
    Literal { needle: Vec<u8>, case_sensitive: bool },
    Regex(regex::bytes::Regex),
}

impl Matcher {
    fn new(pattern: &str, use_regex: bool, case_sensitive: bool) -> Result<Matcher, String> {
        if use_regex {
            let compiled = if case_sensitive {
                regex::bytes::Regex::new(pattern)
            } else {
                regex::bytes::Regex::new(&format!("(?i){pattern}"))
            };
            compiled.map(Matcher::Regex).map_err(|e| e.to_string())
        } else {
            let needle = if case_sensitive {
                pattern.as_bytes().to_vec()
            } else {
                pattern.to_ascii_lowercase().into_bytes()
            };
            Ok(Matcher::Literal { needle, case_sensitive })
        }
    }

    /// Every (start, end) byte span the pattern matches in `haystack`,
    /// in ascending order.
    fn find_all(&self, haystack: &[u8]) -> Vec<(usize, usize)> {
        match self {
            Matcher::Regex(re) => re.find_iter(haystack).map(|m| (m.start(), m.end())).collect(),
            Matcher::Literal { needle, case_sensitive } => {
                let folded: Cow<[u8]> = if *case_sensitive {
                    Cow::Borrowed(haystack)
                } else {
                    Cow::Owned(haystack.iter().map(|b| b.to_ascii_lowercase()).collect())
                };
                let finder = memmem::Finder::new(needle);
                finder
                    .find_iter(&folded)
                    .map(|start| (start, start + needle.len()))
                    .collect()
            }
        }
    }
}

/// Searches file contents under `root` for `pattern`, respecting
/// .gitignore along the way. `case_sensitive` controls matching case
/// directly -- no auto-detection. `use_regex` treats the pattern as a
/// regular expression instead of a literal string. Matches stream out
/// on the returned channel as they're found.
///
/// A thread walks the tree feeding paths into a queue; a pool of
/// worker threads (one per core) pulls from it and searches files in
/// parallel. Each file is memory-mapped rather than read into a
/// buffer, since the OS can hand back the bytes directly.
pub fn search(
    root: &Path,
    pattern: &str,
    use_regex: bool,
    case_sensitive: bool,
) -> Result<mpsc::Receiver<Match>, String> {
    let matcher = Matcher::new(pattern, use_regex, case_sensitive)?;

    let (result_tx, result_rx) = mpsc::channel::<Match>();
    let (path_tx, path_rx) = mpsc::channel::<PathBuf>();
    let path_rx = Arc::new(Mutex::new(path_rx));
    let stop = Arc::new(AtomicBool::new(false));
    let found = Arc::new(AtomicUsize::new(0));

    let root_owned = root.to_path_buf();
    let walk_stop = Arc::clone(&stop);
    thread::spawn(move || {
        for result in build_walker(&root_owned) {
            if walk_stop.load(Ordering::Relaxed) {
                break;
            }
            let entry = match result {
                Ok(e) => e,
                Err(_) => continue,
            };
            let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
            if is_file {
                if path_tx.send(entry.path().to_path_buf()).is_err() {
                    break;
                }
            }
        }
    });

    let workers = thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let path_rx = Arc::clone(&path_rx);
        let result_tx = result_tx.clone();
        let matcher = matcher.clone();
        let stop = Arc::clone(&stop);
        let found = Arc::clone(&found);
        handles.push(thread::spawn(move || loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let path = {
                let rx = path_rx.lock().unwrap();
                rx.recv()
            };
            match path {
                Ok(p) => search_file(&p, &matcher, &result_tx, &stop, &found),
                Err(_) => break,
            }
        }));
    }
    drop(result_tx);

    thread::spawn(move || {
        for h in handles {
            let _ = h.join();
        }
    });

    Ok(result_rx)
}

fn search_file(
    path: &Path,
    matcher: &Matcher,
    tx: &mpsc::Sender<Match>,
    stop: &AtomicBool,
    found: &AtomicUsize,
) {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let mmap = match unsafe { Mmap::map(&file) } {
        Ok(m) => m,
        Err(_) => return,
    };
    if mmap.is_empty() {
        return;
    }

    // Skip likely-binary files: a null byte in the first chunk is a
    // reliable enough signal without a full content-type check.
    let head_len = mmap.len().min(512);
    if mmap[..head_len].contains(&0) {
        return;
    }

    let positions = matcher.find_all(&mmap);
    if positions.is_empty() {
        return;
    }

    // Group matches that land on the same line into one Match with
    // multiple spans, so a line with several hits is printed once with
    // all of them highlighted -- not printed once per hit.
    let mut i = 0;
    while i < positions.len() {
        if stop.load(Ordering::Relaxed) {
            return;
        }

        let (start, end) = positions[i];
        let line = mmap[..start].iter().filter(|&&b| b == b'\n').count() + 1;
        let line_begin = mmap[..start]
            .iter()
            .rposition(|&b| b == b'\n')
            .map(|p| p + 1)
            .unwrap_or(0);
        let line_end = mmap[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| start + p)
            .unwrap_or(mmap.len());

        let mut spans = vec![(start - line_begin, end - start)];
        let mut j = i + 1;
        while j < positions.len() && positions[j].0 < line_end {
            let (s, e) = positions[j];
            spans.push((s - line_begin, e - s));
            j += 1;
        }

        let full_line = String::from_utf8_lossy(&mmap[line_begin..line_end]).to_string();
        let (text, spans) = truncate_line(&full_line, &spans);

        if tx
            .send(Match {
                path: path.to_path_buf(),
                line,
                text,
                spans,
            })
            .is_err()
        {
            return;
        }

        if found.fetch_add(1, Ordering::Relaxed) + 1 >= MAX_TOTAL_MATCHES {
            stop.store(true, Ordering::Relaxed);
            return;
        }

        i = j;
    }
}

/// Centers a display window around the first match and drops it in
/// with "..." markers, so a 4000-character minified line doesn't flood
/// the screen. Spans are re-mapped into the truncated text; any span
/// that falls outside the window is dropped rather than shown wrong.
fn truncate_line(line: &str, spans: &[(usize, usize)]) -> (String, Vec<(usize, usize)>) {
    if line.len() <= MAX_LINE_LEN {
        return (line.to_string(), spans.to_vec());
    }

    let first_start = spans.first().map(|s| s.0).unwrap_or(0);
    let raw_start = first_start.saturating_sub(CONTEXT_BEFORE);
    let raw_end = (raw_start + MAX_LINE_LEN).min(line.len());

    let window_start = floor_char_boundary(line, raw_start);
    let window_end = ceil_char_boundary(line, raw_end);

    let prefix = if window_start > 0 { "..." } else { "" };
    let suffix = if window_end < line.len() { "..." } else { "" };

    let mut snippet = String::with_capacity(prefix.len() + (window_end - window_start) + suffix.len());
    snippet.push_str(prefix);
    snippet.push_str(&line[window_start..window_end]);
    snippet.push_str(suffix);

    let shift = window_start as isize - prefix.len() as isize;
    let new_spans = spans
        .iter()
        .filter_map(|&(s, l)| {
            if s >= window_start && s + l <= window_end {
                Some(((s as isize - shift) as usize, l))
            } else {
                None
            }
        })
        .collect();

    (snippet, new_spans)
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn ceil_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_line_is_untouched() {
        let (text, spans) = truncate_line("short TODO line", &[(6, 4)]);
        assert_eq!(text, "short TODO line");
        assert_eq!(spans, vec![(6, 4)]);
    }

    #[test]
    fn long_line_gets_truncated_around_the_match() {
        let padding = "x".repeat(500);
        let line = format!("{padding}TODO{padding}");
        let match_start = padding.len();
        let (text, spans) = truncate_line(&line, &[(match_start, 4)]);

        assert!(text.len() < line.len());
        assert!(text.starts_with("..."));
        assert!(text.ends_with("..."));
        // the match must still be recoverable at its new position
        let (s, l) = spans[0];
        assert_eq!(&text[s..s + l], "TODO");
    }

    #[test]
    fn span_outside_the_window_is_dropped_not_corrupted() {
        // two matches very far apart in a huge line -- only the one
        // near the window should survive
        let line = format!("TODO{}TODO", "x".repeat(1000));
        let (_, spans) = truncate_line(&line, &[(0, 4), (1004, 4)]);
        assert_eq!(spans.len(), 1);
    }
}
