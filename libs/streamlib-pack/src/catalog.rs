// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Publish-time catalog assembly.
//!
//! Turns a package's `streamlib.yaml` into the resolved
//! [`PackageCatalog`] + per-processor [`CatalogIndexLine`]s + the JSON Type
//! Definitions for the schemas it OWNS, ready for
//! [`crate::static_registry`] to write into the registry tree.
//!
//! Bare port / config schema references (`schema: VideoFrame`) are resolved
//! to release-core [`SchemaIdent`]s against the manifest's `schemas:` map:
//! a `Local` entry resolves to this package's own `(org, name, version)`; an
//! `External` entry resolves to the OWNING dependency's version, looked up in
//! the set of packages being published in the same release. Resolution
//! failures surface as a typed [`CatalogError`] — never a panic, never a
//! bare-name fallback.
//!
//! Schema-only packages (no `processors:` key, e.g. `@tatolab/core`) emit a
//! present-but-empty catalog plus their owned JTDs and contribute zero
//! aggregate index lines. Auto-discovery mode (no explicit `schemas:` map)
//! keys JTDs by each file's `metadata.type`; two files sharing a
//! `metadata.type` is malformed input and last-wins-overwrites one JTD —
//! documented, not defended.

use std::collections::BTreeMap;
use std::path::Path;

use streamlib_idents::{
    CatalogConfig, CatalogIndexLine, CatalogPort, CatalogProcessor, CatalogRuntime,
    CatalogSchemaRef, Manifest, PackageCatalog, PackageRef, SchemaEntry, SchemaIdent, SemVer,
    TypeName,
};
use streamlib_processor_schema::{
    PortSchemaSpec, ProcessorLanguage, ProcessorPortSchema, ProcessorSchema, ProjectConfigMinimal,
};

/// A published package's version + parsed manifest — the resolution universe
/// for external schema references. Keyed by [`PackageRef`] in a
/// [`SiblingVersions`] map.
#[derive(Debug, Clone)]
pub struct PublishedPackage {
    pub version: SemVer,
    pub manifest: Manifest,
}

/// Every package being published in the release, keyed by `@org/name`. Used
/// to resolve an `External { package }` schema reference to that package's
/// concrete version.
pub type SiblingVersions = BTreeMap<PackageRef, PublishedPackage>;

/// A schema's JSON Type Definition, ready to be written under the owning
/// package's `schemas/<Type>.jtd.json`.
#[derive(Debug, Clone)]
pub struct SchemaJtdFile {
    /// The PascalCase type this JTD defines.
    pub type_name: TypeName,
    /// The JTD document (the `schemas/*.yaml` converted to JSON).
    pub json: serde_json::Value,
}

/// Everything the tree emit needs to write for one package's catalog.
#[derive(Debug, Clone)]
pub struct PackageCatalogArtifacts {
    /// The per-package `<name>.catalog.json` payload.
    pub catalog: PackageCatalog,
    /// One line per processor for the registry-wide `catalog/index.ndjson`.
    pub index_lines: Vec<CatalogIndexLine>,
    /// The JTD files this package owns (deduped by ownership).
    pub schema_jtd: Vec<SchemaJtdFile>,
}

/// Why catalog assembly failed. Every variant carries actionable context;
/// resolution never panics and never silently drops a reference.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("read {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parse {path}: {source}")]
    ManifestParse {
        path: std::path::PathBuf,
        #[source]
        source: serde_yaml::Error,
    },

    #[error("{path} has no [package] block — a catalog requires a publishable package")]
    NotAPackage { path: std::path::PathBuf },

    #[error(
        "processor `{processor}` in `{package}` references schema type `{type_name}` \
         which is not declared in the manifest's `schemas:` map (resolution chain: {chain})"
    )]
    UnresolvedNamedSchema {
        package: String,
        processor: String,
        type_name: String,
        chain: String,
    },

    #[error(
        "schema type `{type_name}` in `{package}` is imported from dependency `{dep}`, \
         but `{dep}` is not among the packages being published — cannot resolve its version"
    )]
    ExternalDepMissing {
        package: String,
        type_name: String,
        dep: String,
    },

    #[error(
        "resolving schema type `{type_name}` in `{package}` cycles through external \
         imports: {chain}"
    )]
    SchemaResolutionCycle {
        package: String,
        type_name: String,
        chain: String,
    },

    #[error("parse schema YAML {path}: {source}")]
    SchemaParse {
        path: std::path::PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
}

/// Load a package directory's manifest + minimal project config. Shared by
/// the sibling-map builder and [`build_package_catalog`].
fn load_manifest(pkg_dir: &Path) -> Result<Manifest, CatalogError> {
    let path = pkg_dir.join(Manifest::FILE_NAME);
    let body = std::fs::read_to_string(&path).map_err(|e| CatalogError::Io {
        path: path.clone(),
        source: e,
    })?;
    serde_yaml::from_str(&body).map_err(|e| CatalogError::ManifestParse { path, source: e })
}

fn load_project_config(pkg_dir: &Path) -> Result<ProjectConfigMinimal, CatalogError> {
    let path = pkg_dir.join(Manifest::FILE_NAME);
    let body = std::fs::read_to_string(&path).map_err(|e| CatalogError::Io {
        path: path.clone(),
        source: e,
    })?;
    serde_yaml::from_str(&body).map_err(|e| CatalogError::ManifestParse { path, source: e })
}

/// Build the [`SiblingVersions`] resolution universe from a set of package
/// directories (each a `packages/<name>/`). Directories without a
/// `streamlib.yaml` or without a `[package]` block are skipped — only
/// publishable packages participate in external-ref resolution.
pub fn build_sibling_versions(pkg_dirs: &[std::path::PathBuf]) -> Result<SiblingVersions, CatalogError> {
    let mut out = SiblingVersions::new();
    for dir in pkg_dirs {
        if !dir.join(Manifest::FILE_NAME).is_file() {
            continue;
        }
        let manifest = load_manifest(dir)?;
        let Some(pkg) = manifest.package.as_ref() else {
            continue;
        };
        let pkg_ref = PackageRef::new(pkg.org.clone(), pkg.name.clone());
        let version = pkg.version;
        out.insert(pkg_ref, PublishedPackage { version, manifest });
    }
    Ok(out)
}

/// Assemble the catalog artifacts for the package at `pkg_dir`. `siblings`
/// carries the versions of every package in the release, for resolving
/// external schema references.
pub fn build_package_catalog(
    pkg_dir: &Path,
    siblings: &SiblingVersions,
) -> Result<PackageCatalogArtifacts, CatalogError> {
    let manifest = load_manifest(pkg_dir)?;
    let config = load_project_config(pkg_dir)?;

    let pkg_meta = manifest.package.as_ref().ok_or_else(|| CatalogError::NotAPackage {
        path: pkg_dir.join(Manifest::FILE_NAME),
    })?;
    let owner_ref = PackageRef::new(pkg_meta.org.clone(), pkg_meta.name.clone());
    let owner_version = pkg_meta.version;

    // Resolve every processor's config + ports.
    let mut processors = Vec::with_capacity(config.processors.len());
    for proc in &config.processors {
        processors.push(build_processor(proc, &owner_ref, owner_version, &manifest, siblings)?);
    }

    let catalog = PackageCatalog {
        package: owner_ref.clone(),
        version: owner_version,
        processors: processors.clone(),
    };
    let index_lines = processors
        .into_iter()
        .map(|processor| CatalogIndexLine {
            package: owner_ref.clone(),
            version: owner_version,
            processor,
        })
        .collect();

    let schema_jtd = collect_owned_schema_jtd(pkg_dir, &manifest)?;

    Ok(PackageCatalogArtifacts {
        catalog,
        index_lines,
        schema_jtd,
    })
}

fn build_processor(
    proc: &ProcessorSchema,
    owner_ref: &PackageRef,
    owner_version: SemVer,
    manifest: &Manifest,
    siblings: &SiblingVersions,
) -> Result<CatalogProcessor, CatalogError> {
    let config = match &proc.config {
        Some(cfg) => Some(CatalogConfig {
            name: cfg.name.clone(),
            schema: resolve_named(
                &cfg.schema,
                &proc.name,
                owner_ref,
                owner_version,
                manifest,
                siblings,
            )?,
        }),
        None => None,
    };

    let inputs = proc
        .inputs
        .iter()
        .map(|p| build_port(p, &proc.name, owner_ref, owner_version, manifest, siblings))
        .collect::<Result<Vec<_>, _>>()?;
    let outputs = proc
        .outputs
        .iter()
        .map(|p| build_port(p, &proc.name, owner_ref, owner_version, manifest, siblings))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(CatalogProcessor {
        name: proc.name.clone(),
        description: proc.description.clone(),
        runtime: runtime_of(&proc.runtime.language),
        entrypoint: proc.entrypoint.clone(),
        config,
        inputs,
        outputs,
    })
}

fn build_port(
    port: &ProcessorPortSchema,
    processor_name: &str,
    owner_ref: &PackageRef,
    owner_version: SemVer,
    manifest: &Manifest,
    siblings: &SiblingVersions,
) -> Result<CatalogPort, CatalogError> {
    let schema = match &port.schema {
        PortSchemaSpec::Any => CatalogSchemaRef::Any,
        PortSchemaSpec::Named(name) => CatalogSchemaRef::Schema(resolve_named(
            name,
            processor_name,
            owner_ref,
            owner_version,
            manifest,
            siblings,
        )?),
        // A `Specific` here is already resolved (uncommon from raw YAML, but
        // pass it through release-core-projected rather than re-resolving).
        PortSchemaSpec::Specific(ident) => CatalogSchemaRef::Schema(SchemaIdent::new(
            ident.org.clone(),
            ident.package.clone(),
            ident.r#type.clone(),
            ident.version,
        )),
    };
    Ok(CatalogPort {
        name: port.name.clone(),
        description: port.description.clone(),
        schema,
        read_mode: port.read_mode.clone(),
    })
}

fn runtime_of(language: &ProcessorLanguage) -> CatalogRuntime {
    match language {
        ProcessorLanguage::Rust => CatalogRuntime::Rust,
        ProcessorLanguage::Python => CatalogRuntime::Python,
        ProcessorLanguage::TypeScript => CatalogRuntime::TypeScript,
    }
}

/// Resolve a bare PascalCase `type_name` referenced by `processor_name` in
/// `owner` to a fully-qualified [`SchemaIdent`], walking `schemas:` maps the
/// same way [`streamlib_idents::resolve_bare_schema_name`] does — but sourced
/// from the in-flight release's sibling versions rather than a live registry.
fn resolve_named(
    type_name: &TypeName,
    processor_name: &str,
    owner: &PackageRef,
    owner_version: SemVer,
    owner_manifest: &Manifest,
    siblings: &SiblingVersions,
) -> Result<SchemaIdent, CatalogError> {
    let mut chain: Vec<String> = vec![owner.to_string()];
    resolve_named_internal(
        type_name,
        processor_name,
        owner,
        owner_version,
        owner_manifest,
        siblings,
        &mut chain,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_named_internal(
    type_name: &TypeName,
    processor_name: &str,
    owner: &PackageRef,
    owner_version: SemVer,
    owner_manifest: &Manifest,
    siblings: &SiblingVersions,
    chain: &mut Vec<String>,
) -> Result<SchemaIdent, CatalogError> {
    let root_package = chain.first().cloned().unwrap_or_else(|| owner.to_string());

    match owner_manifest.schemas.as_ref() {
        // Explicit `schemas:` map — look the type up.
        Some(map) => match map.get(type_name) {
            Some(SchemaEntry::Local { .. }) => Ok(local_ident(owner, type_name, owner_version)),
            Some(SchemaEntry::External { package: dep_ref }) => {
                let dep = siblings.get(dep_ref).ok_or_else(|| CatalogError::ExternalDepMissing {
                    package: root_package.clone(),
                    type_name: type_name.as_str().to_string(),
                    dep: dep_ref.to_string(),
                })?;
                // Guard against a mutually- or self-referential external chain
                // (A → B → A) — otherwise the recursion never terminates. The
                // contract is "resolution never panics"; a cycle surfaces as a
                // typed error, not a stack overflow.
                let dep_id = dep_ref.to_string();
                if chain.contains(&dep_id) {
                    chain.push(dep_id);
                    return Err(CatalogError::SchemaResolutionCycle {
                        package: root_package.clone(),
                        type_name: type_name.as_str().to_string(),
                        chain: chain.join(" -> "),
                    });
                }
                chain.push(dep_id);
                resolve_named_internal(
                    type_name,
                    processor_name,
                    dep_ref,
                    dep.version,
                    &dep.manifest,
                    siblings,
                    chain,
                )
            }
            None => Err(unresolved(&root_package, processor_name, type_name, chain)),
        },
        // No explicit map: auto-discovery treats every `schemas/*.yaml` this
        // package owns as Local. A type not owned locally is unresolvable
        // (an external ref is only expressible via an explicit map).
        None => Ok(local_ident(owner, type_name, owner_version)),
    }
}

fn local_ident(owner: &PackageRef, type_name: &TypeName, version: SemVer) -> SchemaIdent {
    SchemaIdent::new(owner.org.clone(), owner.name.clone(), type_name.clone(), version)
}

fn unresolved(
    package: &str,
    processor: &str,
    type_name: &TypeName,
    chain: &[String],
) -> CatalogError {
    CatalogError::UnresolvedNamedSchema {
        package: package.to_string(),
        processor: processor.to_string(),
        type_name: type_name.as_str().to_string(),
        chain: chain.join(" -> "),
    }
}

/// Collect the JTD files for schemas this package OWNS — the `Local` entries
/// of its `schemas:` map, or every `schemas/*.yaml` when the map is absent
/// (auto-discovery). Each YAML is parsed and re-serialized as JSON; the type
/// name is the map key (explicit) or the schema's `metadata.type` (discovery).
fn collect_owned_schema_jtd(
    pkg_dir: &Path,
    manifest: &Manifest,
) -> Result<Vec<SchemaJtdFile>, CatalogError> {
    let mut out = Vec::new();
    match manifest.schemas.as_ref() {
        Some(map) => {
            for (type_name, entry) in map {
                let SchemaEntry::Local { file } = entry else {
                    continue; // External types are emitted by their owning package.
                };
                let abs = if file.is_absolute() {
                    file.clone()
                } else {
                    pkg_dir.join(file)
                };
                let json = read_yaml_as_json(&abs)?;
                out.push(SchemaJtdFile {
                    type_name: type_name.clone(),
                    json,
                });
            }
        }
        None => {
            let schemas_dir = pkg_dir.join("schemas");
            if schemas_dir.is_dir() {
                let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&schemas_dir)
                    .map_err(|e| CatalogError::Io {
                        path: schemas_dir.clone(),
                        source: e,
                    })?
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| matches!(p.extension().and_then(|s| s.to_str()), Some("yaml" | "yml")))
                    .collect();
                files.sort();
                for path in files {
                    let json = read_yaml_as_json(&path)?;
                    if let Some(type_name) = jtd_type_name(&json) {
                        out.push(SchemaJtdFile { type_name, json });
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Read a schema YAML file and re-encode it as a JSON [`serde_json::Value`].
fn read_yaml_as_json(path: &Path) -> Result<serde_json::Value, CatalogError> {
    let body = std::fs::read_to_string(path).map_err(|e| CatalogError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let value: serde_json::Value =
        serde_yaml::from_str(&body).map_err(|e| CatalogError::SchemaParse {
            path: path.to_path_buf(),
            source: e,
        })?;
    Ok(value)
}

/// Extract `metadata.type` (PascalCase) from a parsed JTD document, for the
/// auto-discovery keying path.
fn jtd_type_name(json: &serde_json::Value) -> Option<TypeName> {
    json.get("metadata")
        .and_then(|m| m.get("type"))
        .and_then(|t| t.as_str())
        .and_then(|s| TypeName::new(s).ok())
}

/// Convenience: parse `<org>`/`<name>` from a raw `@org/name` — used by the
/// tree emit when building an owner ref from a manifest it already parsed.
pub fn owner_ref_of(manifest: &Manifest) -> Option<PackageRef> {
    manifest
        .package
        .as_ref()
        .map(|p| PackageRef::new(p.org.clone(), p.name.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use streamlib_idents::{Org, Package};

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// A schema-owning `@tatolab/core` + a multi-processor `@tatolab/camera`
    /// importing `VideoFrame` from core. Returns (tmp, camera_dir, siblings).
    fn two_package_tree() -> (tempfile::TempDir, PathBuf, SiblingVersions) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let core_dir = root.join("core");
        write(
            &core_dir,
            "streamlib.yaml",
            r#"
package:
  org: tatolab
  name: core
  version: 1.4.0
schemas:
  VideoFrame:
    file: schemas/video_frame.yaml
"#,
        );
        write(
            &core_dir,
            "schemas/video_frame.yaml",
            "metadata:\n  type: VideoFrame\n  description: A frame\nproperties:\n  width:\n    type: uint32\n",
        );

        let cam_dir = root.join("camera");
        write(
            &cam_dir,
            "streamlib.yaml",
            r#"
package:
  org: tatolab
  name: camera
  version: 2.1.0
dependencies:
  '@tatolab/core':
    version: ^1.0.0
schemas:
  CameraConfig:
    file: schemas/camera_config.yaml
  VideoFrame:
    package: '@tatolab/core'
processors:
- name: Camera
  version: 1.0.0
  description: Captures video
  runtime: rust
  execution: manual
  config:
    name: config
    schema: CameraConfig
  inputs: []
  outputs:
  - name: video
    schema: VideoFrame
    description: Live frames
- name: PassThrough
  version: 1.0.0
  runtime: python
  entrypoint: src.pass:PassThrough
  execution: reactive
  inputs:
  - name: any_in
    schema: any
    read_mode: skip_to_latest
  outputs:
  - name: video_out
    schema: VideoFrame
"#,
        );
        write(
            &cam_dir,
            "schemas/camera_config.yaml",
            "metadata:\n  type: CameraConfig\n  description: cfg\noptionalProperties:\n  device_id:\n    type: string\n",
        );

        let siblings = build_sibling_versions(&[core_dir, cam_dir.clone()]).unwrap();
        (tmp, cam_dir, siblings)
    }

    #[test]
    fn resolves_local_and_external_refs_to_release_core_idents() {
        let (_tmp, cam_dir, siblings) = two_package_tree();
        let arts = build_package_catalog(&cam_dir, &siblings).unwrap();

        assert_eq!(arts.catalog.package.to_string(), "@tatolab/camera");
        assert_eq!(arts.catalog.version, SemVer::new(2, 1, 0));
        assert_eq!(arts.catalog.processors.len(), 2);

        let camera = &arts.catalog.processors[0];
        assert_eq!(camera.name, "Camera");
        assert_eq!(camera.runtime, CatalogRuntime::Rust);
        // Config ref is Local → camera's own version.
        let cfg = camera.config.as_ref().unwrap();
        assert_eq!(cfg.schema.to_string(), "@tatolab/camera/CameraConfig@2.1.0");
        // Output port `video` is External(@tatolab/core) → core's version.
        assert_eq!(
            camera.outputs[0].schema,
            CatalogSchemaRef::Schema(SchemaIdent::new(
                Org::new("tatolab").unwrap(),
                Package::new("core").unwrap(),
                TypeName::new("VideoFrame").unwrap(),
                SemVer::new(1, 4, 0),
            ))
        );

        let passthrough = &arts.catalog.processors[1];
        assert_eq!(passthrough.runtime, CatalogRuntime::Python);
        assert_eq!(passthrough.entrypoint.as_deref(), Some("src.pass:PassThrough"));
        // `any` port stays a wildcard.
        assert_eq!(passthrough.inputs[0].schema, CatalogSchemaRef::Any);
        assert_eq!(passthrough.inputs[0].read_mode.as_deref(), Some("skip_to_latest"));
    }

    #[test]
    fn owns_only_locally_declared_schema_jtd() {
        let (_tmp, cam_dir, siblings) = two_package_tree();
        let arts = build_package_catalog(&cam_dir, &siblings).unwrap();
        // Camera owns CameraConfig (Local) but NOT VideoFrame (External → core owns it).
        let owned: Vec<&str> = arts.schema_jtd.iter().map(|s| s.type_name.as_str()).collect();
        assert_eq!(owned, vec!["CameraConfig"]);
        // The JTD is the YAML re-encoded as JSON.
        assert_eq!(arts.schema_jtd[0].json["metadata"]["type"], "CameraConfig");
    }

    #[test]
    fn index_lines_are_one_per_processor_with_owning_package() {
        let (_tmp, cam_dir, siblings) = two_package_tree();
        let arts = build_package_catalog(&cam_dir, &siblings).unwrap();
        assert_eq!(arts.index_lines.len(), 2);
        for line in &arts.index_lines {
            assert_eq!(line.package.to_string(), "@tatolab/camera");
            assert_eq!(line.version, SemVer::new(2, 1, 0));
        }
        assert_eq!(arts.index_lines[0].processor.name, "Camera");
    }

    #[test]
    fn missing_external_dep_is_typed_error_not_panic() {
        // Build the catalog for camera WITHOUT core in the sibling set —
        // the External VideoFrame ref must fail with a typed error.
        let (_tmp, cam_dir, _siblings) = two_package_tree();
        let mut siblings = SiblingVersions::new();
        // include only camera itself so the local lookup still finds the map
        let cam_manifest = load_manifest(&cam_dir).unwrap();
        siblings.insert(
            owner_ref_of(&cam_manifest).unwrap(),
            PublishedPackage {
                version: SemVer::new(2, 1, 0),
                manifest: cam_manifest,
            },
        );
        let err = build_package_catalog(&cam_dir, &siblings).unwrap_err();
        match err {
            CatalogError::ExternalDepMissing { dep, type_name, .. } => {
                assert_eq!(dep, "@tatolab/core");
                assert_eq!(type_name, "VideoFrame");
            }
            other => panic!("expected ExternalDepMissing, got {other:?}"),
        }
    }

    #[test]
    fn external_schema_reference_cycle_is_typed_error_not_stack_overflow() {
        // A ↔ B mutual `External` re-export of the same type would recurse
        // forever without the cycle guard. It must surface as a typed error.
        let tmp = tempfile::tempdir().unwrap();
        let a_dir = tmp.path().join("a");
        write(
            &a_dir,
            "streamlib.yaml",
            r#"
package:
  org: tatolab
  name: a
  version: 1.0.0
dependencies:
  '@tatolab/b':
    version: ^1.0.0
schemas:
  Loop:
    package: '@tatolab/b'
processors:
- name: A
  version: 1.0.0
  runtime: rust
  execution: reactive
  outputs:
  - name: out
    schema: Loop
"#,
        );
        let b_dir = tmp.path().join("b");
        write(
            &b_dir,
            "streamlib.yaml",
            r#"
package:
  org: tatolab
  name: b
  version: 1.0.0
dependencies:
  '@tatolab/a':
    version: ^1.0.0
schemas:
  Loop:
    package: '@tatolab/a'
"#,
        );
        let siblings = build_sibling_versions(&[a_dir.clone(), b_dir]).unwrap();
        let err = build_package_catalog(&a_dir, &siblings).unwrap_err();
        match err {
            CatalogError::SchemaResolutionCycle { type_name, chain, .. } => {
                assert_eq!(type_name, "Loop");
                assert!(chain.contains("@tatolab/a"));
                assert!(chain.contains("@tatolab/b"));
            }
            other => panic!("expected SchemaResolutionCycle, got {other:?}"),
        }
    }

    #[test]
    fn schema_only_package_emits_empty_catalog_and_owned_jtds() {
        // Real emit inputs include schema-only packages (core, escalate) with
        // no `processors:` key: catalog present-but-empty, owned JTDs
        // emitted, zero aggregate index lines.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("core");
        write(
            &dir,
            "streamlib.yaml",
            r#"
package:
  org: tatolab
  name: core
  version: 1.4.0
schemas:
  VideoFrame:
    file: schemas/video_frame.yaml
"#,
        );
        write(
            &dir,
            "schemas/video_frame.yaml",
            "metadata:\n  type: VideoFrame\nproperties:\n  width:\n    type: uint32\n",
        );
        let siblings = build_sibling_versions(&[dir.clone()]).unwrap();
        let arts = build_package_catalog(&dir, &siblings).unwrap();
        assert!(arts.catalog.processors.is_empty(), "no processors declared");
        assert!(arts.index_lines.is_empty(), "no index lines for a schema-only package");
        let owned: Vec<&str> = arts.schema_jtd.iter().map(|s| s.type_name.as_str()).collect();
        assert_eq!(owned, vec!["VideoFrame"], "owned JTDs still emitted");
    }

    /// Locks the deliberate version asymmetry: a `-dev.N` package's catalog
    /// carries the FULL prerelease version, while every schema ident it
    /// resolves is release-core (per the SchemaIdent invariant).
    #[test]
    fn prerelease_package_catalog_keeps_prerelease_but_idents_are_release_core() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("widget");
        write(
            &dir,
            "streamlib.yaml",
            r#"
package:
  org: tatolab
  name: widget
  version: 2.1.0-dev.3
schemas:
  WidgetConfig:
    file: schemas/widget_config.yaml
processors:
- name: Widget
  version: 1.0.0
  runtime: rust
  execution: reactive
  config:
    name: config
    schema: WidgetConfig
  outputs:
  - name: out
    schema: WidgetConfig
"#,
        );
        write(
            &dir,
            "schemas/widget_config.yaml",
            "metadata:\n  type: WidgetConfig\nproperties: {}\n",
        );
        let siblings = build_sibling_versions(&[dir.clone()]).unwrap();
        let arts = build_package_catalog(&dir, &siblings).unwrap();
        // Catalog version: the full published prerelease.
        assert_eq!(arts.catalog.version.to_string(), "2.1.0-dev.3");
        assert_eq!(arts.index_lines[0].version.to_string(), "2.1.0-dev.3");
        // Idents: release-core projected.
        let cfg = arts.catalog.processors[0].config.as_ref().unwrap();
        assert_eq!(cfg.schema.to_string(), "@tatolab/widget/WidgetConfig@2.1.0");
        let port = arts.catalog.processors[0].outputs[0].schema.schema().unwrap();
        assert_eq!(port.version, SemVer::new(2, 1, 0));
    }

    #[test]
    fn undeclared_named_schema_is_typed_error() {
        // A processor references a type not in the schemas map.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("widget");
        write(
            &dir,
            "streamlib.yaml",
            r#"
package:
  org: tatolab
  name: widget
  version: 1.0.0
schemas:
  KnownType:
    file: schemas/known.yaml
processors:
- name: Widget
  version: 1.0.0
  runtime: rust
  execution: reactive
  outputs:
  - name: out
    schema: MysteryType
"#,
        );
        write(&dir, "schemas/known.yaml", "metadata:\n  type: KnownType\nproperties: {}\n");
        let siblings = build_sibling_versions(&[dir.clone()]).unwrap();
        let err = build_package_catalog(&dir, &siblings).unwrap_err();
        match err {
            CatalogError::UnresolvedNamedSchema { type_name, processor, .. } => {
                assert_eq!(type_name, "MysteryType");
                assert_eq!(processor, "Widget");
            }
            other => panic!("expected UnresolvedNamedSchema, got {other:?}"),
        }
    }

    /// Mentally revert the resolver to emit the owner's version for an
    /// external ref (drop the sibling lookup) and this fails: the external
    /// ref MUST carry the dependency's version, not the importer's.
    #[test]
    fn external_ref_version_is_the_dependencys_not_the_importers() {
        let (_tmp, cam_dir, siblings) = two_package_tree();
        let arts = build_package_catalog(&cam_dir, &siblings).unwrap();
        let video = arts.catalog.processors[0].outputs[0].schema.schema().unwrap();
        assert_eq!(video.version, SemVer::new(1, 4, 0)); // core's version
        assert_ne!(video.version, SemVer::new(2, 1, 0)); // NOT camera's version
    }
}
