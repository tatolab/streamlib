// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use schemars::r#gen::SchemaGenerator;
use schemars::schema::{Schema, SchemaObject, SubschemaValidation};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{ResolverError, ResolverResult};
use crate::ident::{Org, Package, PackageRef};
use crate::semver::{SemVer, SemVerRange};

/// `streamlib.yaml` — single source of truth for a package or project.
///
/// **Package flavor** has a `package:` block declaring `org`/`name`/`version`
/// and is publishable. **Project flavor** has no `package:` block; it's a
/// consumer like an application or example.
///
/// Each manifest is **standalone**: it carries its own dep declarations
/// (`dependencies:`) and optional dev-time overrides (`patch:`) without
/// reaching into a tree-level shared registry. The streamlib model
/// matches `wrangler.toml` / `Cargo.toml` per-package shape — there is
/// no workspace walk-up; what you see in the yaml is what the runtime
/// resolves against.
///
/// The resolver reads `package`, `dependencies`, `patch`, and `schemas`.
/// Other top-level fields like `processors:` and `env:` are runtime
/// concerns owned by the streamlib runtime; they are tolerated here
/// without being interpreted, so one `streamlib.yaml` carries both
/// schema-identity and runtime configuration without splitting into
/// two files.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Manifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<PackageMetadata>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<PackageRef, DependencySpec>,

    /// Per-consumer resolution overrides. Mirrors Cargo's
    /// `[patch.crates-io]` shape but lives in the consumer's own yaml
    /// (no workspace walk-up). When the runtime / resolver iterates a
    /// dep declared in [`Self::dependencies`], it consults this table
    /// before falling through to the installed-package cache.
    ///
    /// Path-flavor entries are dev-time overrides only — `streamlib pack`
    /// rejects yamls whose `patch:` table contains any `path:` entries
    /// (mirrors `npm publish` / `cargo publish` rejecting path deps).
    /// Path patches are validated strictly at parse time: a missing path
    /// is a hard error so the dev knows immediately to fix the manifest.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub patch: BTreeMap<PackageRef, DependencySpec>,

    /// Explicit list of schema YAML files this package owns, relative to the
    /// manifest's directory. When `None`, the resolver auto-discovers
    /// `schemas/*.yaml` in the manifest dir.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schemas: Option<Vec<PathBuf>>,
}

impl Manifest {
    /// Conventional file name.
    pub const FILE_NAME: &'static str = "streamlib.yaml";

    /// Read a manifest from a directory containing `streamlib.yaml`.
    pub fn load(dir: &Path) -> ResolverResult<Self> {
        Self::load_file(&dir.join(Self::FILE_NAME))
    }

    /// Read a manifest from a specific file path.
    pub fn load_file(path: &Path) -> ResolverResult<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| ResolverError::ManifestRead {
            path: path.to_path_buf(),
            source: e,
        })?;
        serde_yaml::from_str(&content).map_err(|e| ResolverError::ManifestParse {
            path: path.to_path_buf(),
            source: e,
        })
    }

    /// True when this manifest declares its own `package:` block (publishable).
    pub fn is_package_flavor(&self) -> bool {
        self.package.is_some()
    }

    /// Canonical [`PackageRef`] for this manifest, when it's a package
    /// flavor. Consumers that need the joined `"@org/name"` string form
    /// (e.g. for lockfile keys, log messages) call `.to_string()` on the
    /// returned ref — the typed shape stays primary, the joined form
    /// stays render-only.
    pub fn package_ref(&self) -> Option<PackageRef> {
        self.package
            .as_ref()
            .map(|p| PackageRef::new(p.org.clone(), p.name.clone()))
    }

    /// Joined-string convenience for the canonical [`PackageRef`]. Prefer
    /// [`Self::package_ref`] in code; this is here for the resolver's
    /// internal lockfile-key + log-message paths.
    pub fn package_id(&self) -> Option<String> {
        self.package_ref().map(|r| r.to_string())
    }
}

/// Package metadata. `version` lives here and ONLY here — per the
/// package-as-publication-unit rule (CI lint rejects per-schema versions).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageMetadata {
    pub org: Org,
    pub name: Package,
    pub version: SemVer,
    #[serde(default)]
    pub description: Option<String>,
}

/// Dependency declaration. Three sources:
///
/// - String form `^1.2.3` → registry dependency with a semver range
/// - `{ path: ../foo }` → path dependency
/// - `{ git: ..., rev: ... }` → git dependency
///
/// `.slpkg` archives are an additional path-flavored source: a `path` value
/// that ends in `.slpkg` is treated as a zip archive that the resolver
/// extracts before reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum DependencySpec {
    Registry(RegistryDependency),
    Path(PathDependency),
    Git(GitDependency),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistryDependency {
    pub version: SemVerRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PathDependency {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GitDependency {
    pub git: String,
    /// Pinned commit. Branch / tag refs are deliberately not supported —
    /// pinning is required for reproducible resolution. Mirrors the
    /// workspace rule from `CLAUDE.md` (`Conventions → Dependencies`).
    pub rev: String,
}

impl JsonSchema for DependencySpec {
    fn schema_name() -> String {
        "DependencySpec".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_idents::DependencySpec")
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let semver_range = generator.subschema_for::<SemVerRange>();
        let registry = generator.subschema_for::<RegistryDependency>();
        let path = generator.subschema_for::<PathDependency>();
        let git = generator.subschema_for::<GitDependency>();
        Schema::Object(SchemaObject {
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some(
                    "Dependency declaration: a bare semver-range string (registry shorthand), or one of the structured `{ version }` / `{ path }` / `{ git, rev }` maps."
                        .into(),
                ),
                ..Default::default()
            })),
            subschemas: Some(Box::new(SubschemaValidation {
                one_of: Some(vec![semver_range, registry, path, git]),
                ..Default::default()
            })),
            ..Default::default()
        })
    }
}

// Custom Deserialize: a bare string `^1.2.3` is sugar for a Registry
// dependency. A map is one of the structured variants.
impl<'de> Deserialize<'de> for DependencySpec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Range(SemVerRange),
            Map(StructuredRepr),
        }

        #[derive(Deserialize)]
        #[serde(untagged, deny_unknown_fields)]
        enum StructuredRepr {
            Registry(RegistryDependency),
            Path(PathDependency),
            Git(GitDependency),
        }

        let repr = Repr::deserialize(d)?;
        Ok(match repr {
            Repr::Range(v) => Self::Registry(RegistryDependency { version: v }),
            Repr::Map(StructuredRepr::Registry(r)) => Self::Registry(r),
            Repr::Map(StructuredRepr::Path(p)) => Self::Path(p),
            Repr::Map(StructuredRepr::Git(g)) => Self::Git(g),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg_ref(org: &str, name: &str) -> PackageRef {
        PackageRef::new(Org::new(org).unwrap(), Package::new(name).unwrap())
    }

    #[test]
    fn package_flavor_round_trip() {
        let yaml = "
package:
  org: tatolab
  name: core
  version: 1.0.0
  description: Canonical wire vocabulary
dependencies: {}
";
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.is_package_flavor());
        let pkg = m.package.as_ref().unwrap();
        assert_eq!(pkg.org.as_str(), "tatolab");
        assert_eq!(pkg.name.as_str(), "core");
        assert_eq!(pkg.version, SemVer::new(1, 0, 0));
        assert_eq!(m.package_id().as_deref(), Some("@tatolab/core"));
        assert_eq!(m.package_ref(), Some(pkg_ref("tatolab", "core")));
        assert!(m.dependencies.is_empty());
    }

    #[test]
    fn project_flavor_with_three_dep_sources() {
        let yaml = r#"
dependencies:
  "@tatolab/core": "^1.0.0"
  "@tatolab/h264":
    path: ../h264
  "@tatolab/moq":
    git: https://github.com/tatolab/moq
    rev: abc123def456
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(!m.is_package_flavor());
        assert_eq!(m.package_id(), None);
        assert_eq!(m.dependencies.len(), 3);

        // Typed-key lookup — `BTreeMap<PackageRef, DependencySpec>` accepts
        // PackageRef keys directly. Mentally reverting to `BTreeMap<String, _>`
        // would force a string parser at the lookup site, which is the
        // structured-everywhere anti-pattern.
        match m.dependencies.get(&pkg_ref("tatolab", "core")).unwrap() {
            DependencySpec::Registry(r) => assert_eq!(r.version.to_string(), "^1.0.0"),
            other => panic!("expected Registry, got {:?}", other),
        }

        match m.dependencies.get(&pkg_ref("tatolab", "h264")).unwrap() {
            DependencySpec::Path(p) => assert_eq!(p.path, PathBuf::from("../h264")),
            other => panic!("expected Path, got {:?}", other),
        }

        match m.dependencies.get(&pkg_ref("tatolab", "moq")).unwrap() {
            DependencySpec::Git(g) => {
                assert_eq!(g.git, "https://github.com/tatolab/moq");
                assert_eq!(g.rev, "abc123def456");
            }
            other => panic!("expected Git, got {:?}", other),
        }
    }

    #[test]
    fn manifest_carries_per_consumer_patch_table() {
        // Each consumer manifest carries its own `patch:` block alongside
        // `dependencies:`. No workspace walk-up — what's in this yaml is
        // what gets resolved against. Mirrors npm's package.json carrying
        // its own `overrides:` and Cargo's per-package `[patch]`.
        let yaml = r#"
package:
  org: tatolab
  name: consumer
  version: 1.0.0
dependencies:
  "@tatolab/core": "^1.0.0"
patch:
  "@tatolab/core":
    path: ../packages/core
  "@tatolab/h264":
    git: https://github.com/tatolab/h264-fork
    rev: abc123
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.is_package_flavor());
        assert_eq!(m.dependencies.len(), 1);
        assert_eq!(m.patch.len(), 2);

        match m.patch.get(&pkg_ref("tatolab", "core")).unwrap() {
            DependencySpec::Path(p) => assert_eq!(p.path, PathBuf::from("../packages/core")),
            other => panic!("expected Path patch, got {:?}", other),
        }
        match m.patch.get(&pkg_ref("tatolab", "h264")).unwrap() {
            DependencySpec::Git(g) => assert_eq!(g.rev, "abc123"),
            other => panic!("expected Git patch, got {:?}", other),
        }
    }

    #[test]
    fn manifest_without_patch_block_round_trips_cleanly() {
        // The "customer-shape" yaml: declares deps canonically, no
        // dev-time patches. This is what a published / installed yaml
        // looks like — pack rejects the path-flavored variant, so the
        // wire form a customer sees never carries path overrides.
        let yaml = r#"
package:
  org: tatolab
  name: consumer
  version: 1.0.0
dependencies:
  "@tatolab/core": "^1.0.0"
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.is_package_flavor());
        assert_eq!(m.dependencies.len(), 1);
        assert!(m.patch.is_empty());
    }

    #[test]
    fn manifest_tolerates_runtime_extras() {
        // streamlib.yaml carries runtime fields like `processors:` and `env:`
        // that the resolver ignores. The manifest must NOT reject them with
        // `deny_unknown_fields` — that's the whole point of one file.
        let yaml = r#"
package:
  org: tatolab
  name: core
  version: 1.0.0
processors:
  - name: com.tatolab.foo
    version: 1.0.0
env:
  STREAMLIB_BACKEND: vulkan
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.is_package_flavor());
        assert_eq!(m.package_id().as_deref(), Some("@tatolab/core"));
    }

    #[test]
    fn manifest_rejects_invalid_org() {
        let yaml = "
package:
  org: Tatolab
  name: core
  version: 1.0.0
";
        let res: Result<Manifest, _> = serde_yaml::from_str(yaml);
        assert!(res.is_err());
    }

    #[test]
    fn manifest_rejects_invalid_version() {
        let yaml = "
package:
  org: tatolab
  name: core
  version: not-a-version
";
        let res: Result<Manifest, _> = serde_yaml::from_str(yaml);
        assert!(res.is_err());
    }

    #[test]
    fn git_dependency_requires_rev() {
        let yaml = r#"
dependencies:
  "@tatolab/moq":
    git: https://github.com/tatolab/moq
"#;
        let res: Result<Manifest, _> = serde_yaml::from_str(yaml);
        assert!(res.is_err(), "git dep without rev must fail");
    }

    #[test]
    fn manifest_load_from_directory() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n",
        )
        .unwrap();
        let m = Manifest::load(tmp.path()).unwrap();
        assert_eq!(m.package_id().as_deref(), Some("@tatolab/core"));
    }

    #[test]
    fn manifest_load_missing_file_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let res = Manifest::load(tmp.path());
        assert!(matches!(res, Err(ResolverError::ManifestRead { .. })));
    }
}
