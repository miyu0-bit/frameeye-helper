//! What the helper remembers between runs. The password is never stored.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub with_panel: bool,
    /// "host:port" -> SHA256 fingerprint of the headset's SSH host key (trusted on first use).
    pub known_hosts: BTreeMap<String, String>,
    pub check_steamlink_osc_off: bool,
    pub check_vrchat_osc_on: bool,
    pub check_avatar: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: String::new(),
            user: "steamos".into(),
            port: 22,
            with_panel: true,
            known_hosts: BTreeMap::new(),
            check_steamlink_osc_off: false,
            check_vrchat_osc_on: false,
            check_avatar: false,
        }
    }
}

fn path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "FrameEyeHelper")?;
    Some(dirs.config_dir().join("settings.json"))
}

impl Settings {
    pub fn load() -> Self {
        path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Best effort: a read-only profile just means nothing is remembered.
    pub fn save(&self) {
        let Some(path) = path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let temporary = path.with_extension("json.tmp");
            if std::fs::write(&temporary, json).is_ok() {
                let _ = std::fs::rename(temporary, path);
            }
        }
    }

    pub fn host_key_id(host: &str, port: u16) -> String {
        format!("{}:{}", host.trim(), port)
    }
}
