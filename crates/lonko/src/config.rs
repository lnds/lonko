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
    pub excluded_hosts: HashSet<String>,
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
            excluded_hosts: HashSet::new(),
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

/// Save config to disk. Creates the config directory if needed.
pub fn save(config: &Config) {
    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(toml_str) = toml::to_string_pretty(config) {
        let _ = std::fs::write(config_path(), toml_str);
    }
}

/// Save only the excluded_hosts to the existing config (merge, don't overwrite).
pub fn save_excluded_hosts(excluded: &HashSet<String>) {
    let mut config = load();
    config.remote.excluded_hosts = excluded.clone();
    save(&config);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_remote_disabled() {
        let config = Config::default();
        assert!(!config.remote.enabled);
        assert_eq!(config.remote.poll_interval_secs, 10);
        assert!(config.remote.excluded_hosts.is_empty());
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
            excluded_hosts = ["printer", "phone"]
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.remote.enabled);
        assert_eq!(config.remote.poll_interval_secs, 30);
        assert!(config.remote.excluded_hosts.contains("printer"));
        assert!(config.remote.excluded_hosts.contains("phone"));
    }

    #[test]
    fn empty_file_returns_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.remote.enabled);
    }
}
