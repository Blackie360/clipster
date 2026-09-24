//! TOML configuration with mtime-based hot reload.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Maximum number of unpinned entries retained. Pinned entries are never
    /// counted against this and never evicted.
    pub history_size: usize,
    /// Entries larger than this are dropped rather than stored. Guards the
    /// idle-RSS budget against someone copying a 200MB log file.
    pub max_item_bytes: usize,
    /// Skip entries that contain nothing but whitespace.
    pub ignore_whitespace_only: bool,
    #[serde(rename = "privacy")]
    pub privacy: Privacy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Privacy {
    /// Window classes whose clipboard writes are never recorded, matched as a
    /// case-insensitive substring against both WM_CLASS fields.
    ///
    /// Defaults are deliberately non-empty: an empty denylist means the first
    /// password you copy lands in a plaintext SQLite file, and no later
    /// release can un-write it.
    pub denylist: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            history_size: 500,
            max_item_bytes: 1024 * 1024,
            ignore_whitespace_only: true,
            privacy: Privacy::default(),
        }
    }
}

impl Default for Privacy {
    fn default() -> Self {
        Self {
            denylist: [
                "keepassxc",
                "keepass",
                "1password",
                "bitwarden",
                "enpass",
                "keeper",
                "seahorse",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("parsing config at {}", path.display())),
            // No config file is the normal case, not an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading config at {}", path.display())),
        }
    }

    /// True if `class` matches any denylist entry.
    pub fn is_denied(&self, class: &str) -> bool {
        let class = class.to_ascii_lowercase();
        self.privacy
            .denylist
            .iter()
            .any(|d| !d.is_empty() && class.contains(&d.to_ascii_lowercase()))
    }
}

/// Re-reads the config file when its mtime changes.
///
/// Polling mtime rather than using inotify keeps the daemon dependency-free
/// and costs one `stat` per clipboard event, which is far below the noise
/// floor of an X11 round trip.
pub struct ConfigWatcher {
    path: PathBuf,
    mtime: Option<SystemTime>,
    config: Config,
}

impl ConfigWatcher {
    pub fn new(path: PathBuf) -> Self {
        let mtime = Self::mtime_of(&path);
        let config = Config::load(&path).unwrap_or_else(|e| {
            log::warn!("{e:#}; falling back to defaults");
            Config::default()
        });
        Self { path, mtime, config }
    }

    fn mtime_of(path: &Path) -> Option<SystemTime> {
        std::fs::metadata(path).ok()?.modified().ok()
    }

    /// Returns the current config, reloading first if the file changed.
    ///
    /// A config that fails to parse is reported and ignored: a typo should not
    /// take the daemon's capture loop down with it.
    pub fn current(&mut self) -> &Config {
        let mtime = Self::mtime_of(&self.path);
        if mtime != self.mtime {
            self.mtime = mtime;
            match Config::load(&self.path) {
                Ok(cfg) => {
                    if cfg != self.config {
                        log::info!("config reloaded from {}", self.path.display());
                        self.config = cfg;
                    }
                }
                Err(e) => log::warn!("{e:#}; keeping previous config"),
            }
        }
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_yields_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.history_size, 500);
        assert!(!cfg.privacy.denylist.is_empty());
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let cfg: Config = toml::from_str("history_size = 42").unwrap();
        assert_eq!(cfg.history_size, 42);
        assert_eq!(cfg.max_item_bytes, 1024 * 1024);
    }

    #[test]
    fn unknown_key_is_an_error_not_a_silent_no_op() {
        assert!(toml::from_str::<Config>("histroy_size = 42").is_err());
    }

    #[test]
    fn denylist_matches_case_insensitive_substring() {
        let cfg = Config::default();
        assert!(cfg.is_denied("KeePassXC"));
        assert!(cfg.is_denied("org.keepassxc.KeePassXC"));
        assert!(!cfg.is_denied("Alacritty"));
    }

    #[test]
    fn empty_denylist_entry_does_not_match_everything() {
        let cfg = Config {
            privacy: Privacy { denylist: vec![String::new()] },
            ..Default::default()
        };
        assert!(!cfg.is_denied("Alacritty"));
    }
}
