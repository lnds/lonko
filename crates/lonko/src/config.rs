// Lonko configuration file (~/.config/lonko/config.toml).
//
// Example:
//   [remote]
//   enabled = true
//   poll_interval_secs = 10
//   excluded_hosts = ["printer", "phone"]

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("lonko")
}

fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub remote: RemoteConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteConfig {
    pub enabled: bool,
    pub poll_interval_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            remote: RemoteConfig::default(),
        }
    }
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            poll_interval_secs: 10,
        }
    }
}

/// Load config from disk, returning defaults if the file is missing or malformed.
pub fn load() -> Config {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

// ── Excluded hosts (separate file, not in config.toml) ───────────────────────
// Kept in its own JSON file so lonko never writes to config.toml
// (preserving user comments, ordering, and unknown keys).

fn excluded_hosts_path() -> PathBuf {
    config_dir().join("excluded-hosts.json")
}

pub fn load_excluded_hosts() -> HashSet<String> {
    std::fs::read_to_string(excluded_hosts_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_excluded_hosts(excluded: &HashSet<String>) {
    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_string_pretty(excluded) {
        let _ = std::fs::write(excluded_hosts_path(), json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_remote_disabled() {
        let config = Config::default();
        assert!(!config.remote.enabled);
        assert_eq!(config.remote.poll_interval_secs, 10);
    }

    #[test]
    fn parses_minimal_toml() {
        let toml_str = r#"
            [remote]
            enabled = true
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.remote.enabled);
        assert_eq!(config.remote.poll_interval_secs, 10); // default
    }

    #[test]
    fn parses_full_toml() {
        let toml_str = r#"
            [remote]
            enabled = true
            poll_interval_secs = 30
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.remote.enabled);
        assert_eq!(config.remote.poll_interval_secs, 30);
    }

    #[test]
    fn empty_file_returns_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.remote.enabled);
    }
}
