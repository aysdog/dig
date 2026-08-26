use ignore::{DirEntry, WalkBuilder};
use std::collections::HashSet;
use std::path::Path;

/// Junk directories skipped wherever they appear, by name alone.
pub fn skip_dir_names() -> HashSet<&'static str> {
    [
        ".cache",
        ".thumbnails",
        "node_modules",
        ".git",
        "target",
        "dist",
        "build",
        "__pycache__",
        ".venv",
        ".idea",
        ".vscode",
    ]
    .into_iter()
    .collect()
}

/// Junk directories identified by their path relative to the search
/// root, since the name alone isn't specific enough (e.g. `.local`
/// also holds things worth searching).
pub fn skip_rel_paths() -> HashSet<&'static str> {
    [".local/share/Trash"].into_iter().collect()
}

pub fn should_skip_dir(root: &Path, path: &Path, name: &str) -> bool {
    if skip_dir_names().contains(name) {
        return true;
    }
    if let Ok(rel) = path.strip_prefix(root) {
        if let Some(rel_str) = rel.to_str() {
            if skip_rel_paths().contains(rel_str) {
                return true;
            }
        }
    }
    // Only matters for a whole-computer scan (root == "/"): pseudo
    // filesystems like /proc and /sys aren't real files, and this only
    // fires for direct children of "/", so ~/dev is never touched.
    if root == Path::new("/") && path.parent() == Some(root) {
        const SYSTEM_ROOTS: &[&str] = &["proc", "sys", "dev", "run", "lost+found"];
        if SYSTEM_ROOTS.contains(&name) {
            return true;
        }
    }
    false
}

/// Directory walker that respects .gitignore, same as ripgrep, plus
/// our own fixed junk-dir list as a fallback for trees with no
/// .gitignore at all.
pub fn build_walker(root: &Path) -> ignore::Walk {
    let root_owned = root.to_path_buf();
    let mut builder = WalkBuilder::new(root);
    builder.filter_entry(move |entry: &DirEntry| {
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if !is_dir {
            return true;
        }
        if entry.path() == root_owned {
            return true;
        }
        let name = entry.file_name().to_string_lossy();
        !should_skip_dir(&root_owned, entry.path(), &name)
    });
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn skips_proc_only_directly_under_root() {
        let root = PathBuf::from("/");
        assert!(should_skip_dir(&root, &PathBuf::from("/proc"), "proc"));
        assert!(should_skip_dir(&root, &PathBuf::from("/sys"), "sys"));
        assert!(should_skip_dir(&root, &PathBuf::from("/dev"), "dev"));
    }

    #[test]
    fn does_not_skip_a_dev_folder_inside_home() {
        // The whole point of the parent-check: someone's ~/dev project
        // folder must never be mistaken for the real /dev.
        let root = PathBuf::from("/home/anirban");
        assert!(!should_skip_dir(&root, &PathBuf::from("/home/anirban/dev"), "dev"));
    }

    #[test]
    fn does_not_skip_nested_proc_looking_folder() {
        let root = PathBuf::from("/");
        // /home/user/proc is not a direct child of "/", so it's untouched
        assert!(!should_skip_dir(&root, &PathBuf::from("/home/user/proc"), "proc"));
    }

    #[test]
    fn still_skips_ordinary_junk_dirs() {
        let root = PathBuf::from("/home/anirban/project");
        assert!(should_skip_dir(
            &root,
            &PathBuf::from("/home/anirban/project/node_modules"),
            "node_modules"
        ));
    }
}