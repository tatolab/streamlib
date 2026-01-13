// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Project-level configuration via `streamlib.toml`.
//!
//! Each processor project can have a `streamlib.toml` file that configures
//! runtime behavior. Currently supports:
//!
//! - `[env]` section: Environment variables to inject into subprocesses
//!
//! # Example
//!
//! ```toml
//! [env]
//! STREAMLIB_RHI_BACKEND = "opengl"
//! ```

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Project configuration from `streamlib.toml`.
#[derive(Debug, Default, Deserialize)]
pub struct ProjectConfig {
    /// Environment variables to inject into subprocesses.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl ProjectConfig {
    /// Configuration file name.
    pub const FILE_NAME: &'static str = "streamlib.toml";

    /// Load project configuration from a directory.
    ///
    /// Looks for `streamlib.toml` in the given directory. Returns default
    /// config if file doesn't exist.
    pub fn load(project_path: &Path) -> Self {
        let config_path = project_path.join(Self::FILE_NAME);

        if !config_path.exists() {
            tracing::debug!(
                "No {} found in {}, using defaults",
                Self::FILE_NAME,
                project_path.display()
            );
            return Self::default();
        }

        match std::fs::read_to_string(&config_path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(config) => {
                    tracing::info!("Loaded project config from {}", config_path.display());
                    config
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse {}: {}, using defaults",
                        config_path.display(),
                        e
                    );
                    Self::default()
                }
            },
            Err(e) => {
                tracing::warn!(
                    "Failed to read {}: {}, using defaults",
                    config_path.display(),
                    e
                );
                Self::default()
            }
        }
    }

    /// Get environment variables to inject.
    pub fn env_vars(&self) -> &HashMap<String, String> {
        &self.env
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_load_missing_file() {
        let dir = TempDir::new().unwrap();
        let config = ProjectConfig::load(dir.path());
        assert!(config.env.is_empty());
    }

    #[test]
    fn test_load_with_env() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("streamlib.toml");
        let mut file = std::fs::File::create(&config_path).unwrap();
        writeln!(file, "[env]").unwrap();
        writeln!(file, "STREAMLIB_RHI_BACKEND = \"opengl\"").unwrap();
        writeln!(file, "MY_CUSTOM_VAR = \"value\"").unwrap();

        let config = ProjectConfig::load(dir.path());
        assert_eq!(
            config.env.get("STREAMLIB_RHI_BACKEND"),
            Some(&"opengl".to_string())
        );
        assert_eq!(config.env.get("MY_CUSTOM_VAR"), Some(&"value".to_string()));
    }

    #[test]
    fn test_load_empty_file() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("streamlib.toml");
        std::fs::File::create(&config_path).unwrap();

        let config = ProjectConfig::load(dir.path());
        assert!(config.env.is_empty());
    }
}
