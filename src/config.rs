use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;

#[derive(Default, Clone)]
pub struct Config {
    pub code_editor: Option<String>,
    pub file_manager: Option<String>,
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"));
    base.join("dig").join("config")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// Reads the config file if it exists; a missing or unreadable file
/// just means no overrides are set, not an error.
pub fn load() -> Config {
    let path = config_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Config::default();
    };

    let mut values: HashMap<String, String> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim().to_string(), value.trim().to_string());
        }
    }

    Config {
        code_editor: values.get("code_editor").filter(|v| !v.is_empty()).cloned(),
        file_manager: values.get("file_manager").filter(|v| !v.is_empty()).cloned(),
    }
}

pub fn save(cfg: &Config) -> io::Result<()> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut out = String::new();
    out.push_str(&format!("code_editor={}\n", cfg.code_editor.as_deref().unwrap_or("")));
    out.push_str(&format!("file_manager={}\n", cfg.file_manager.as_deref().unwrap_or("")));
    std::fs::write(path, out)
}

/// Interactive `dig -config` wizard: shows the current value for each
/// setting and lets the user type a new one, leave it blank to keep
/// it, or type "clear" to unset it.
pub fn run_wizard() {
    let mut cfg = load();

    println!("dig config -- press enter to keep the current value, type 'clear' to unset it");
    println!();

    cfg.code_editor = prompt_setting("code editor (e.g. code, nvim, subl)", cfg.code_editor);
    cfg.file_manager = prompt_setting("file manager (e.g. nautilus, dolphin, nemo)", cfg.file_manager);

    match save(&cfg) {
        Ok(()) => println!("\nSaved to {}", config_path().display()),
        Err(e) => eprintln!("\ndig: couldn't save config: {e}"),
    }
}

fn prompt_setting(label: &str, current: Option<String>) -> Option<String> {
    let shown = current.as_deref().unwrap_or("not set");
    print!("{label} [{shown}]: ");
    io::stdout().flush().ok();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).unwrap_or(0) == 0 {
        return current; // EOF: keep whatever was already set
    }
    let input = input.trim();

    if input.is_empty() {
        current
    } else if input.eq_ignore_ascii_case("clear") {
        None
    } else {
        Some(input.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_key_value_lines() {
        let text = "code_editor=nvim\nfile_manager=dolphin\n";
        let mut values = HashMap::new();
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                values.insert(k.to_string(), v.to_string());
            }
        }
        assert_eq!(values.get("code_editor"), Some(&"nvim".to_string()));
        assert_eq!(values.get("file_manager"), Some(&"dolphin".to_string()));
    }

    #[test]
    fn empty_value_is_treated_as_unset() {
        // save() writes "code_editor=" when unset; load() must treat
        // that the same as the key being absent entirely
        let text = "code_editor=\nfile_manager=dolphin\n";
        let mut values = HashMap::new();
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                values.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        let code_editor = values.get("code_editor").filter(|v| !v.is_empty()).cloned();
        assert_eq!(code_editor, None);
    }
}
