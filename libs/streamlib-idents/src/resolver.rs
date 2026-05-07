// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib.yaml` dependency resolver.
//!
//! Given a root manifest, walks declared dependencies via path / git / .slpkg
//! sources, validates package identifiers + semver ranges, and returns a
//! [`ResolvedPackages`] set along with content hashes that drive
//! `streamlib.lock`.
//!
//! The resolver is the keystone of the milestone-10 architecture: it converts
//! the structured `Manifest` into the input shape codegen consumes (a flat
//! set of `(SchemaIdent, JtdSchema)` pairs grouped by package).

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{ResolverError, ResolverResult};
use crate::ident::PackageRef;
use crate::lockfile::{compute_content_hash, Lockfile, LockfileEntry, LockfileSource};
use crate::manifest::{DependencySpec, Manifest};

/// Outcome of resolving a `streamlib.yaml` graph: the root project + every
/// transitive package, keyed by canonical `"@org/name"` lockfile key.
///
/// `root` is the entry point manifest the resolver was invoked on; it's
/// accessible separately from `packages` because:
///
/// 1. A project-flavor root has no `package_id()` and therefore no place in
///    the lockfile.
/// 2. Even when the root *is* a package, the lockfile records *its
///    dependencies*, not the package itself (mirrors `Cargo.lock`).
#[derive(Debug, Clone)]
pub struct ResolvedPackages {
    pub root: ResolvedPackage,
    pub packages: BTreeMap<String, ResolvedPackage>,
}

impl ResolvedPackages {
    /// Iterate root + every dependency.
    pub fn iter_all(&self) -> impl Iterator<Item = &ResolvedPackage> {
        std::iter::once(&self.root).chain(self.packages.values())
    }

    /// Build a `Lockfile` from the dependency set (root excluded — the lock
    /// records dependencies, not the consumer).
    pub fn to_lockfile(&self) -> Lockfile {
        let mut packages = BTreeMap::new();
        for (key, pkg) in &self.packages {
            let entry = LockfileEntry {
                version: pkg
                    .manifest
                    .package
                    .as_ref()
                    .map(|p| p.version)
                    .expect("dependencies must be package-flavor manifests"),
                source: pkg.source.to_lockfile_source(),
                content_hash: pkg.content_hash.clone(),
            };
            packages.insert(key.clone(), entry);
        }
        Lockfile {
            version: 1,
            packages,
        }
    }
}

/// A resolved package: the loaded manifest, its root directory on disk, the
/// schema files it owns, the source it was resolved from, and a content hash
/// that locks the pair.
#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub manifest: Manifest,
    pub root_dir: PathBuf,
    pub schema_files: Vec<PathBuf>,
    pub source: ResolvedSource,
    pub content_hash: String,
}

/// Resolved provenance: where this package came from after walking the
/// dependency graph.
#[derive(Debug, Clone)]
pub enum ResolvedSource {
    /// The root manifest the resolver was invoked on. Not in the lockfile.
    Root,
    /// Resolved from a path dependency (manifest path stored relative to the
    /// consumer's manifest dir, mirroring how the user wrote the dep).
    Path { relative: PathBuf },
    /// Resolved from a git pinned commit.
    Git { url: String, rev: String },
    /// Resolved from a `.slpkg` archive (path to the archive is stored).
    Slpkg { archive: PathBuf },
    /// Resolved from a registry. Reserved — v1 does not implement a
    /// registry, so this variant is constructable only by future code paths.
    Registry { url: String },
}

impl ResolvedSource {
    fn to_lockfile_source(&self) -> LockfileSource {
        match self {
            Self::Root => unreachable!("root source never lands in the lockfile"),
            Self::Path { relative } => LockfileSource::Path {
                path: relative.clone(),
            },
            Self::Git { url, rev } => LockfileSource::Git {
                url: url.clone(),
                rev: rev.clone(),
            },
            Self::Slpkg { archive } => LockfileSource::Path {
                path: archive.clone(),
            },
            Self::Registry { url } => LockfileSource::Registry { url: url.clone() },
        }
    }
}

/// Configuration for [`resolve`]. Defaults to `~/.streamlib/resolver-cache/`
/// for git + `.slpkg` extraction storage.
#[derive(Debug, Clone, Default)]
pub struct ResolverOptions {
    /// Override the cache directory for git clones and `.slpkg` extractions.
    /// `None` falls back to `$HOME/.streamlib/resolver-cache/`.
    pub cache_dir: Option<PathBuf>,
}

/// Resolve a `streamlib.yaml` at `root_dir` and every transitive dependency
/// it declares.
pub fn resolve(root_dir: &Path) -> ResolverResult<ResolvedPackages> {
    resolve_with(root_dir, &ResolverOptions::default())
}

/// Resolve with explicit options.
pub fn resolve_with(
    root_dir: &Path,
    options: &ResolverOptions,
) -> ResolverResult<ResolvedPackages> {
    let root_manifest_path = root_dir.join(Manifest::FILE_NAME);
    let root_manifest = Manifest::load_file(&root_manifest_path)?;

    let cache_dir = match &options.cache_dir {
        Some(p) => p.clone(),
        None => default_cache_dir()?,
    };

    let root = build_resolved_package(
        root_manifest,
        root_dir.to_path_buf(),
        ResolvedSource::Root,
    )?;

    // The dep map is now typed end-to-end: `BTreeMap<PackageRef, _>`. The
    // canonical-string lookup-key invariants the resolver previously
    // hand-validated in `parse_dep_key` are now enforced by `PackageRef`'s
    // Deserialize at YAML-read time — invalid keys never reach this code.
    let mut packages: BTreeMap<String, ResolvedPackage> = BTreeMap::new();
    let mut queue: VecDeque<(PathBuf, BTreeMap<PackageRef, DependencySpec>)> = VecDeque::new();
    queue.push_back((root.root_dir.clone(), root.manifest.dependencies.clone()));

    let mut visiting: HashSet<String> = HashSet::new();

    while let Some((consumer_dir, deps)) = queue.pop_front() {
        for (dep_ref, spec) in deps {
            // Lockfile + return-shape are still keyed on the canonical
            // joined-string form (yaml map keys are strings on disk). The
            // typed PackageRef is the in-memory primary; `Display` is the
            // wire form, used here only for the lockfile key.
            let dep_id = dep_ref.to_string();

            if let Some(existing) = packages.get(&dep_id) {
                check_existing_satisfies_spec(existing, &spec, &consumer_dir, &dep_id)?;
                continue;
            }

            if !visiting.insert(dep_id.clone()) {
                return Err(ResolverError::CircularDependency { chain: dep_id });
            }

            let resolved = resolve_one(
                &consumer_dir,
                &dep_ref,
                &dep_id,
                &spec,
                &cache_dir,
            )?;

            check_resolved_id_matches(&resolved, &dep_id, &consumer_dir)?;
            check_resolved_satisfies_spec(&resolved, &spec, &consumer_dir, &dep_id)?;

            queue.push_back((resolved.root_dir.clone(), resolved.manifest.dependencies.clone()));
            packages.insert(dep_id.clone(), resolved);
            visiting.remove(&dep_id);
        }
    }

    Ok(ResolvedPackages { root, packages })
}

fn resolve_one(
    consumer_dir: &Path,
    dep_ref: &PackageRef,
    dep_id: &str,
    spec: &DependencySpec,
    cache_dir: &Path,
) -> ResolverResult<ResolvedPackage> {
    match spec {
        DependencySpec::Path(path_dep) => resolve_path_dependency(
            consumer_dir,
            dep_id,
            path_dep,
            cache_dir,
        ),
        DependencySpec::Git(git_dep) => {
            let target = fetch_git(dep_id, &git_dep.git, &git_dep.rev, cache_dir)?;
            let manifest = Manifest::load(&target)?;
            build_resolved_package(
                manifest,
                target,
                ResolvedSource::Git {
                    url: git_dep.git.clone(),
                    rev: git_dep.rev.clone(),
                },
            )
        }
        DependencySpec::Registry(_) => {
            // Registry deps resolve through the workspace `[patch]` table
            // (#717). The resolver mirrors the runtime's three-tier chain
            // for codegen/lockfile purposes — if the patch entry redirects
            // to a path or git location, recurse through `resolve_one`
            // with the patched spec. Without a workspace patch, the
            // resolver still fails (no registry server in v1).
            if let Some(patched_path) =
                lookup_workspace_patch_path(consumer_dir, dep_ref)?
            {
                return resolve_path_dependency(
                    consumer_dir,
                    dep_id,
                    &crate::manifest::PathDependency {
                        path: patched_path,
                    },
                    cache_dir,
                );
            }
            Err(ResolverError::RegistryNotImplemented {
                name: dep_id.to_string(),
            })
        }
    }
}

fn resolve_path_dependency(
    consumer_dir: &Path,
    dep_id: &str,
    path_dep: &crate::manifest::PathDependency,
    cache_dir: &Path,
) -> ResolverResult<ResolvedPackage> {
    let abs = if path_dep.path.is_absolute() {
        path_dep.path.clone()
    } else {
        consumer_dir.join(&path_dep.path)
    };
    if !abs.exists() {
        return Err(ResolverError::PathDependencyNotFound {
            name: dep_id.to_string(),
            path: abs,
        });
    }
    // `.slpkg` archive (path-flavored): extract first.
    if abs.extension().and_then(|s| s.to_str()) == Some("slpkg") {
        let extracted = extract_slpkg(&abs, cache_dir)?;
        let manifest = Manifest::load(&extracted)?;
        return build_resolved_package(
            manifest,
            extracted,
            ResolvedSource::Slpkg { archive: abs },
        );
    }
    if !abs.is_dir() {
        return Err(ResolverError::PathDependencyNotDirectory {
            name: dep_id.to_string(),
            path: abs,
        });
    }
    let manifest = Manifest::load(&abs)?;
    let relative = path_dep.path.clone();
    build_resolved_package(manifest, abs, ResolvedSource::Path { relative })
}

/// Walk up from `consumer_dir` for a workspace-flavor manifest and
/// consult its `patch:` table for `dep_ref`. Returns the absolute path
/// to redirect to when a path-style patch entry is found; `None`
/// otherwise. Workspace-level `path:` entries are resolved relative
/// to the workspace root (the same idiom Cargo uses for
/// `[patch.crates-io] foo = { path = "vendor/foo" }`).
///
/// Patch entries that are themselves `Registry` or `Git` are not
/// supported by the resolver today — workspace overrides are concrete
/// pointers, not further indirections.
fn lookup_workspace_patch_path(
    consumer_dir: &Path,
    dep_ref: &PackageRef,
) -> ResolverResult<Option<PathBuf>> {
    let Some(workspace) = crate::workspace::discover_workspace(consumer_dir) else {
        return Ok(None);
    };
    let Some(patch_spec) = workspace.manifest.patch.get(dep_ref) else {
        return Ok(None);
    };
    match patch_spec {
        DependencySpec::Path(p) => Ok(Some(if p.path.is_absolute() {
            p.path.clone()
        } else {
            workspace.root.join(&p.path)
        })),
        DependencySpec::Registry(_) | DependencySpec::Git(_) => {
            Err(ResolverError::WorkspacePatchUnsupportedShape {
                name: dep_ref.to_string(),
                workspace_root: workspace.root,
            })
        }
    }
}

fn build_resolved_package(
    manifest: Manifest,
    root_dir: PathBuf,
    source: ResolvedSource,
) -> ResolverResult<ResolvedPackage> {
    let schema_files = discover_schema_files(&manifest, &root_dir)?;

    let manifest_path = root_dir.join(Manifest::FILE_NAME);
    let manifest_body = if manifest_path.exists() {
        std::fs::read_to_string(&manifest_path).map_err(|e| ResolverError::ManifestRead {
            path: manifest_path.clone(),
            source: e,
        })?
    } else {
        // Manifests synthesized in tests may have no on-disk file; serialize.
        serde_yaml::to_string(&manifest).map_err(|e| ResolverError::ManifestParse {
            path: manifest_path.clone(),
            source: e,
        })?
    };

    let mut schema_pairs = Vec::with_capacity(schema_files.len());
    for path in &schema_files {
        let rel = path
            .strip_prefix(&root_dir)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());
        let body = std::fs::read_to_string(path).map_err(|e| ResolverError::Io {
            path: path.clone(),
            source: e,
        })?;
        schema_pairs.push((rel, body));
    }

    let content_hash = compute_content_hash(&manifest_body, &schema_pairs);

    Ok(ResolvedPackage {
        manifest,
        root_dir,
        schema_files,
        source,
        content_hash,
    })
}

/// Discover the schema files this manifest owns. Two modes:
///
/// 1. Explicit: `manifest.schemas: [path1, path2, ...]` — relative to root_dir.
/// 2. Implicit: every `*.yaml` under `<root_dir>/schemas/` (sorted).
fn discover_schema_files(manifest: &Manifest, root_dir: &Path) -> ResolverResult<Vec<PathBuf>> {
    if let Some(declared) = &manifest.schemas {
        let mut files = Vec::with_capacity(declared.len());
        for rel in declared {
            let abs = if rel.is_absolute() {
                rel.clone()
            } else {
                root_dir.join(rel)
            };
            if !abs.exists() {
                return Err(ResolverError::SchemaNotFound {
                    path: abs,
                    from: root_dir.join(Manifest::FILE_NAME),
                });
            }
            files.push(abs);
        }
        return Ok(files);
    }

    let schemas_dir = root_dir.join("schemas");
    if !schemas_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    let entries = std::fs::read_dir(&schemas_dir).map_err(|e| ResolverError::Io {
        path: schemas_dir.clone(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| ResolverError::Io {
            path: schemas_dir.clone(),
            source: e,
        })?;
        let path = entry.path();
        let ext = path.extension().and_then(|s: &OsStr| s.to_str());
        if matches!(ext, Some("yaml") | Some("yml")) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn check_resolved_id_matches(
    resolved: &ResolvedPackage,
    requested: &str,
    consumer_dir: &Path,
) -> ResolverResult<()> {
    match resolved.manifest.package_id() {
        Some(declared) if declared == requested => Ok(()),
        Some(declared) => Err(ResolverError::PackageIdMismatch {
            path: consumer_dir.join(Manifest::FILE_NAME),
            declared,
            requested: requested.to_string(),
        }),
        None => Err(ResolverError::PackageIdMismatch {
            path: consumer_dir.join(Manifest::FILE_NAME),
            declared: "<no package block>".into(),
            requested: requested.to_string(),
        }),
    }
}

fn check_resolved_satisfies_spec(
    resolved: &ResolvedPackage,
    spec: &DependencySpec,
    consumer_dir: &Path,
    dep_id: &str,
) -> ResolverResult<()> {
    let DependencySpec::Registry(reg) = spec else {
        return Ok(());
    };
    let v = resolved
        .manifest
        .package
        .as_ref()
        .map(|p| p.version)
        .ok_or_else(|| ResolverError::PackageIdMismatch {
            path: consumer_dir.join(Manifest::FILE_NAME),
            declared: "<no package block>".into(),
            requested: dep_id.to_string(),
        })?;
    if !reg.version.matches(v) {
        return Err(ResolverError::VersionRangeUnsatisfied {
            name: dep_id.to_string(),
            from: consumer_dir.join(Manifest::FILE_NAME),
            found: v.to_string(),
            range: reg.version.to_string(),
        });
    }
    Ok(())
}

fn check_existing_satisfies_spec(
    existing: &ResolvedPackage,
    spec: &DependencySpec,
    consumer_dir: &Path,
    dep_id: &str,
) -> ResolverResult<()> {
    if let DependencySpec::Registry(reg) = spec {
        let v = existing
            .manifest
            .package
            .as_ref()
            .map(|p| p.version)
            .expect("existing dep is package-flavor");
        if !reg.version.matches(v) {
            return Err(ResolverError::VersionRangeConflict {
                name: dep_id.to_string(),
                range_a: format!("(already-resolved {})", v),
                from_a: existing.root_dir.join(Manifest::FILE_NAME),
                range_b: reg.version.to_string(),
                from_b: consumer_dir.join(Manifest::FILE_NAME),
            });
        }
    }
    Ok(())
}

fn default_cache_dir() -> ResolverResult<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| ResolverError::Io {
            path: PathBuf::from("$HOME"),
            source: std::io::Error::other("HOME environment variable not set"),
        })?;
    Ok(home.join(".streamlib").join("resolver-cache"))
}

fn fetch_git(name: &str, url: &str, rev: &str, cache_dir: &Path) -> ResolverResult<PathBuf> {
    let safe = url
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect::<String>();
    let target = cache_dir.join("git").join(format!("{}_{}", safe, rev));

    let manifest_path = target.join(Manifest::FILE_NAME);
    if manifest_path.exists() {
        return Ok(target);
    }

    std::fs::create_dir_all(&target).map_err(|e| ResolverError::Io {
        path: target.clone(),
        source: e,
    })?;

    let clone = Command::new("git")
        .args(["clone", "--quiet", url, "."])
        .current_dir(&target)
        .output()
        .map_err(|e| ResolverError::GitDependencyFailed {
            name: name.to_string(),
            url: url.to_string(),
            message: format!("git clone invocation failed: {e}"),
        })?;
    if !clone.status.success() {
        return Err(ResolverError::GitDependencyFailed {
            name: name.to_string(),
            url: url.to_string(),
            message: String::from_utf8_lossy(&clone.stderr).trim().to_string(),
        });
    }

    let checkout = Command::new("git")
        .args(["checkout", "--quiet", rev])
        .current_dir(&target)
        .output()
        .map_err(|e| ResolverError::GitDependencyFailed {
            name: name.to_string(),
            url: url.to_string(),
            message: format!("git checkout invocation failed: {e}"),
        })?;
    if !checkout.status.success() {
        return Err(ResolverError::GitDependencyFailed {
            name: name.to_string(),
            url: url.to_string(),
            message: format!(
                "git checkout {} failed: {}",
                rev,
                String::from_utf8_lossy(&checkout.stderr).trim()
            ),
        });
    }

    Ok(target)
}

fn extract_slpkg(archive: &Path, cache_dir: &Path) -> ResolverResult<PathBuf> {
    let archive_bytes = std::fs::read(archive).map_err(|e| ResolverError::SlpkgExtractFailed {
        path: archive.to_path_buf(),
        message: format!("read failed: {e}"),
    })?;
    let archive_hash = crate::lockfile::hash_content(&archive_bytes);
    let safe_hash = archive_hash.replace(':', "_");
    let target = cache_dir.join("slpkg").join(safe_hash);

    let manifest_path = target.join(Manifest::FILE_NAME);
    if manifest_path.exists() {
        return Ok(target);
    }

    std::fs::create_dir_all(&target).map_err(|e| ResolverError::Io {
        path: target.clone(),
        source: e,
    })?;

    let cursor = std::io::Cursor::new(&archive_bytes);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| ResolverError::SlpkgExtractFailed {
        path: archive.to_path_buf(),
        message: format!("not a valid zip: {e}"),
    })?;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| ResolverError::SlpkgExtractFailed {
            path: archive.to_path_buf(),
            message: format!("entry {i} read failed: {e}"),
        })?;
        let entry_name = entry.name().to_string();
        // Reject path traversal.
        if entry_name.contains("..") || entry_name.starts_with('/') {
            return Err(ResolverError::SlpkgExtractFailed {
                path: archive.to_path_buf(),
                message: format!("rejected unsafe entry path: {entry_name}"),
            });
        }
        let out_path = target.join(&entry_name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| ResolverError::Io {
                path: out_path,
                source: e,
            })?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ResolverError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        let mut out = std::fs::File::create(&out_path).map_err(|e| ResolverError::Io {
            path: out_path.clone(),
            source: e,
        })?;
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| ResolverError::SlpkgExtractFailed {
                path: archive.to_path_buf(),
                message: format!("read entry {entry_name} failed: {e}"),
            })?;
        std::io::Write::write_all(&mut out, &buf).map_err(|e| ResolverError::Io {
            path: out_path,
            source: e,
        })?;
    }

    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_yaml(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn write_streamlib_yaml(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        write_yaml(dir, Manifest::FILE_NAME, body);
    }

    #[test]
    fn resolve_root_only_no_dependencies() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n",
        );
        let res = resolve(&root).unwrap();
        assert!(res.packages.is_empty());
        assert_eq!(res.root.manifest.package_id().as_deref(), Some("@tatolab/core"));
        assert!(res.root.content_hash.starts_with("sha256:"));
        assert!(matches!(res.root.source, ResolvedSource::Root));
    }

    #[test]
    fn resolve_path_dependency() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let core = tmp.path().join("core");

        write_streamlib_yaml(
            &core,
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n",
        );
        std::fs::create_dir_all(core.join("schemas")).unwrap();
        write_yaml(
            &core.join("schemas"),
            "VideoFrame.yaml",
            "metadata:\n  name: VideoFrame\nproperties:\n  width:\n    type: uint32\n",
        );

        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/core":
    path: ../core
"#,
        );

        let res = resolve(&root).unwrap();
        assert_eq!(res.packages.len(), 1);
        let core_pkg = res.packages.get("@tatolab/core").unwrap();
        assert_eq!(core_pkg.schema_files.len(), 1);
        assert!(matches!(core_pkg.source, ResolvedSource::Path { .. }));
        assert!(core_pkg.content_hash.starts_with("sha256:"));
    }

    #[test]
    fn resolve_missing_path_dependency() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/core":
    path: ../core
"#,
        );
        let err = resolve(&root).unwrap_err();
        assert!(matches!(err, ResolverError::PathDependencyNotFound { .. }));
    }

    #[test]
    fn resolve_path_dependency_id_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let core = tmp.path().join("core");
        write_streamlib_yaml(
            &core,
            "package:\n  org: tatolab\n  name: notcore\n  version: 1.0.0\n",
        );
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/core":
    path: ../core
"#,
        );
        let err = resolve(&root).unwrap_err();
        assert!(matches!(err, ResolverError::PackageIdMismatch { .. }));
    }

    #[test]
    fn resolve_invalid_dep_key_shape() {
        // Post-#717 the canonical-key shape is enforced at YAML parse time
        // by `PackageRef::Deserialize`, not in the resolver. A bare
        // `"tatolab/core"` (missing `@` prefix) fails to deserialize as a
        // PackageRef and surfaces as a `ManifestParse` error. The structural
        // intent — that invalid shapes can't reach the resolver's lookup
        // logic — is preserved; the rejection just moves earlier.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "tatolab/core":
    path: ../core
"#,
        );
        let err = resolve(&root).unwrap_err();
        assert!(
            matches!(err, ResolverError::ManifestParse { .. }),
            "expected ManifestParse, got {:?}",
            err,
        );
    }

    #[test]
    fn resolve_transitive_path_dependencies() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let h264 = tmp.path().join("h264");
        let core = tmp.path().join("core");

        write_streamlib_yaml(
            &core,
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n",
        );
        write_streamlib_yaml(
            &h264,
            r#"
package:
  org: tatolab
  name: h264
  version: 0.4.0
dependencies:
  "@tatolab/core":
    path: ../core
"#,
        );
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/h264":
    path: ../h264
"#,
        );

        let res = resolve(&root).unwrap();
        assert_eq!(res.packages.len(), 2);
        assert!(res.packages.contains_key("@tatolab/core"));
        assert!(res.packages.contains_key("@tatolab/h264"));
    }

    #[test]
    fn resolve_diamond_dependency_dedupes() {
        // root → a, b
        // a → core
        // b → core
        // Expect `core` resolved once, not twice.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        let core = tmp.path().join("core");

        write_streamlib_yaml(
            &core,
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n",
        );
        write_streamlib_yaml(
            &a,
            r#"
package:
  org: tatolab
  name: a
  version: 1.0.0
dependencies:
  "@tatolab/core":
    path: ../core
"#,
        );
        write_streamlib_yaml(
            &b,
            r#"
package:
  org: tatolab
  name: b
  version: 1.0.0
dependencies:
  "@tatolab/core":
    path: ../core
"#,
        );
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/a":
    path: ../a
  "@tatolab/b":
    path: ../b
"#,
        );

        let res = resolve(&root).unwrap();
        assert_eq!(res.packages.len(), 3);
        assert!(res.packages.contains_key("@tatolab/core"));
        assert!(res.packages.contains_key("@tatolab/a"));
        assert!(res.packages.contains_key("@tatolab/b"));
    }

    #[test]
    fn registry_dependency_not_implemented() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/core": "^1.0.0"
"#,
        );
        let err = resolve(&root).unwrap_err();
        assert!(matches!(err, ResolverError::RegistryNotImplemented { .. }));
    }

    #[test]
    fn slpkg_path_dependency_extracts_and_loads() {
        // Build a minimal .slpkg in a temp dir, then resolve a path dep
        // that points at the archive file.
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let archive_path = tmp.path().join("core.slpkg");

        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive_path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file(Manifest::FILE_NAME, opts).unwrap();
        zip.write_all(b"package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n")
            .unwrap();
        zip.start_file("schemas/VideoFrame.yaml", opts).unwrap();
        zip.write_all(b"metadata:\n  name: VideoFrame\nproperties: {}\n")
            .unwrap();
        zip.finish().unwrap();

        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            &format!(
                "dependencies:\n  \"@tatolab/core\":\n    path: {}\n",
                archive_path.to_string_lossy()
            ),
        );

        let opts = ResolverOptions {
            cache_dir: Some(cache),
        };
        let res = resolve_with(&root, &opts).unwrap();
        let core = res.packages.get("@tatolab/core").unwrap();
        assert!(matches!(core.source, ResolvedSource::Slpkg { .. }));
        assert_eq!(core.schema_files.len(), 1);
    }

    #[test]
    fn lockfile_built_from_resolved_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let core = tmp.path().join("core");

        write_streamlib_yaml(
            &core,
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n",
        );
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/core":
    path: ../core
"#,
        );
        let res = resolve(&root).unwrap();
        let lock = res.to_lockfile();
        assert_eq!(lock.version, 1);
        assert_eq!(lock.packages.len(), 1);
        let entry = lock.packages.get("@tatolab/core").unwrap();
        assert_eq!(entry.version.to_string(), "1.0.0");
        assert!(matches!(entry.source, LockfileSource::Path { .. }));
        assert!(entry.content_hash.starts_with("sha256:"));
    }

    #[test]
    fn schema_auto_discovery_picks_yaml_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n",
        );
        let schemas = root.join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        write_yaml(&schemas, "VideoFrame.yaml", "metadata:\n  name: VideoFrame\n");
        write_yaml(&schemas, "AudioFrame.yaml", "metadata:\n  name: AudioFrame\n");
        // Non-yaml files must not be picked up.
        write_yaml(&schemas, "README.md", "readme\n");

        let res = resolve(&root).unwrap();
        assert_eq!(res.root.schema_files.len(), 2);
    }

    #[test]
    fn explicit_schema_list_overrides_auto_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");

        std::fs::create_dir_all(root.join("schemas")).unwrap();
        write_yaml(
            &root.join("schemas"),
            "Implicit.yaml",
            "metadata:\n  name: Implicit\n",
        );
        let custom = root.join("custom");
        std::fs::create_dir_all(&custom).unwrap();
        write_yaml(&custom, "Explicit.yaml", "metadata:\n  name: Explicit\n");

        write_streamlib_yaml(
            &root,
            r#"
package:
  org: tatolab
  name: core
  version: 1.0.0
schemas:
  - custom/Explicit.yaml
"#,
        );

        let res = resolve(&root).unwrap();
        assert_eq!(res.root.schema_files.len(), 1);
        assert!(res.root.schema_files[0].ends_with("Explicit.yaml"));
    }
}
