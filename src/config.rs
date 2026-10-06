use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::station::{builtin_stations, Station};

pub const DEFAULT_VOLUME: f32 = 0.7;
pub const VOLUME_STEP: f32 = 0.05;

/// On-disk config file shape (TOML).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileConfig {
    #[serde(default)]
    pub volume: Option<f32>,
    #[serde(default)]
    pub default_station: Option<String>,
    #[serde(default)]
    pub stations: Vec<Station>,
}

/// Resolved runtime config: builtin stations + user stations, volume, etc.
#[derive(Debug, Clone)]
pub struct Config {
    pub volume: f32,
    pub default_station: Option<String>,
    pub stations: Vec<Station>,
    /// Where this config was loaded from (None = defaults only).
    pub file_path: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            volume: DEFAULT_VOLUME,
            default_station: None,
            stations: builtin_stations(),
            file_path: None,
        }
    }
}

impl Config {
    /// Merge a parsed file config over the builtins.
    pub fn from_file_config(fc: FileConfig, path: Option<PathBuf>) -> Self {
        let mut stations = builtin_stations();
        stations.extend(fc.stations);
        let volume = fc.volume.unwrap_or(DEFAULT_VOLUME).clamp(0.0, 1.0);
        Self {
            volume,
            default_station: fc.default_station,
            stations,
            file_path: path,
        }
    }

    pub fn config_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "lofi")
            .map(|d| d.config_dir().join("config.toml"))
    }

    /// Load from the platform config dir. Missing file => defaults.
    /// Invalid TOML => Err (caller displays a human-readable message).
    pub fn load() -> Result<Self, ConfigError> {
        let Some(path) = Self::config_path() else {
            return Ok(Self::default());
        };
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| ConfigError::Read {
            path: path.clone(),
            detail: e.to_string(),
        })?;
        Self::from_toml(&text, Some(path))
    }

    pub fn from_toml(text: &str, path: Option<PathBuf>) -> Result<Self, ConfigError> {
        let fc: FileConfig =
            toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        // Validate custom station URLs early with a clear message.
        for s in &fc.stations {
            if s.name.trim().is_empty() {
                return Err(ConfigError::Parse(
                    "custom station with empty name".to_string(),
                ));
            }
            if !(s.url.starts_with("http://") || s.url.starts_with("https://")) {
                return Err(ConfigError::Parse(format!(
                    "station '{}' has invalid url (must start with http:// or https://)",
                    s.name
                )));
            }
        }
        Ok(Self::from_file_config(fc, path))
    }

    /// Persist current volume (and stations) back to the config file.
    /// Best-effort: failure is returned but never panics the TUI.
    pub fn save(&self) -> Result<(), ConfigError> {
        let Some(path) = Self::config_path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ConfigError::Read {
                path: parent.to_path_buf(),
                detail: e.to_string(),
            })?;
        }
        // Only persist user-added stations (not builtins) to avoid duplication.
        let builtin_len = builtin_stations().len();
        let custom: Vec<Station> = self.stations.iter().skip(builtin_len).cloned().collect();
        let fc = FileConfig {
            volume: Some(self.volume),
            default_station: self.default_station.clone(),
            stations: custom,
        };
        let text = toml::to_string_pretty(&fc).map_err(|e| ConfigError::Parse(e.to_string()))?;
        std::fs::write(&path, text).map_err(|e| ConfigError::Read {
            path: path.clone(),
            detail: e.to_string(),
        })?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read config file {path:?}: {detail}")]
    Read { path: PathBuf, detail: String },
    #[error("could not parse config: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_config() {
        let toml = r#"
volume = 0.5
default_station = "Groove Salad"

[[stations]]
name = "My Station"
description = "Custom radio"
url = "https://example.com/stream"
"#;
        let cfg = Config::from_toml(toml, None).unwrap();
        assert!((cfg.volume - 0.5).abs() < f32::EPSILON);
        assert_eq!(cfg.default_station.as_deref(), Some("Groove Salad"));
        // builtins + 1 custom
        assert_eq!(cfg.stations.len(), builtin_stations().len() + 1);
        assert_eq!(cfg.stations.last().unwrap().name, "My Station");
    }

    #[test]
    fn rejects_bad_url() {
        let toml = r#"
[[stations]]
name = "Bad"
url = "not-a-url"
"#;
        assert!(Config::from_toml(toml, None).is_err());
    }

    #[test]
    fn volume_is_clamped() {
        let cfg = Config::from_file_config(
            FileConfig {
                volume: Some(5.0),
                ..Default::default()
            },
            None,
        );
        assert!((cfg.volume - 1.0).abs() < f32::EPSILON);
    }
}
