//! Sonda's settings (~/.config/sonda/config.json): which folders it reads,
//! which ones it reads as code, what it leaves out, and whether it is paused.

use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Read by what is in them: pictures, PDFs, documents, sound, video.
    pub folders: Vec<String>,
    /// Read as code (only searched with `code:`).
    pub code_folders: Vec<String>,
    /// Never read.
    pub excluded: Vec<String>,
    /// The model on the graphics card (Vulkan), or on the processor.
    pub gpu: bool,
    /// Reading stopped until it is resumed.
    pub paused: bool,
    /// PDFs: pages past this are left out.
    pub max_pages: usize,
    /// Sound and video: minutes past this are left out.
    pub max_minutes: f64,
}

impl Default for Config {
    fn default() -> Self {
        let mut folders = Vec::new();
        for key in ["DESKTOP", "DOCUMENTS", "DOWNLOAD", "PICTURES", "VIDEOS", "MUSIC"] {
            if let Some(d) = paths::user_dir(key) {
                folders.push(d.to_string_lossy().into_owned());
            }
        }
        let mut code_folders = Vec::new();
        if let Some(d) = paths::user_dir("PROJECTS") {
            code_folders.push(d.to_string_lossy().into_owned());
        }
        Config { folders, code_folders, excluded: Vec::new(), gpu: true, paused: false, max_pages: 400, max_minutes: 240.0 }
    }
}

impl Config {
    pub fn load() -> Config {
        let file = paths::config().join("config.json");
        match std::fs::read_to_string(&file) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("sonda · {}: {e}; using the defaults", file.display());
                Config::default()
            }),
            Err(_) => {
                let c = Config::default();
                c.save();
                c
            }
        }
    }

    pub fn save(&self) {
        let file = paths::config().join("config.json");
        let tmp = file.with_extension("json.new");
        if let Ok(text) = serde_json::to_string_pretty(self) {
            if std::fs::write(&tmp, text + "\n").is_ok() {
                let _ = std::fs::rename(&tmp, &file);
            }
        }
    }

    pub fn is_excluded(&self, path: &str) -> bool {
        self.excluded.iter().any(|e| path == e || path.starts_with(&format!("{e}/")))
    }
}
