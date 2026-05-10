// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Project-level configuration via `streamlib.yaml`.

use crate::core::{Result, Error};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use streamlib_idents::{DependencySpec, PackageRef};
use streamlib_processor_schema::{Org, Package, SemVer};

/// Package-level metadata from `streamlib.yaml`. Structured fields per the
/// architecture's "structured-everywhere" rule — every published streamlib
/// package owns its own `streamlib.yaml` with `package: { org, name,
/// version }`, and the runtime composes processor identities from these
/// typed segments.
#[derive(Debug, Deserialize)]
pub struct PackageMetadata {
    pub org: Org,
    pub name: Package,
    pub version: SemVer,
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

    /// Schema YAML paths declared by this package, relative to its
    /// manifest dir. The runtime reads each at `Runner::load_project`
    /// time and registers the YAML body with the engine's schema
    /// registry so `get_embedded_schema_definition` /
    /// `max_payload_bytes_for_schema` / api-server `/schemas` discover
    /// it. Absent from `streamlib.yaml` is fine — package may declare
    /// only `processors:` without explicit schema files.
    #[serde(default)]
    pub schemas: Vec<PathBuf>,

    /// Inline processor definitions.
    #[serde(default)]
    pub processors: Vec<streamlib_processor_schema::ProcessorSchema>,

    /// Package dependencies, keyed by canonical [`PackageRef`]. Mirrors
    /// [`streamlib_idents::Manifest::dependencies`] and the JSON Schema
    /// source-of-truth `StreamlibYaml` so a single declaration shape
    /// flows through the resolver, the editor schema, and the runtime
    /// loader. The typed-key contract from #717 means the runtime never
    /// parses a string at the lookup site — `PackageRef`'s `Deserialize`
    /// validates the canonical `@org/name` shape at YAML-read time.
    #[serde(default)]
    pub dependencies: BTreeMap<PackageRef, DependencySpec>,

    /// Per-consumer resolution overrides. Mirrors Cargo's
    /// `[patch.crates-io]` shape but lives in the consumer's own yaml —
    /// no workspace walk-up. When the runtime iterates a dep declared in
    /// [`Self::dependencies`], it consults this table before falling
    /// through to the installed-package cache. Path-flavor entries are
    /// dev-time overrides only; `streamlib pack` rejects them.
    #[serde(default)]
    pub patch: BTreeMap<PackageRef, DependencySpec>,
}

impl ProjectConfig {
    /// Configuration file name.
    pub const FILE_NAME: &'static str = "streamlib.yaml";

    /// Load project configuration from a directory. Returns error if file is
    /// missing or cannot be parsed.
    pub fn load(project_path: &Path) -> Result<Self> {
        let config_path = project_path.join(Self::FILE_NAME);

        let content = std::fs::read_to_string(&config_path).map_err(|e| {
            Error::Configuration(format!("Failed to read {}: {}", config_path.display(), e))
        })?;

        let config: Self = serde_yaml::from_str(&content).map_err(|e| {
            Error::Configuration(format!("Failed to parse {}: {}", config_path.display(), e))
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
                Error::Configuration(format!(
                    "Unsupported streamlib_version constraint '{}' (only >=X.Y.Z is supported)",
                    constraint
                ))
            })?
            .trim();

        if compare_semver(runtime_version, min_version) < 0 {
            return Err(Error::Configuration(format!(
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

    fn pkg_ref(org: &str, name: &str) -> PackageRef {
        PackageRef::new(Org::new(org).unwrap(), Package::new(name).unwrap())
    }

    #[test]
    fn test_load_with_path_dependency() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("streamlib.yaml");
        std::fs::write(
            &config_path,
            r#"
dependencies:
  "@tatolab/core":
    path: ../../packages/core
"#,
        )
        .unwrap();

        let config = ProjectConfig::load(dir.path()).unwrap();
        assert_eq!(config.dependencies.len(), 1);
        // Typed-key lookup. Mentally reverting `dependencies` to
        // `BTreeMap<String, _>` would force a string-parser at this lookup
        // site, which is the structured-everywhere anti-pattern.
        let spec = config
            .dependencies
            .get(&pkg_ref("tatolab", "core"))
            .expect("@tatolab/core dep present");
        match spec {
            DependencySpec::Path(p) => {
                assert_eq!(p.path.to_str().unwrap(), "../../packages/core");
            }
            other => panic!("expected Path dep, got {:?}", other),
        }
    }

    #[test]
    fn test_load_with_registry_dependency_short_form() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("streamlib.yaml");
        std::fs::write(
            &config_path,
            r#"
dependencies:
  "@tatolab/core": "^1.0.0"
"#,
        )
        .unwrap();

        let config = ProjectConfig::load(dir.path()).unwrap();
        assert_eq!(config.dependencies.len(), 1);
        match config.dependencies.get(&pkg_ref("tatolab", "core")).unwrap() {
            DependencySpec::Registry(r) => {
                assert_eq!(r.version.to_string(), "^1.0.0");
            }
            other => panic!("expected Registry dep, got {:?}", other),
        }
    }

    #[test]
    fn test_load_with_git_dependency() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("streamlib.yaml");
        std::fs::write(
            &config_path,
            r#"
dependencies:
  "@tatolab/moq":
    git: https://github.com/tatolab/moq
    rev: abc123def456
"#,
        )
        .unwrap();

        let config = ProjectConfig::load(dir.path()).unwrap();
        match config.dependencies.get(&pkg_ref("tatolab", "moq")).unwrap() {
            DependencySpec::Git(g) => {
                assert_eq!(g.git, "https://github.com/tatolab/moq");
                assert_eq!(g.rev, "abc123def456");
            }
            other => panic!("expected Git dep, got {:?}", other),
        }
    }

    #[test]
    fn test_load_rejects_invalid_canonical_key_at_parse_time() {
        // Post-#717: bare `tatolab/core` (no `@` prefix) fails to
        // deserialize as a `PackageRef` and surfaces as a yaml-parse error
        // before any runtime code runs. The structural invariant — that
        // invalid dep keys can't reach the lookup site — is locked here;
        // mentally reverting the `BTreeMap<PackageRef, _>` migration would
        // make this test hit a `Result::Ok` instead of an `Err`, since
        // `BTreeMap<String, _>` accepts any string.
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("streamlib.yaml");
        std::fs::write(
            &config_path,
            r#"
dependencies:
  "tatolab/core": "^1.0.0"
"#,
        )
        .unwrap();
        let result = ProjectConfig::load(dir.path());
        assert!(result.is_err(), "missing @ prefix must fail to deserialize");
    }

    #[test]
    fn test_load_rejects_legacy_sequence_dependencies() {
        // Pre-#716 ProjectConfig accepted `dependencies: [name1, name2]`. The
        // shape now matches the JSON Schema source-of-truth and the resolver,
        // so a bare sequence must error rather than silently parse against a
        // mismatched type.
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("streamlib.yaml");
        std::fs::write(
            &config_path,
            r#"
dependencies:
  - some-installed-package
"#,
        )
        .unwrap();

        let result = ProjectConfig::load(dir.path());
        assert!(result.is_err(), "legacy sequence-shaped deps must error");
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
  org: tatolab
  name: test-package
  version: "0.1.0"
  description: Test package

env:
  MY_VAR: value

processors:
  - name: Grayscale
    version: "1.0.0"
    description: Grayscale processor
    runtime: python
    execution: reactive
    entrypoint: "grayscale_processor:GrayscaleProcessor"
    inputs:
      - name: video_in
        schema: {{ org: tatolab, package: core, type: VideoFrame, version: 1.0.0 }}
    outputs:
      - name: video_out
        schema: {{ org: tatolab, package: core, type: VideoFrame, version: 1.0.0 }}
"#
        )
        .unwrap();

        let config = ProjectConfig::load(dir.path()).unwrap();
        assert!(config.package.is_some());
        let pkg = config.package.unwrap();
        assert_eq!(pkg.org.as_str(), "tatolab");
        assert_eq!(pkg.name.as_str(), "test-package");
        assert_eq!(pkg.version.to_string(), "0.1.0");
        assert_eq!(config.env.get("MY_VAR"), Some(&"value".to_string()));
        assert_eq!(config.processors.len(), 1);
        assert_eq!(config.processors[0].name, "Grayscale");
        assert_eq!(config.processors[0].inputs.len(), 1);
        assert_eq!(config.processors[0].outputs.len(), 1);
    }
}
