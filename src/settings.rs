use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::scene::Theme;

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    /// Animate units, birds, ships and water.
    pub animate: bool,
    /// Animation frame rate cap.
    pub fps: u32,
    /// Layout seed; "Shuffle layout" picks a new one.
    pub seed: u64,
    /// Optional custom AoE2 DE install folder.
    pub game_path: String,
    pub paused: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { theme: Theme::Arabia, animate: true, fps: 15, seed: 1, game_path: String::new(), paused: false }
    }
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Wololand")
}

impl Settings {
    pub fn load() -> Settings {
        std::fs::read_to_string(data_dir().join("settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(dir.join("settings.json"), s);
        }
    }
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "Wololand";

pub fn autostart_enabled() -> bool {
    windows_registry::CURRENT_USER.open(RUN_KEY).and_then(|k| k.get_string(RUN_NAME)).is_ok()
}

pub fn set_autostart(on: bool) {
    let Ok(key) = windows_registry::CURRENT_USER.create(RUN_KEY) else { return };
    if on {
        if let Ok(exe) = std::env::current_exe() {
            let _ = key.set_string(RUN_NAME, format!("\"{}\" --autostart", exe.display()));
        }
    } else {
        let _ = key.remove_value(RUN_NAME);
    }
}
