//! Where Sonda keeps things: its index and models in ~/.local/share/sonda,
//! what it can make again (thumbnails, rendered pages, sound cut up) in
//! ~/.cache/sonda, its settings in ~/.config/sonda, and its socket in the
//! session's runtime folder.

use std::path::PathBuf;

fn env_dir(var: &str, fallback: &str) -> PathBuf {
    match std::env::var(var) {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

pub fn data() -> PathBuf {
    let d = env_dir("XDG_DATA_HOME", ".local/share").join("sonda");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn cache() -> PathBuf {
    let d = env_dir("XDG_CACHE_HOME", ".cache").join("sonda");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn cache_sub(name: &str) -> PathBuf {
    let d = cache().join(name);
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn config() -> PathBuf {
    let d = env_dir("XDG_CONFIG_HOME", ".config").join("sonda");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn index_db() -> PathBuf {
    data().join("index.db")
}

pub fn socket() -> PathBuf {
    if let Some(s) = std::env::var_os("SONDA_SOCKET").filter(|s| !s.is_empty()) {
        return PathBuf::from(s);
    }
    let dir = std::env::var("XDG_RUNTIME_DIR").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/tmp".into());
    PathBuf::from(dir).join("sonda.sock")
}

pub fn log() -> PathBuf {
    data().join("sonda.log")
}

/// The model files: beside the program's data, or where `SONDA_MODELS` says.
pub fn models() -> PathBuf {
    std::env::var("SONDA_MODELS").ok().filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| data().join("models"))
}

pub const TEXT_MODEL: &str = "embeddinggemma-2-Q8_0.gguf";
pub const MEDIA_MODEL: &str = "mmproj-embeddinggemma-2-Q8_0.gguf";

/// The user's own folders, as `xdg-user-dirs` names them (in their language).
pub fn user_dir(key: &str) -> Option<PathBuf> {
    let text = std::fs::read_to_string(env_dir("XDG_CONFIG_HOME", ".config").join("user-dirs.dirs")).ok()?;
    for line in text.lines() {
        let Some(rest) = line.strip_prefix(&format!("XDG_{key}_DIR=")) else { continue };
        let value = rest.trim().trim_matches('"').replace("$HOME", &home().to_string_lossy());
        let p = PathBuf::from(value);
        if p != home() && p.is_dir() {
            return Some(p);
        }
    }
    None
}
