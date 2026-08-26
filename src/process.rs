use std::collections::HashSet;
use std::thread;
use std::time::Duration;
use sysinfo::{Pid, PidExt, ProcessExt, System, SystemExt};


/// sysinfo needs two samples apart in time to compute per-process CPU
/// usage; this is that gap.
const CPU_SAMPLE_WINDOW: Duration = Duration::from_millis(200);

pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub mem_mb: u64,
    pub cpu_percent: f32,
}

#[derive(PartialEq, Eq, Debug, Clone, Copy, PartialOrd, Ord)]
pub enum Category {
    System,
    Development,
    Browser,
    Media,
    Other,
}

impl Category {
    pub fn label(&self) -> &'static str {
        match self {
            Category::System => "System",
            Category::Development => "Development",
            Category::Browser => "Browser",
            Category::Media => "Media",
            Category::Other => "Other",
        }
    }
}

/// Buckets a process by name using simple keyword matching. This is a
/// heuristic, not an authoritative classification -- an app with an
/// unusual process name will fall into "Other", which is the safe
/// default rather than a wrong guess.
pub fn categorize(name: &str) -> Category {
    let n = name.to_lowercase();

    const SYSTEM: &[&str] = &[
        "systemd",
        "kthreadd",
        "kworker",
        "init",
        "sshd",
        "dbus-daemon",
        "networkmanager",
        "udevd",
        "cron",
        "polkitd",
        "upowerd",
        "cosmic-comp",
        "xorg",
        "xwayland",
        "wayland",
        "gnome-shell",
        "plasmashell",
        "irq/",
        "cpuhp",
        "migration",
        "ksoftirqd",
        "rcu_",
        "kauditd",
        "khungtaskd",
    ];
    const DEVELOPMENT: &[&str] = &[
        "code",
        "rustc",
        "cargo",
        "node",
        "python",
        "java",
        "docker",
        "containerd",
        " go ",
        "gcc",
        "clang",
        "cc1",
        "rust-analyzer",
        "gopls",
        "tsserver",
        "jetbrains",
        "idea",
        "clion",
        "pycharm",
        "git",
        "vim",
        "nvim",
        "emacs",
    ];
    const BROWSER: &[&str] = &[
        "firefox",
        "chrome",
        "chromium",
        "brave",
        "opera",
        "msedge",
        "edge",
        "vivaldi",
        "epiphany",
        "safari",
    ];
    const MEDIA: &[&str] = &[
        "spotify",
        "vlc",
        "mpv",
        "obs",
        "ffmpeg",
        "pulseaudio",
        "pipewire",
        "rhythmbox",
        "audacious",
    ];

    if SYSTEM.iter().any(|s| n.contains(s)) {
        Category::System
    } else if DEVELOPMENT.iter().any(|s| n.contains(s)) {
        Category::Development
    } else if BROWSER.iter().any(|s| n.contains(s)) {
        Category::Browser
    } else if MEDIA.iter().any(|s| n.contains(s)) {
        Category::Media
    } else {
        Category::Other
    }
}

/// Groups processes by category, sorts each category by memory
/// (highest first), and caps each category at `per_category` entries
/// so one noisy category (usually System) doesn't crowd out the rest.
pub fn group_by_category(mut procs: Vec<ProcInfo>, per_category: usize) -> Vec<(Category, Vec<ProcInfo>)> {
    procs.sort_by(|a, b| b.mem_mb.cmp(&a.mem_mb));

    let order = [
        Category::System,
        Category::Development,
        Category::Browser,
        Category::Media,
        Category::Other,
    ];

    let mut groups: Vec<(Category, Vec<ProcInfo>)> = order.iter().map(|c| (*c, Vec::new())).collect();
    for p in procs {
        let cat = categorize(&p.name);
        if let Some(bucket) = groups.iter_mut().find(|(c, _)| *c == cat) {
            if bucket.1.len() < per_category {
                bucket.1.push(p);
            }
        }
    }

    groups.retain(|(_, list)| !list.is_empty());
    groups
}

/// Names that will take the session or the whole OS down with them.
/// Never allowed to be killed from here.
fn essential_process_names() -> HashSet<&'static str> {
    [
        "systemd",
        "init",
        "kernel",
        "kthreadd",
        "Xorg",
        "Xwayland",
        "wayland",
        "gnome-shell",
        "plasmashell",
        "cosmic-comp",
        "sshd",
        "NetworkManager",
        "dbus-daemon",
        "systemd-journald",
        "systemd-logind",
        "udevd",
        "pipewire",
        "pulseaudio",
        "explorer.exe",
        "winlogon.exe",
        "csrss.exe",
        "services.exe",
        "smss.exe",
        "WindowServer",
        "loginwindow",
        "launchd",
    ]
    .into_iter()
    .collect()
}

pub fn list_processes() -> Vec<ProcInfo> {
    let mut sys = System::new_all();
    sys.refresh_processes();
    thread::sleep(CPU_SAMPLE_WINDOW);
    sys.refresh_processes();

    sys.processes()
        .values()
        .map(|p| ProcInfo {
            pid: p.pid().as_u32(),
            name: p.name().to_string(),
            mem_mb: p.memory() / (1024 * 1024),
            cpu_percent: p.cpu_usage(),
        })
        .collect()
}

pub fn is_protected(p: &ProcInfo, self_pid: u32, parent_pid: u32) -> bool {
    if p.pid == 1 {
        return true;
    }
    if p.pid == self_pid || p.pid == parent_pid {
        return true;
    }
    essential_process_names().contains(p.name.as_str())
}

/// Sends a termination signal, giving the target process a chance to
/// shut down cleanly rather than being forcibly killed. Refuses
/// outright on protected processes.
pub fn kill(p: &ProcInfo, self_pid: u32, parent_pid: u32) -> Result<(), String> {
    if is_protected(p, self_pid, parent_pid) {
        return Err(format!(
            "refusing to kill {} (PID {}) — essential to the system or this session",
            p.name, p.pid
        ));
    }

    let mut sys = System::new_all();
    sys.refresh_processes();
    match sys.process(Pid::from_u32(p.pid)) {
        Some(proc) => {
            if proc.kill_with(sysinfo::Signal::Term).unwrap_or(false) {
                Ok(())
            } else {
                Err("failed to send terminate signal".to_string())
            }
        }
        None => Err("process no longer exists".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub(pid: u32, name: &str) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.to_string(),
            mem_mb: 1,
            cpu_percent: 0.0,
        }
    }

    #[test]
    fn refuses_pid_1() {
        let p = stub(1, "systemd");
        assert!(kill(&p, 9999, 1).is_err());
    }

    #[test]
    fn refuses_essential_name_even_with_unusual_pid() {
        let p = stub(55555, "sshd");
        assert!(kill(&p, 9999, 1).is_err());
    }

    #[test]
    fn refuses_self() {
        let p = stub(9999, "dig");
        assert!(kill(&p, 9999, 1).is_err());
    }

    #[test]
    fn refuses_parent() {
        let p = stub(1, "bash"); // pid 1 always protected regardless of name
        assert!(kill(&p, 9999, 1).is_err());
        let p2 = stub(500, "bash");
        assert!(kill(&p2, 9999, 500).is_err()); // 500 is the "parent" here
    }

    #[test]
    fn allows_ordinary_process_past_the_guardrail() {
        // We can't actually kill something real in a test, but we can
        // confirm is_protected() doesn't block a harmless-looking process
        // before the kill() call even reaches sysinfo.
        let p = stub(999999, "some-random-app");
        assert!(!is_protected(&p, 9999, 1));
    }

    #[test]
    fn categorizes_known_names_correctly() {
        assert_eq!(categorize("firefox"), Category::Browser);
        assert_eq!(categorize("chromium-browser"), Category::Browser);
        assert_eq!(categorize("kworker/R-abc"), Category::System);
        assert_eq!(categorize("systemd-journald"), Category::System);
        assert_eq!(categorize("rustc"), Category::Development);
        assert_eq!(categorize("node"), Category::Development);
        assert_eq!(categorize("spotify"), Category::Media);
        assert_eq!(categorize("some-random-app"), Category::Other);
    }

    #[test]
    fn categorize_is_case_insensitive() {
        assert_eq!(categorize("Firefox"), Category::Browser);
        assert_eq!(categorize("SYSTEMD"), Category::System);
    }

    #[test]
    fn grouping_sorts_within_category_by_memory() {
        let procs = vec![
            stub_mem(1, "firefox", 100),
            stub_mem(2, "chrome", 500),
            stub_mem(3, "brave", 200),
        ];
        let groups = group_by_category(procs, 10);
        let browser_group = groups.iter().find(|(c, _)| *c == Category::Browser).unwrap();
        let mems: Vec<u64> = browser_group.1.iter().map(|p| p.mem_mb).collect();
        assert_eq!(mems, vec![500, 200, 100]);
    }

    #[test]
    fn grouping_caps_each_category() {
        let procs: Vec<ProcInfo> = (0..10).map(|i| stub_mem(i, "firefox", i as u64)).collect();
        let groups = group_by_category(procs, 3);
        let browser_group = groups.iter().find(|(c, _)| *c == Category::Browser).unwrap();
        assert_eq!(browser_group.1.len(), 3);
    }

    #[test]
    fn grouping_omits_empty_categories() {
        let procs = vec![stub_mem(1, "firefox", 100)];
        let groups = group_by_category(procs, 10);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, Category::Browser);
    }

    fn stub_mem(pid: u32, name: &str, mem_mb: u64) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.to_string(),
            mem_mb,
            cpu_percent: 0.0,
        }
    }
}
