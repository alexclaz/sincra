//! Persistent GUI settings (same JSON file and keys as the Python version).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::engine::DEFAULT_MODE;
use crate::media::{base_cache_dir, home_dir};
use crate::project::Options;

pub fn settings_file() -> PathBuf {
    if cfg!(target_os = "macos") {
        home_dir().join("Library").join("Application Support").join("sincra").join("settings.json")
    } else {
        base_cache_dir().join("sincra-settings.json")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub mode: String,
    pub order: String,
    pub chronology: bool,
    pub label_synced: String,
    pub label_unsynced: String,
    pub suffix: String,
    pub use_cache: bool,
    pub cache_dir: String,
    pub path_map: Vec<(String, String)>,
    pub search_dirs: Vec<String>,
    pub last_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            mode: DEFAULT_MODE.into(),
            order: "auto".into(),
            chronology: true,
            label_synced: String::new(),
            label_unsynced: "Rose".into(),
            suffix: " - synced".into(),
            use_cache: true,
            cache_dir: String::new(),
            path_map: Vec::new(),
            search_dirs: Vec::new(),
            last_dir: String::new(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(settings_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let f = settings_file();
        if let Some(p) = f.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(f, s);
        }
    }

    pub fn options(&self) -> Options {
        let opt = |s: &str| if s.is_empty() { None } else { Some(s.to_string()) };
        Options {
            mode: self.mode.clone(),
            order: self.order.clone(),
            chronology: self.chronology,
            reference: None,
            label_synced: opt(&self.label_synced),
            label_unsynced: opt(&self.label_unsynced),
            suffix: self.suffix.clone(),
            use_cache: self.use_cache,
            cache_dir: opt(&self.cache_dir),
            path_map: self.path_map.clone(),
            search_dirs: self.search_dirs.clone(),
        }
    }
}
