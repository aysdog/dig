use crate::matcher::{exact_match_all, fuzzy_match_all};
use crate::walker::build_walker;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::SystemTime;

pub const MAX_RESULTS: usize = 25;
// Once this many candidates are found, stop walking early rather than
// scanning the rest of a huge tree just to throw most results away.
const CANDIDATE_POOL: usize = 500;

pub struct Match {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub score: i32,
}

/// Walks `root` in parallel (one worker thread per CPU core, same
/// architecture as the content search) looking for files matching
/// every word in `words`.
///
/// `case_sensitive` controls both matching mode and case at once:
/// false (the default) means fuzzy, typo-tolerant, case-insensitive
/// matching; true means exact literal, case-sensitive matching
/// (`dig -f -c`). One flag, one decision, instead of two separate axes
/// to reason about.
///
/// Returns immediately with a receiver rather than blocking until the
/// whole tree is scanned, so results can show up as they arrive.
pub fn search_stream(root: &Path, words: &[String], case_sensitive: bool) -> mpsc::Receiver<Match> {
    let match_words: Vec<String> = if case_sensitive {
        words.to_vec()
    } else {
        words.iter().map(|w| w.to_lowercase()).collect()
    };

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

    let (result_tx, result_rx) = mpsc::channel::<Match>();
    let workers = thread::available_parallelism().map(|n| n.get()).unwrap_or(4);

    for _ in 0..workers {
        let path_rx = Arc::clone(&path_rx);
        let result_tx = result_tx.clone();
        let match_words = match_words.clone();
        let stop = Arc::clone(&stop);
        let found = Arc::clone(&found);

        thread::spawn(move || loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let path = {
                let rx = path_rx.lock().unwrap();
                rx.recv()
            };
            let path = match path {
                Ok(p) => p,
                Err(_) => break,
            };

            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            let compare_name = if case_sensitive { name.clone() } else { name.to_lowercase() };

            let score = if case_sensitive {
                if exact_match_all(&compare_name, &match_words) {
                    Some(0)
                } else {
                    None
                }
            } else {
                fuzzy_match_all(&compare_name, &match_words)
            };

            let Some(score) = score else { continue };

            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let m = Match {
                path,
                name,
                size: meta.len(),
                modified: meta.modified().ok(),
                score,
            };
            if result_tx.send(m).is_err() {
                break;
            }

            if found.fetch_add(1, Ordering::Relaxed) + 1 >= CANDIDATE_POOL {
                stop.store(true, Ordering::Relaxed);
                break;
            }
        });
    }
    drop(result_tx);

    result_rx
}

/// Renders bytes as a short, human-readable size.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64 / 1024.0;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    format!("{:.1} {}", size, UNITS[unit_idx])
}
