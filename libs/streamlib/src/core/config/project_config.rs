// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Project-level configuration via `streamlib.yaml`.

use crate::core::{Result, StreamError};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Package-level metadata from `streamlib.yaml`.
#[derive(Debug, Deserialize)]
pub struct PackageMetadata {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Minimum compatible StreamLib version (e.g. ">=0.3.0").
    #[serde(default)]
    pub streamlib_version: Option<String>,
}

/// Project configuration from `streamlib.yaml`.
#[derive(Debug, Default, Deserialize)]
pub struct ProjectConfig {
    /// Package metadata.
    #[serde(default)]
    pub package: Option<PackageMetadata>,

    /// Environment variables to inject into subprocesses.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Inline processor definitions.
    #[serde(default)]
    pub processors: Vec<streamlib_codegen_shared::ProcessorSchema>,
}

impl ProjectConfig {
    /// Configuration file name.
    pub const FILE_NAME: &'static str = "streamlib.yaml";

    /// Load project configuration from a directory. Returns error if file is
    /// missing or cannot be parsed.
    pub fn load(project_path: &Path) -> Result<Self> {
        let config_path = project_path.join(Self::FILE_NAME);

        let content = std::fs::read_to_string(&config_path).map_err(|e| {
            StreamError::Configuration(format!("Failed to read {}: {}", config_path.display(), e))
        })?;

        let config: Self = serde_yaml::from_str(&content).map_err(|e| {
            StreamError::Configuration(format!("Failed to parse {}: {}", config_path.display(), e))
        })?;

        tracing::info!("Loaded project config from {}", config_path.display());
        Ok(config)
    }

    /// Load project configuration from a directory, returning defaults if the
    /// file is missing or unparseable.
    pub fn load_or_default(project_path: &Path) -> Self {
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
            Ok(content) => match serde_yaml::from_str(&content) {
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

    /// Check if this package is compatible with the running StreamLib version.
    /// Returns an error if `streamlib_version` is set and the constraint is not
    /// satisfied.
    pub fn check_streamlib_version_compatibility(&self) -> Result<()> {
        let constraint = match self
            .package
            .as_ref()
            .and_then(|p| p.streamlib_version.as_deref())
        {
            Some(c) => c,
            None => return Ok(()),
        };

        let runtime_version = env!("CARGO_PKG_VERSION");

        // Parse ">=X.Y.Z" constraint
        let min_version = constraint
            .strip_prefix(">=")
            .ok_or_else(|| {
                StreamError::Configuration(format!(
                    "Unsupported streamlib_version constraint '{}' (only >=X.Y.Z is supported)",
                    constraint
                ))
            })?
            .trim();

        if compare_semver(runtime_version, min_version) < 0 {
            return Err(StreamError::Configuration(format!(
                "Package requires streamlib {} but running version is {}",
                constraint, runtime_version
            )));
        }

        Ok(())
    }
}

/// Compare two semver strings. Returns -1, 0, or 1.
fn compare_semver(a: &str, b: &str) -> i32 {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let va = parse(a);
    let vb = parse(b);
    let len = va.len().max(vb.len());
    for i in 0..len {
        let pa = va.get(i).copied().unwrap_or(0);
        let pb = vb.get(i).copied().unwrap_or(0);
        if pa < pb {
            return -1;
        }
        if pa > pb {
            return 1;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_load_missing_file_returns_error() {
        let dir = TempDir::new().unwrap();
        let result = ProjectConfig::load(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_load_or_default_missing_file() {
        let dir = TempDir::new().unwrap();
        let config = ProjectConfig::load_or_default(dir.path());
        assert!(config.env.is_empty());
    }

    #[test]
    fn test_load_with_env() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("streamlib.yaml");
        let mut file = std::fs::File::create(&config_path).unwrap();
        write!(
            file,
            r#"
env:
  STREAMLIB_RHI_BACKEND: opengl
  MY_CUSTOM_VAR: value
"#
        )
        .unwrap();

        let config = ProjectConfig::load(dir.path()).unwrap();
        assert_eq!(
            config.env.get("STREAMLIB_RHI_BACKEND"),
            Some(&"opengl".to_string())
        );
        assert_eq!(config.env.get("MY_CUSTOM_VAR"), Some(&"value".to_string()));
    }

    #[test]
    fn test_load_empty_file() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("streamlib.yaml");
        // serde_yaml parses empty content as None, so write empty mapping
        std::fs::write(&config_path, "{}").unwrap();

        let config = ProjectConfig::load(dir.path()).unwrap();
        assert!(config.env.is_empty());
    }

    #[test]
    fn test_load_with_processors() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("streamlib.yaml");
        let mut file = std::fs::File::create(&config_path).unwrap();
        write!(
            file,
            r#"
package:
  name: test-package
  version: "0.1.0"
  description: Test package

env:
  MY_VAR: value

processors:
  - name: com.test.grayscale
    version: "1.0.0"
    description: Grayscale processor
    runtime: python
    execution: reactive
    entrypoint: "grayscale_processor:GrayscaleProcessor"
    inputs:
      - name: video_in
        schema: com.tatolab.videoframe@1.0.0
    outputs:
      - name: video_out
        schema: com.tatolab.videoframe@1.0.0
"#
        )
        .unwrap();

        let config = ProjectConfig::load(dir.path()).unwrap();
        assert!(config.package.is_some());
        let pkg = config.package.unwrap();
        assert_eq!(pkg.name, "test-package");
        assert_eq!(pkg.version, "0.1.0");
        assert_eq!(config.env.get("MY_VAR"), Some(&"value".to_string()));
        assert_eq!(config.processors.len(), 1);
        assert_eq!(config.processors[0].name, "com.test.grayscale");
        assert_eq!(config.processors[0].inputs.len(), 1);
        assert_eq!(config.processors[0].outputs.len(), 1);
    }
}
