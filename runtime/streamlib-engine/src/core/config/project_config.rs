// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Project-level configuration via `streamlib.yaml`.

use crate::core::{Error, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use streamlib_idents::{DependencySpec, PackageRef, SchemaEntry};
use streamlib_processor_schema::{Org, Package, SemVer, TypeName};

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

    /// Name-keyed schema declarations, mirroring
    /// [`streamlib_idents::Manifest::schemas`]. Each entry is either a
    /// `Local { file }` (schema file owned by this package) or
    /// `External { package }` (bare-name import from a declared dep).
    /// The runtime reads `Local` entries at `Runner::add_module` time
    /// and registers their YAML bodies with the engine's schema
    /// registry; `External` entries are import declarations only,
    /// resolved at `schemas:`-map lookup time. Absent from
    /// `streamlib.yaml` is fine — package may declare only `processors:`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schemas: Option<BTreeMap<TypeName, SchemaEntry>>,

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
    /// through to the dep's declared source resolution. Path-flavor entries are
    /// dev-time overrides only; `streamlib pack` rejects them.
    #[serde(default)]
    pub patch: BTreeMap<PackageRef, DependencySpec>,
}

impl ProjectConfig {
    /// Configuration file name.
    pub const FILE_NAME: &'static str = "streamlib.yaml";

    /// Load project configuration from a directory. Returns error if file is
    /// missing or cannot be parsed.
    ///
    /// Also runs the bare-name schema resolution pass (#767) on every
    /// processor's port + config schemas: any `PortSchemaSpec::Named`
    /// reference is resolved against the manifest's `schemas:` map and
    /// rewritten in-place to `PortSchemaSpec::Specific(SchemaIdent)`.
    /// Downstream consumers (graph wiring, iceoryx2 service open,
    /// json-schema render) operate on `Specific` only.
    pub fn load(project_path: &Path) -> Result<Self> {
        Self::load_with_link(project_path, None)
    }

    /// [`Self::load`], made link-aware for the runtime package-load path.
    ///
    /// When `link_checkout` is `Some` (a non-locked run under an active
    /// `streamlib link`), the bare-name schema-resolution pass resolves a
    /// schema dep present in `<checkout>/packages/<name>` from the checkout —
    /// the load-time mirror of the build-time redirect, so a linked package
    /// loaded from its staged cache dir with `STREAMLIB_PACKAGE_SOURCE` unset
    /// still resolves `@tatolab/core` (the zero-registry dev loop). `None`
    /// (every other caller, and every locked run) is byte-identical to before.
    /// The module loader threads its already-discovered, `is_locked`-gated
    /// checkout here.
    pub fn load_with_link(project_path: &Path, link_checkout: Option<&Path>) -> Result<Self> {
        let config_path = project_path.join(Self::FILE_NAME);

        let content = std::fs::read_to_string(&config_path).map_err(|e| {
            Error::Configuration(format!("Failed to read {}: {}", config_path.display(), e))
        })?;

        let mut config: Self = serde_yaml::from_str(&content).map_err(|e| {
            Error::Configuration(format!("Failed to parse {}: {}", config_path.display(), e))
        })?;

        config.resolve_bare_schema_refs(project_path, link_checkout)?;

        tracing::info!("Loaded project config from {}", config_path.display());
        Ok(config)
    }

    /// Walk every processor's port + config schemas and resolve any
    /// [`streamlib_processor_schema::PortSchemaSpec::Named`] reference
    /// to its fully-qualified [`streamlib_processor_schema::SchemaIdent`]
    /// against the manifest's `schemas:` map (#767). No-op when there
    /// are no `Named` references in scope (saves the resolver invocation
    /// cost on `any`-only / config-less manifests).
    fn resolve_bare_schema_refs(
        &mut self,
        project_path: &Path,
        link_checkout: Option<&Path>,
    ) -> Result<()> {
        use streamlib_processor_schema::PortSchemaSpec;

        let needs_resolution = self.processors.iter().any(|p| {
            p.config.is_some()
                || p.inputs
                    .iter()
                    .chain(p.outputs.iter())
                    .any(|port| matches!(port.schema, PortSchemaSpec::Named(_)))
        });

        if !needs_resolution {
            return Ok(());
        }

        // Runtime package-load boundary: read the package source from the
        // environment (STREAMLIB_PACKAGE_SOURCE) so a standalone package
        // resolves its schema deps (e.g. @tatolab/core) by version from that
        // package source instead of failing as PackageSourceNotConfigured. An
        // active `streamlib link` (threaded by the module loader) additionally
        // redirects a schema dep present in the checkout to the checkout — the
        // load-time half of the link dev loop; `None` is unchanged.
        let mut resolver_options = streamlib_idents::ResolverOptions::from_env();
        if let Some(checkout) = link_checkout {
            resolver_options.link_checkout = Some(checkout.to_path_buf());
        }
        let resolved =
            streamlib_idents::resolve_with(project_path, &resolver_options).map_err(|e| {
                Error::Configuration(format!(
                    "failed to resolve manifest dependencies for bare-name \
                 schema lookup at {}: {}",
                    project_path.display(),
                    e
                ))
            })?;

        for proc in &mut self.processors {
            for port in proc.inputs.iter_mut().chain(proc.outputs.iter_mut()) {
                if let PortSchemaSpec::Named(name) = &port.schema {
                    let ident = resolve_named_to_ident(&resolved, name).map_err(|msg| {
                        Error::Configuration(format!(
                            "processor `{}` port `{}`: {}",
                            proc.name, port.name, msg
                        ))
                    })?;
                    port.schema = PortSchemaSpec::Specific(ident);
                }
            }
            // Config schemas hold `TypeName` (Stage 2). They flow into
            // the runtime as canonical id strings via
            // `with_config_schema(&config.schema)`. Rather than
            // converting the field's type, we leave the `TypeName` in
            // place — runtime code paths use `as_str()` on it for the
            // descriptor field, and the canonical-id form is computed
            // lazily by the dynamic-registration path below
            // (`register_processor_descriptor` in runtime.rs) which
            // joins org/package/type/version on the fly. The bare-name
            // is sufficient at the descriptor layer; full resolution is
            // a future-tightening concern tracked under #767.
        }

        Ok(())
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

/// Walk the resolved-packages graph for a bare TypeName reference and
/// build the fully-qualified [`streamlib_processor_schema::SchemaIdent`]
/// from the owning package's metadata. Reads the schema file's
/// `metadata.type` (preferred) for the type segment; falls back to the
/// bare name when the YAML lacks `metadata.type` (legacy reverse-DNS
/// schemas with `metadata.name` only).
fn resolve_named_to_ident(
    resolved: &streamlib_idents::ResolvedPackages,
    name: &TypeName,
) -> std::result::Result<streamlib_processor_schema::SchemaIdent, String> {
    let (owner, schema_path) =
        streamlib_idents::resolve_bare_schema_name(resolved, &resolved.root, name)
            .map_err(|e| format!("bare-name resolution failed: {}", e))?;

    let owner_pkg = owner
        .manifest
        .package
        .as_ref()
        .ok_or_else(|| "owning package has no `package:` block".to_string())?;

    // Prefer `metadata.type` from the schema file; fall back to the
    // bare map-key name. Legacy reverse-DNS schemas with `metadata.name`
    // only carry no separate type segment, so the bare-name lookup form
    // is correct for them.
    let type_segment = std::fs::read_to_string(&schema_path)
        .ok()
        .and_then(|body| serde_yaml::from_str::<serde_yaml::Value>(&body).ok())
        .and_then(|value| {
            value
                .get("metadata")?
                .get("type")?
                .as_str()
                .and_then(|s| TypeName::new(s).ok())
        })
        .unwrap_or_else(|| name.clone());

    Ok(streamlib_processor_schema::SchemaIdent::new(
        owner_pkg.org.clone(),
        owner_pkg.name.clone(),
        type_segment,
        owner_pkg.version,
    ))
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
    fn resolve_named_to_ident_projects_prerelease_owner_to_release_core() {
        // Runtime mirror of the macro-side bare-name resolution. The ident it
        // mints flows onto `PortSchemaSpec`'s wire form, which carries only
        // major/minor/patch — `SchemaIdent::new`'s release-core projection is
        // what prevents a dev-versioned owner from silently truncating there.
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir_all(root.join("schemas")).unwrap();
        std::fs::write(
            root.join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: camera
  version: 0.4.33-dev.2
schemas:
  VideoFrame:
    file: schemas/video_frame.yaml
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("schemas/video_frame.yaml"),
            "metadata:\n  type: VideoFrame\nproperties: {}\n",
        )
        .unwrap();
        let resolved = streamlib_idents::resolve(&root).unwrap();
        let name = streamlib_processor_schema::TypeName::new("VideoFrame").unwrap();
        let ident = resolve_named_to_ident(&resolved, &name).unwrap();
        assert_eq!(ident.version, streamlib_idents::SemVer::new(0, 4, 33));
        assert_eq!(ident.to_string(), "@tatolab/camera/VideoFrame@0.4.33");
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
    fn test_load_with_version_dependency_short_form() {
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
        match config
            .dependencies
            .get(&pkg_ref("tatolab", "core"))
            .unwrap()
        {
            DependencySpec::Version(r) => {
                assert_eq!(r.version.to_string(), "^1.0.0");
            }
            other => panic!("expected Version dep, got {:?}", other),
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
        // Post-#767: port schemas at use-sites are bare PascalCase
        // TypeNames resolved against the manifest's `schemas:` map.
        // To keep this test self-contained (no on-disk dep package
        // required), the processor uses `any`-shaped ports — the
        // bare-name resolution pass is skipped entirely when no port
        // declares a `Named` reference.
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
    description: Grayscale processor
    runtime: python
    execution: reactive
    entrypoint: "grayscale_processor:GrayscaleProcessor"
    inputs:
      - name: video_in
        schema: any
    outputs:
      - name: video_out
        schema: any
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

    /// Removes the resolution-driving env for a test's duration and restores it
    /// after, so a stray `STREAMLIB_PACKAGE_SOURCE` / `STREAMLIB_LINK_CHECKOUT` in
    /// the shell can't make the load-time link tests non-deterministic.
    struct CleanResolutionEnv {
        prev_registry: Option<std::ffi::OsString>,
        prev_link: Option<std::ffi::OsString>,
    }
    impl CleanResolutionEnv {
        fn new() -> Self {
            let prev_registry = std::env::var_os("STREAMLIB_PACKAGE_SOURCE");
            let prev_link = std::env::var_os("STREAMLIB_LINK_CHECKOUT");
            // SAFETY: the tests using this guard are `#[serial]`, so no other
            // thread races the process-global environment for its lifetime.
            unsafe {
                std::env::remove_var("STREAMLIB_PACKAGE_SOURCE");
                std::env::remove_var("STREAMLIB_LINK_CHECKOUT");
            }
            Self {
                prev_registry,
                prev_link,
            }
        }
    }
    impl Drop for CleanResolutionEnv {
        fn drop(&mut self) {
            unsafe {
                match self.prev_registry.take() {
                    Some(v) => std::env::set_var("STREAMLIB_PACKAGE_SOURCE", v),
                    None => std::env::remove_var("STREAMLIB_PACKAGE_SOURCE"),
                }
                match self.prev_link.take() {
                    Some(v) => std::env::set_var("STREAMLIB_LINK_CHECKOUT", v),
                    None => std::env::remove_var("STREAMLIB_LINK_CHECKOUT"),
                }
            }
        }
    }

    /// Write a consumer manifest whose port declares a `Named` `VideoFrame`
    /// imported from `@tatolab/core` (a bare version dep, NO patch) — the shape
    /// that forces the load-time bare-name resolution to walk the dep graph.
    fn write_named_port_consumer(dir: &Path) {
        std::fs::write(
            dir.join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: consumer
  version: 1.0.0
dependencies:
  "@tatolab/core":
    version: ^1.0.0
schemas:
  VideoFrame:
    package: "@tatolab/core"
processors:
  - name: Consumer
    description: d
    runtime: rust
    execution: manual
    inputs:
      - name: in0
        schema: VideoFrame
    outputs: []
"#,
        )
        .unwrap();
    }

    /// Write `<checkout>/packages/core` declaring the `VideoFrame` Local schema.
    fn write_checkout_core(checkout: &Path) {
        let core = checkout.join("packages").join("core");
        std::fs::create_dir_all(core.join("schemas")).unwrap();
        std::fs::write(
            core.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\nschemas:\n  VideoFrame:\n    file: schemas/video_frame.yaml\n",
        )
        .unwrap();
        std::fs::write(
            core.join("schemas/video_frame.yaml"),
            "metadata:\n  type: VideoFrame\nproperties: {}\n",
        )
        .unwrap();
    }

    /// Load-time dev loop: `load_with_link` with an active checkout resolves a
    /// bare-name schema dep from the checkout's `packages/core` with ZERO
    /// registry configured — the load-time half of the zero-registry loop. The
    /// `Named` port is rewritten to a `Specific` ident owned by `@tatolab/core`.
    /// Mentally revert the `link_checkout` threading in `resolve_bare_schema_refs`
    /// (or pass `None`, as the sibling test does) and this fails
    /// `PackageSourceNotConfigured` — locking that the checkout is what resolved it.
    #[test]
    #[serial_test::serial]
    fn load_with_link_resolves_bare_schema_from_checkout_zero_registry() {
        use streamlib_processor_schema::PortSchemaSpec;
        let _env = CleanResolutionEnv::new();
        let tmp = TempDir::new().unwrap();
        let consumer = tmp.path().join("consumer");
        std::fs::create_dir_all(&consumer).unwrap();
        write_named_port_consumer(&consumer);
        let checkout = tmp.path().join("checkout");
        write_checkout_core(&checkout);

        let config = ProjectConfig::load_with_link(&consumer, Some(checkout.as_path())).expect(
            "linked load must resolve the bare-name schema from the checkout, zero registry",
        );
        let port = &config.processors[0].inputs[0];
        match &port.schema {
            PortSchemaSpec::Specific(ident) => {
                assert_eq!(ident.to_string(), "@tatolab/core/VideoFrame@1.0.0")
            }
            other => panic!("port schema must be resolved to a Specific ident, got {other:?}"),
        }
    }

    /// Load-time distribution guardrail: with NO link (`None`) and no registry,
    /// the same manifest fails `PackageSourceNotConfigured` — the checkout on disk is
    /// ignored without a link, so the no-link path is byte-identical to before.
    /// This is also the mental-revert of the positive test above.
    #[test]
    #[serial_test::serial]
    fn load_without_link_ignores_checkout_and_needs_registry() {
        let _env = CleanResolutionEnv::new();
        let tmp = TempDir::new().unwrap();
        let consumer = tmp.path().join("consumer");
        std::fs::create_dir_all(&consumer).unwrap();
        write_named_port_consumer(&consumer);
        // A checkout carrying core exists on disk, but no link is threaded.
        let checkout = tmp.path().join("checkout");
        write_checkout_core(&checkout);

        let err = ProjectConfig::load_with_link(&consumer, None)
            .expect_err("without a link + no package source, the bare version dep must fail loud");
        let msg = err.to_string();
        assert!(
            msg.contains("no package source is configured") || msg.contains("cannot be resolved"),
            "expected a package-source-not-configured failure, got: {msg}"
        );
    }
}
