// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::{InstanceType, Schema, SchemaObject, SubschemaValidation};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{ResolverError, ResolverResult};
use crate::ident::{Org, Package, PackageRef, TypeName};
use crate::semver::{SemVer, SemVerRange};

/// `streamlib.yaml` — single source of truth for a package or project.
///
/// **Package flavor** has a `package:` block declaring `org`/`name`/`version`
/// and is publishable. **Project flavor** has no `package:` block; it's a
/// consumer like an application or example.
///
/// Each manifest is **standalone**: it carries its own dep declarations
/// (`dependencies:`) and optional dev-time overrides (`patch:`) without
/// reaching into a tree-level shared dependency table. The streamlib model
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
    /// before falling through to the dep's declared source resolution.
    ///
    /// Path-flavor entries are dev-time overrides only — `streamlib pack`
    /// rejects yamls whose `patch:` table contains any `path:` entries
    /// (mirrors `npm publish` / `cargo publish` rejecting path deps).
    /// A path patch is a dev-loop-only affordance: at resolve time the
    /// resolver uses it only when its target exists on disk (the monorepo
    /// dev loop) and otherwise falls back to resolving the declared
    /// `dependencies:` version from the package source, so a path patch that
    /// ships in a published artifact never breaks a standalone consumer
    /// (the two-loops distribution model).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub patch: BTreeMap<PackageRef, DependencySpec>,

    /// Name-keyed declarations of the schemas this package surfaces — both
    /// schemas it owns (`Local { file }`) and types it imports from declared
    /// dependencies (`External { package }`).
    ///
    /// Use-site references in `processors[].config.schema` and
    /// `processors[].inputs/outputs[].schema` are bare type-name strings that
    /// resolve against this map. Discrimination between local vs external
    /// happens here at the declaration site; use-sites are parser-free.
    ///
    /// `None` → the resolver falls back to auto-discovery: every YAML file
    /// under `<root_dir>/schemas/` is treated as a Local entry, keyed by the
    /// schema file's `metadata.type` (or stem-derived PascalCase as a last
    /// resort for legacy reverse-DNS schemas).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schemas: Option<BTreeMap<TypeName, SchemaEntry>>,
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

    /// The number of declared dependencies when this manifest is an **app**
    /// manifest (project flavor — no `package:` block) that nonetheless carries
    /// a non-empty `dependencies:` block, else `None`.
    ///
    /// An app is code, not a manifest: it resolves processor refs against its
    /// installed set (`streamlib_modules/` + `streamlib.lock`), so a
    /// `dependencies:` list on an app manifest is a phantom-dependency
    /// declaration that never resolves — both `streamlib add` and the runtime
    /// load gate reject it. A **package**-flavor manifest's `dependencies:` are
    /// legitimate (the recursive walker resolves a loaded package's transitive
    /// deps), so this returns `None` for it.
    pub fn app_dependency_violation_count(&self) -> Option<usize> {
        (!self.is_package_flavor() && !self.dependencies.is_empty())
            .then_some(self.dependencies.len())
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

/// A single entry in a package's `schemas:` map.
///
/// Two flavors, discriminated by which key appears:
/// - `{ file: <relative path> }` — schema YAML lives under this package's
///   directory; the package owns the type.
/// - `{ package: "@org/name" }` — type is imported from a declared
///   dependency; resolution walks the dep's own `schemas:` map for the
///   bare name.
///
/// Both keys present, or neither, is a parse error — the discrimination
/// must be unambiguous at the declaration site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaEntry {
    /// Schema YAML file owned by this package, relative to the manifest dir.
    Local { file: PathBuf },
    /// Bare-name reference to a type owned by a declared dependency.
    External { package: PackageRef },
}

impl SchemaEntry {
    /// Convenience: file path for local entries, `None` for external.
    pub fn local_file(&self) -> Option<&Path> {
        match self {
            Self::Local { file } => Some(file),
            Self::External { .. } => None,
        }
    }

    /// Convenience: dep `PackageRef` for external entries, `None` for local.
    pub fn external_package(&self) -> Option<&PackageRef> {
        match self {
            Self::External { package } => Some(package),
            Self::Local { .. } => None,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SchemaEntryRaw {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    package: Option<PackageRef>,
}

impl Serialize for SchemaEntry {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Local { file } => SchemaEntryRaw {
                file: Some(file.clone()),
                package: None,
            }
            .serialize(ser),
            Self::External { package } => SchemaEntryRaw {
                file: None,
                package: Some(package.clone()),
            }
            .serialize(ser),
        }
    }
}

impl<'de> Deserialize<'de> for SchemaEntry {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = SchemaEntryRaw::deserialize(d)?;
        match (raw.file, raw.package) {
            (Some(file), None) => Ok(Self::Local { file }),
            (None, Some(package)) => Ok(Self::External { package }),
            (Some(_), Some(_)) => Err(serde::de::Error::custom(
                "schemas: entry has both `file:` and `package:` keys; pick one. \
                 `file:` declares a local schema this package owns; `package:` \
                 imports a bare type name from a declared dependency.",
            )),
            (None, None) => Err(serde::de::Error::custom(
                "schemas: entry has neither `file:` nor `package:` key; one is required. \
                 `file:` declares a local schema this package owns; `package:` \
                 imports a bare type name from a declared dependency.",
            )),
        }
    }
}

impl JsonSchema for SchemaEntry {
    fn schema_name() -> String {
        "SchemaEntry".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_idents::SchemaEntry")
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        use schemars::schema::ObjectValidation;
        let pkg_ref = generator.subschema_for::<PackageRef>();

        let local = Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::Object.into()),
            object: Some(Box::new(ObjectValidation {
                properties: {
                    let mut p = schemars::Map::new();
                    p.insert(
                        "file".into(),
                        Schema::Object(SchemaObject {
                            instance_type: Some(InstanceType::String.into()),
                            metadata: Some(Box::new(schemars::schema::Metadata {
                                description: Some(
                                    "Path to the schema YAML, relative to this manifest's directory."
                                        .into(),
                                ),
                                ..Default::default()
                            })),
                            ..Default::default()
                        }),
                    );
                    p
                },
                required: ["file".to_string()].into_iter().collect(),
                additional_properties: Some(Box::new(Schema::Bool(false))),
                ..Default::default()
            })),
            ..Default::default()
        });

        let external = Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::Object.into()),
            object: Some(Box::new(ObjectValidation {
                properties: {
                    let mut p = schemars::Map::new();
                    p.insert("package".into(), pkg_ref);
                    p
                },
                required: ["package".to_string()].into_iter().collect(),
                additional_properties: Some(Box::new(Schema::Bool(false))),
                ..Default::default()
            })),
            ..Default::default()
        });

        Schema::Object(SchemaObject {
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some(
                    "Schema declaration: either `{ file: path }` (local schema this package owns) \
                     or `{ package: \"@org/name\" }` (imported from a declared dependency)."
                        .into(),
                ),
                ..Default::default()
            })),
            subschemas: Some(Box::new(SubschemaValidation {
                one_of: Some(vec![local, external]),
                ..Default::default()
            })),
            ..Default::default()
        })
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
/// - String form `^1.2.3` → version dependency with a semver range, resolved
///   by version from a package source
/// - `{ path: ../foo }` → path dependency
/// - `{ git: ..., rev: ... }` → git dependency
///
/// `.slpkg` archives are an additional path-flavored source: a `path` value
/// that ends in `.slpkg` is treated as a zip archive that the resolver
/// extracts before reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum DependencySpec {
    Version(VersionDependency),
    Path(PathDependency),
    Git(GitDependency),
}

impl DependencySpec {
    /// Whether this dependency is a **runtime** dependency — one the package
    /// composes at runtime without importing any of its schema types. The
    /// build-time dependency reconciler derives a package's referenced set from
    /// its schema/port references; a declared dependency that resolves to none
    /// of them is otherwise reported as prunable. `runtime: true` marks the
    /// dependency as intentionally schema-unreferenced so the reconciler keeps
    /// it. A bare version-range string (`^1.2.3`) is never a runtime dependency.
    pub fn is_runtime(&self) -> bool {
        match self {
            Self::Version(r) => r.runtime,
            Self::Path(p) => p.runtime,
            Self::Git(g) => g.runtime,
        }
    }
}

/// `skip_serializing_if` predicate for the default-`false` `runtime` flag, so a
/// plain dependency serializes without a redundant `runtime: false`.
fn is_false(flag: &bool) -> bool {
    !*flag
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VersionDependency {
    pub version: SemVerRange,
    /// Runtime-only dependency marker — see [`DependencySpec::is_runtime`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub runtime: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PathDependency {
    pub path: PathBuf,
    /// Runtime-only dependency marker — see [`DependencySpec::is_runtime`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub runtime: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GitDependency {
    pub git: String,
    /// Pinned commit. Branch / tag refs are deliberately not supported —
    /// pinning is required for reproducible resolution. Mirrors the
    /// workspace rule from `CLAUDE.md` (`Conventions → Dependencies`).
    pub rev: String,
    /// Runtime-only dependency marker — see [`DependencySpec::is_runtime`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub runtime: bool,
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
        let version = generator.subschema_for::<VersionDependency>();
        let path = generator.subschema_for::<PathDependency>();
        let git = generator.subschema_for::<GitDependency>();
        Schema::Object(SchemaObject {
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some(
                    "Dependency declaration: a bare semver-range string (resolved by version from a package source), or one of the structured `{ version }` / `{ path }` / `{ git, rev }` maps."
                        .into(),
                ),
                ..Default::default()
            })),
            subschemas: Some(Box::new(SubschemaValidation {
                one_of: Some(vec![semver_range, version, path, git]),
                ..Default::default()
            })),
            ..Default::default()
        })
    }
}

// Custom Deserialize: a bare string `^1.2.3` is sugar for a version
// dependency (resolved by version from a package source). A map is one of
// the structured variants.
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
            Version(VersionDependency),
            Path(PathDependency),
            Git(GitDependency),
        }

        let repr = Repr::deserialize(d)?;
        Ok(match repr {
            Repr::Range(v) => Self::Version(VersionDependency {
                version: v,
                runtime: false,
            }),
            Repr::Map(StructuredRepr::Version(r)) => Self::Version(r),
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

    fn type_name(s: &str) -> TypeName {
        TypeName::new(s).unwrap()
    }

    #[test]
    fn schemas_map_round_trip() {
        let yaml = r#"
package:
  org: tatolab
  name: h264
  version: 1.0.0
schemas:
  H264EncoderConfig:
    file: schemas/h264_encoder_config.yaml
  VideoFrame:
    package: "@tatolab/core"
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        let schemas = m.schemas.expect("schemas: present");
        assert_eq!(schemas.len(), 2);
        match schemas.get(&type_name("H264EncoderConfig")).unwrap() {
            SchemaEntry::Local { file } => {
                assert_eq!(file, &PathBuf::from("schemas/h264_encoder_config.yaml"))
            }
            other => panic!("expected Local, got {:?}", other),
        }
        match schemas.get(&type_name("VideoFrame")).unwrap() {
            SchemaEntry::External { package } => assert_eq!(package, &pkg_ref("tatolab", "core")),
            other => panic!("expected External, got {:?}", other),
        }
    }

    #[test]
    fn schemas_entry_with_both_keys_rejected() {
        let yaml = r#"
package:
  org: tatolab
  name: foo
  version: 1.0.0
schemas:
  Foo:
    file: schemas/foo.yaml
    package: "@tatolab/core"
"#;
        let res: Result<Manifest, _> = serde_yaml::from_str(yaml);
        assert!(res.is_err());
        let msg = res.unwrap_err().to_string();
        assert!(
            msg.contains("both `file:` and `package:`"),
            "expected both-keys error message, got: {msg}",
        );
    }

    #[test]
    fn schemas_entry_with_neither_key_rejected() {
        let yaml = r#"
package:
  org: tatolab
  name: foo
  version: 1.0.0
schemas:
  Foo: {}
"#;
        let res: Result<Manifest, _> = serde_yaml::from_str(yaml);
        assert!(res.is_err());
        let msg = res.unwrap_err().to_string();
        assert!(
            msg.contains("neither `file:` nor `package:`"),
            "expected neither-key error message, got: {msg}",
        );
    }

    #[test]
    fn schemas_map_key_must_be_pascal_case_type_name() {
        // BTreeMap<TypeName, _> rejects keys that don't match the TypeName
        // grammar (`^[A-Z][A-Za-z0-9]*$`).
        let yaml = r#"
package:
  org: tatolab
  name: foo
  version: 1.0.0
schemas:
  not_pascal_case:
    file: schemas/foo.yaml
"#;
        let res: Result<Manifest, _> = serde_yaml::from_str(yaml);
        assert!(res.is_err());
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
    fn package_flavor_round_trips_prerelease_version() {
        // A package may carry a `-dev.N` / `-rc.N` prerelease version.
        let yaml = "
package:
  org: tatolab
  name: camera
  version: 0.4.33-dev.2
dependencies:
  \"@tatolab/core\": \">=1.0.0-rc.1\"
";
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        let pkg = m.package.as_ref().unwrap();
        assert_eq!(
            pkg.version,
            SemVer::new_prerelease(0, 4, 33, crate::semver::PrereleaseKind::Dev, 2)
        );
        // Serializes back to the canonical dotted-plus-suffix form.
        let out = serde_yaml::to_string(&m).unwrap();
        assert!(out.contains("0.4.33-dev.2"), "serialized: {out}");
        // A prerelease range dep round-trips too.
        match m.dependencies.get(&pkg_ref("tatolab", "core")).unwrap() {
            DependencySpec::Version(r) => assert_eq!(r.version.to_string(), ">=1.0.0-rc.1"),
            other => panic!("expected Version, got {:?}", other),
        }
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
            DependencySpec::Version(r) => assert_eq!(r.version.to_string(), "^1.0.0"),
            other => panic!("expected Version, got {:?}", other),
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
    fn app_dependency_violation_flags_project_flavor_with_deps() {
        // Project flavor (no `package:`) + non-empty `dependencies:` is the
        // phantom-dependency app manifest — flagged with the declared count.
        let app: Manifest = serde_yaml::from_str(
            "dependencies:\n  '@tatolab/core': ^1.0.0\n  '@tatolab/camera': ^2.0.0\n",
        )
        .unwrap();
        assert_eq!(app.app_dependency_violation_count(), Some(2));

        // Project flavor with no deps is fine.
        let empty_app: Manifest = serde_yaml::from_str("dependencies: {}\n").unwrap();
        assert_eq!(empty_app.app_dependency_violation_count(), None);

        // A package-flavor manifest's deps are legitimate — never flagged.
        let package: Manifest = serde_yaml::from_str(
            "package:\n  org: tatolab\n  name: widget\n  version: 0.1.0\n\
             dependencies:\n  '@tatolab/core': ^1.0.0\n",
        )
        .unwrap();
        assert_eq!(package.app_dependency_violation_count(), None);
    }

    #[test]
    fn manifest_load_missing_file_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let res = Manifest::load(tmp.path());
        assert!(matches!(res, Err(ResolverError::ManifestRead { .. })));
    }
}
