// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Static-file `.slpkg` registry tree emission — the daemon-free read side.
//!
//! An emit renders the workspace's `packages/*` into a plain on-disk tree — a
//! `.slpkg` generic store, a per-package + aggregate catalog, and a release
//! manifest — that is tokenless to read over `file://` (the existing
//! [`streamlib_idents::RegistryClient`] transport) and browsable as a plain
//! HTTP directory index. No registry daemon, no database, no token is required
//! to serve it.
//!
//! ## Atomicity
//!
//! A `file://` consumer must never observe a half-written tree. The tree is
//! built in a staging directory and moved into the served path only after the
//! [`ReleaseManifest`] lands — the whole release flips at once. See
//! [`publish_staged_tree`].

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use streamlib_idents::{
    CATALOG_INDEX_PATH, CatalogIndexLine, PackageRef, RegistryClient, RegistryConfig,
    ReleaseManifest, ReleaseManifestMember, SemVer, parse_catalog_index_ndjson,
    render_catalog_index_ndjson, schema_jtd_file_name,
};

use crate::catalog::{build_package_catalog, build_sibling_versions};

/// Options for [`emit_static_registry`].
#[derive(Debug, Clone)]
pub struct EmitOptions {
    pub workspace_root: PathBuf,
    /// Final served tree root. Built in a staging sibling and flipped in
    /// atomically once complete.
    pub out: PathBuf,
    /// `-dev.N` suffix for the emitted release version.
    pub dev: Option<u32>,
}

/// Atomically publish a fully-built `staging` tree into the served `served`
/// path so a concurrent `file://` reader observes either the OLD complete tree
/// or the NEW complete tree — never a partial one.
///
/// The caller builds `staging` completely (the whole `.slpkg` store + catalog +
/// the [`ReleaseManifest`] written LAST) before calling this. `staging` and
/// `served` must live on the same filesystem (same parent) so the flip is a
/// single rename.
///
/// - `served` absent → one atomic `rename(staging → served)` (gapless: the
///   tree appears in a single instant).
/// - `served` present → a single gapless swap. On Linux this is
///   `renameat2(RENAME_EXCHANGE)`, which atomically exchanges `staging` and
///   `served` in one syscall — there is no instant at which `served` is
///   absent or partial. The old tree ends up at `staging` and is removed
///   afterward. On other platforms (this tooling is Linux-only in practice)
///   it degrades to rename-aside, which has a sub-instant window where
///   `served` is momentarily absent (never partial — the tree is always
///   whole).
pub fn publish_staged_tree(staging: &Path, served: &Path) -> Result<()> {
    if !staging.is_dir() {
        anyhow::bail!("staging tree {} does not exist", staging.display());
    }
    if let Some(parent) = served.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    if !served.exists() {
        return std::fs::rename(staging, served)
            .with_context(|| format!("rename {} → {}", staging.display(), served.display()));
    }
    // `served` exists → gapless replace where the platform supports it.
    #[cfg(target_os = "linux")]
    {
        match exchange_paths(staging, served) {
            Ok(()) => {
                // After the exchange, `staging` holds the old tree.
                std::fs::remove_dir_all(staging).ok();
                return Ok(());
            }
            // Filesystem / kernel without RENAME_EXCHANGE support (e.g. some
            // network / FUSE mounts, pre-3.15 kernels) → degrade to the
            // rename-aside path below (sub-instant absence window, never a
            // partial tree).
            Err(e) if matches!(e.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOSYS)) => {}
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "atomically swap {} ⇄ {}",
                        staging.display(),
                        served.display()
                    )
                });
            }
        }
    }
    rename_aside_replace(staging, served)
}

/// Replace `served` with `staging` via rename-aside: old tree renamed away,
/// staging renamed in, old removed. Never exposes a partial tree; has a
/// sub-instant window where `served` is absent (the RENAME_EXCHANGE-less
/// fallback).
fn rename_aside_replace(staging: &Path, served: &Path) -> Result<()> {
    let backup = sibling_temp(served, "old");
    std::fs::rename(served, &backup)
        .with_context(|| format!("rename {} → {}", served.display(), backup.display()))?;
    match std::fs::rename(staging, served) {
        Ok(()) => {
            std::fs::remove_dir_all(&backup).ok();
            Ok(())
        }
        Err(e) => {
            std::fs::rename(&backup, served).ok();
            Err(e).with_context(|| format!("rename {} → {}", staging.display(), served.display()))
        }
    }
}

/// Atomically exchange two paths via `renameat2(RENAME_EXCHANGE)` (Linux).
/// Both paths must exist; on success `a` and `b` have traded inodes in a
/// single, uninterruptible operation.
#[cfg(target_os = "linux")]
fn exchange_paths(a: &Path, b: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let ca = std::ffi::CString::new(a.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let cb = std::ffi::CString::new(b.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    // SAFETY: both CStrings are valid NUL-terminated paths held for the call;
    // AT_FDCWD resolves the relative/absolute paths against the cwd.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            ca.as_ptr(),
            libc::AT_FDCWD,
            cb.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// A uniquely-named sibling path next to `path` (same parent → same
/// filesystem), tagged `tag`, for staging/backup dirs.
pub fn sibling_temp(path: &Path, tag: &str) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tree".to_string());
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    parent.join(format!(".{name}.{tag}.{nonce}"))
}

/// The registry org the tree is emitted under (`STREAMLIB_REGISTRY_ORG`, default `tatolab`).
fn registry_org() -> String {
    std::env::var("STREAMLIB_REGISTRY_ORG")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "tatolab".to_string())
}

fn workspace_version(workspace_root: &Path) -> Result<String> {
    let path = workspace_root.join("Cargo.toml");
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let doc: toml::Value =
        toml::from_str(&body).with_context(|| format!("parsing {}", path.display()))?;
    doc.get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .context("[workspace.package].version missing from workspace Cargo.toml")
}

fn target_version(base: &str, dev: Option<u32>) -> String {
    match dev {
        Some(n) => format!("{base}-dev.{n}"),
        None => base.to_string(),
    }
}

/// Emit a complete `.slpkg` registry tree (generic store + catalog + release
/// manifest) for the current workspace's `packages/*` into
/// [`EmitOptions::out`], via a staging dir that is flipped in atomically once
/// the [`ReleaseManifest`] lands (see [`publish_staged_tree`]). The whole tree
/// is browsable as a plain HTTP directory index or read over `file://`.
pub fn emit_static_registry(opts: &EmitOptions) -> Result<()> {
    let base = workspace_version(&opts.workspace_root)?;
    let target = target_version(&base, opts.dev);
    let org = registry_org();

    build_and_flip(&opts.out, |staging| {
        emit_slpkg_and_manifest(opts, staging, &target, &org)
    })?;
    tracing::info!(out = %opts.out.display(), release = %target, "static registry emitted");
    Ok(())
}

/// The staged-swap seam: run `build` against a fresh staging sibling of `out`,
/// then flip staging into `out` atomically ([`publish_staged_tree`]). On build
/// error the staging dir is removed and `out` is untouched — a crashed /
/// failed emit never perturbs the served tree.
pub fn build_and_flip(out: &Path, build: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let staging = sibling_temp(out, "staging");
    // Best-effort clean of any prior staging remnant, then a fresh dir.
    std::fs::remove_dir_all(&staging).ok();
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("create staging dir {}", staging.display()))?;
    match build(&staging) {
        Ok(()) => publish_staged_tree(&staging, out),
        Err(e) => {
            std::fs::remove_dir_all(&staging).ok();
            Err(e)
        }
    }
}

/// Whether a `packages/*` dir participates in the whole-tree `.slpkg` emit.
enum PackageEmitDecision {
    /// Publishable — assemble + upload + catalog + release-manifest membership.
    Emit,
    /// Non-distributable: carries a `streamlib.yaml` path-`patch:` block OR a
    /// Cargo.toml dependency-table `path` dep (the test-only fixtures).
    /// Skipped from the whole-tree emit, naming every offender.
    SkipNonDistributable(Vec<String>),
}

/// Classify a `packages/*` dir for the whole-tree emit. A package carrying a
/// `streamlib.yaml` path-`patch:` block OR a Cargo.toml dependency-table `path`
/// dep is non-distributable by construction — the exact set
/// `ensure_no_path_artifacts` rejects inside [`assemble_slpkg_bytes`] for the
/// `Slpkg` target — so the whole-tree emit skips it rather than hard-failing
/// the whole release. The predicate is shared with that gate via
/// [`crate::non_distributable_path_offenders`], so the skip set equals the
/// rejection set, sound by construction rather than a proxy.
fn decide_package_emit(pkg_dir: &Path) -> Result<PackageEmitDecision> {
    let offenders = crate::non_distributable_path_offenders(pkg_dir)?;
    if offenders.is_empty() {
        Ok(PackageEmitDecision::Emit)
    } else {
        Ok(PackageEmitDecision::SkipNonDistributable(offenders))
    }
}

/// Assemble each workspace package into a `.slpkg`, write it into the `file://`
/// generic store, and write the [`ReleaseManifest`] LAST (the atomicity flip
/// within the staging tree; the whole staging tree then flips atomically).
fn emit_slpkg_and_manifest(
    opts: &EmitOptions,
    staging: &Path,
    target: &str,
    org: &str,
) -> Result<()> {
    let slpkg_dir = staging.join("slpkg");
    std::fs::create_dir_all(&slpkg_dir)?;
    // The registry client is rooted at the tree root and writes under `slpkg/`.
    let config = RegistryConfig {
        base_url: format!("file://{}", staging.display()),
    };

    let packages_dir = opts.workspace_root.join("packages");
    let mut package_members: Vec<ReleaseManifestMember> = Vec::new();
    // Registry-wide processor index accumulated across every package — the
    // node-palette aggregate, written LAST (after the release manifest).
    let mut catalog_index: Vec<CatalogIndexLine> = Vec::new();
    if packages_dir.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&packages_dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort();

        // Resolution universe for external schema refs: every package being
        // published, with its version + manifest. Built up front so an
        // importer's catalog resolves a dep's version regardless of emit
        // order (alphabetical `entries` doesn't respect dependency order).
        let siblings = build_sibling_versions(&entries)
            .context("building the catalog resolution universe from packages/")?;

        let mut emitted = 0usize;
        let mut skipped = 0usize;
        for pkg_dir in &entries {
            let yaml = pkg_dir.join("streamlib.yaml");
            if !yaml.is_file() {
                continue;
            }
            // A package carrying a `streamlib.yaml` path-`patch:` block OR a
            // Cargo.toml dependency-table `path` dep is non-distributable by
            // construction (the same set `ensure_no_path_artifacts` rejects
            // inside `assemble_slpkg_bytes` for the `Slpkg` target). The
            // whole-tree emit skips it — with a warning naming every offender —
            // rather than hard-failing the whole release; the single-package
            // `streamlib pkg build/publish` still hard-fails so an author sees
            // the error. Skipping here means no upload, no release-manifest
            // membership, and no catalog entry.
            match decide_package_emit(pkg_dir)? {
                PackageEmitDecision::SkipNonDistributable(offenders) => {
                    tracing::warn!(
                        package = %pkg_dir.display(),
                        offenders = %offenders.join(", "),
                        "skipping non-distributable package (path patch or cargo path dep) from static-registry .slpkg emit"
                    );
                    skipped += 1;
                    continue;
                }
                PackageEmitDecision::Emit => {}
            }
            let (pkg_ref, version, bytes) = assemble_slpkg_bytes(pkg_dir)?;
            let semver: SemVer = version.parse().with_context(|| {
                format!("package {} version `{version}` is not semver", pkg_ref)
            })?;
            RegistryClient::new(&config)
                .upload_slpkg(&pkg_ref, semver, &bytes)
                .map_err(|e| anyhow::anyhow!("upload {}: {e}", pkg_ref))?;
            package_members.push(ReleaseManifestMember::new(
                pkg_ref.to_string(),
                version.clone(),
            ));

            // Publish-time catalog: per-package `<name>.catalog.json` + the
            // JTDs this package owns, written into the same version dir as the
            // `.slpkg`. Accumulate the per-processor index lines for the
            // registry-wide aggregate.
            let artifacts = build_package_catalog(pkg_dir, &siblings)
                .with_context(|| format!("building catalog for {pkg_ref}"))?;
            write_package_catalog(&slpkg_dir, &artifacts)
                .with_context(|| format!("writing catalog for {pkg_ref}"))?;
            catalog_index.extend(artifacts.index_lines);
            emitted += 1;
        }
        tracing::info!(emitted, skipped, "static-registry .slpkg emit complete");
    }

    // The release manifest lists exactly the `.slpkg` packages this emit
    // published. The SDK crate chain is NOT published by this emit — the custom
    // cargo registry was removed; SDK / library crates ship from public
    // registries as a separate, gated release step — so `crates` is empty.
    let mut manifest = ReleaseManifest::new(target.to_string(), Vec::new());
    manifest.packages = package_members;

    // Written LAST — the completion marker.
    RegistryClient::new(&config)
        .upload_release_manifest(org, &manifest)
        .map_err(|e| anyhow::anyhow!("upload release manifest: {e}"))?;

    // Registry-wide catalog aggregate, written AFTER the per-package catalog
    // files and the release manifest, at a STAGING-relative path so it rides
    // the same atomic flip (`build_and_flip`) — a `file://` reader never
    // observes the aggregate without the release it describes. Ordering it
    // last keeps the HTTP-direct-write intuition (index after members)
    // consistent. The CI static-registry emit smoke asserts the aggregate +
    // a per-package catalog exist in the emitted tree, locking this
    // staging-relativity through the real emit path.
    let index_path = staging.join(CATALOG_INDEX_PATH);
    if let Some(parent) = index_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&index_path, render_catalog_index_ndjson(&catalog_index))
        .with_context(|| format!("write {}", index_path.display()))?;
    Ok(())
}

/// Write one package's catalog artifacts under the tree's `slpkg/` root:
/// `<name>.catalog.json` beside the `.slpkg` in the FULL-version dir
/// (`slpkg/<name>/<version>/`, matching per-package catalog fetch by exact
/// published version), and each owned schema's JTD under the **release-core**
/// version dir (`slpkg/<name>/<release-core>/schemas/`) — schema idents are
/// release-core by invariant, so the reader derives the JTD path from the
/// ident's projected version. A `-dev.N` publisher whose JTDs sat under the
/// full prerelease dir would be silently unfetchable.
pub fn write_package_catalog(
    slpkg_dir: &Path,
    artifacts: &crate::catalog::PackageCatalogArtifacts,
) -> Result<()> {
    let name = artifacts.catalog.package.name.as_str();
    let version = artifacts.catalog.version.to_string();
    let ver_dir = slpkg_dir.join(name).join(&version);
    std::fs::create_dir_all(&ver_dir).with_context(|| format!("create {}", ver_dir.display()))?;

    let catalog_path = ver_dir.join(streamlib_idents::package_catalog_file_name(name));
    let catalog_json =
        serde_json::to_vec_pretty(&artifacts.catalog).context("serialize package catalog JSON")?;
    std::fs::write(&catalog_path, catalog_json)
        .with_context(|| format!("write {}", catalog_path.display()))?;

    if !artifacts.schema_jtd.is_empty() {
        // Release-core dir — the version basis of every SchemaIdent.
        let jtd_ver_dir = slpkg_dir
            .join(name)
            .join(artifacts.catalog.version.release_core().to_string());
        let schemas_dir = jtd_ver_dir.join("schemas");
        std::fs::create_dir_all(&schemas_dir)
            .with_context(|| format!("create {}", schemas_dir.display()))?;
        for jtd in &artifacts.schema_jtd {
            let path = schemas_dir.join(schema_jtd_file_name(&jtd.type_name));
            let bytes = serde_json::to_vec_pretty(&jtd.json)
                .with_context(|| format!("serialize JTD for {}", jtd.type_name))?;
            std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
        }
    }
    Ok(())
}

/// Merge one package's catalog index lines into the tree-wide aggregate
/// `catalog/index.ndjson`, so an incremental `streamlib pkg publish` keeps the
/// aggregate in step with the per-package catalog it just wrote — matching the
/// per-processor shape the whole-tree emit renders.
///
/// The whole-tree emit accumulates every package's lines and writes the
/// aggregate once ([`emit_slpkg_and_manifest`]); a single-package publish has
/// only its own lines, so it read-merge-writes instead: the existing aggregate
/// is read (absent ⇒ empty, self-healing like the per-package version index),
/// every line owned by this `(package, version)` is dropped, `new_lines` is
/// appended, and the file is rewritten. Dropping-then-appending makes a
/// republish of the same `(package, version)` replace its lines rather than
/// duplicate them, and drops the stale line of a processor removed on a
/// republish. `tree_root` is the registry tree root (the directory holding
/// `slpkg/` and `catalog/`).
pub fn merge_catalog_index_lines(
    tree_root: &Path,
    package: &PackageRef,
    version: &SemVer,
    new_lines: &[CatalogIndexLine],
) -> Result<()> {
    let index_path = tree_root.join(CATALOG_INDEX_PATH);
    let mut lines: Vec<CatalogIndexLine> = match std::fs::read(&index_path) {
        Ok(body) => parse_catalog_index_ndjson(&body),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            return Err(anyhow::Error::new(e))
                .with_context(|| format!("read {}", index_path.display()));
        }
    };
    lines.retain(|line| !(&line.package == package && &line.version == version));
    lines.extend(new_lines.iter().cloned());

    if let Some(parent) = index_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&index_path, render_catalog_index_ndjson(&lines))
        .with_context(|| format!("write {}", index_path.display()))?;
    Ok(())
}

/// Assemble a package dir into `.slpkg` bytes, returning its
/// [`PackageRef`], version, and the bytes.
fn assemble_slpkg_bytes(pkg_dir: &Path) -> Result<(PackageRef, String, Vec<u8>)> {
    let tmp = tempfile::Builder::new()
        .prefix("slpkg-emit-")
        .suffix(".slpkg")
        .tempfile()
        .context("create temp .slpkg")?;
    let outcome = crate::assemble_artifact(
        pkg_dir,
        &crate::AssembleTarget::Slpkg(tmp.path().to_path_buf()),
        &crate::AssembleOptions {
            no_build: false,
            profile: crate::CargoProfile::Release,
            path_deps: crate::PathDepPolicy::RejectPathPatches,
        },
        &(),
    )?;
    let bytes = std::fs::read(tmp.path()).context("read assembled .slpkg")?;
    // `AssembleOutcome.package_name` is the bare name; the org comes from the
    // manifest's `package:` block.
    #[derive(serde::Deserialize)]
    struct Pkg {
        org: String,
    }
    #[derive(serde::Deserialize)]
    struct Yaml {
        package: Pkg,
    }
    let yaml_body = std::fs::read_to_string(pkg_dir.join("streamlib.yaml"))
        .with_context(|| format!("read {}/streamlib.yaml", pkg_dir.display()))?;
    let parsed: Yaml = serde_yaml::from_str(&yaml_body)
        .with_context(|| format!("parse {}/streamlib.yaml", pkg_dir.display()))?;
    let pkg_ref = PackageRef::new(
        streamlib_idents::Org::new(&parsed.package.org).map_err(|e| anyhow::anyhow!("{e}"))?,
        streamlib_idents::Package::new(&outcome.package_name)
            .map_err(|e| anyhow::anyhow!("{e}"))?,
    );
    Ok((pkg_ref, outcome.package_version, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_staged_tree_fresh_target_is_atomic_rename() {
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join(".staging");
        let served = root.path().join("served");
        std::fs::create_dir_all(staging.join("slpkg")).unwrap();
        std::fs::write(staging.join("marker"), b"v2").unwrap();

        publish_staged_tree(&staging, &served).unwrap();
        assert!(!staging.exists(), "staging consumed by the rename");
        assert_eq!(
            std::fs::read_to_string(served.join("marker")).unwrap(),
            "v2"
        );
        assert!(served.join("slpkg").is_dir());
    }

    /// The set of *complete* release versions under a served tree's channel —
    /// what a `file://` [`ReleaseManifest`] consumer's `list_release_versions`
    /// observes. A release subdir counts only when its `manifest.json` (the
    /// completion marker, written LAST) is present.
    fn complete_releases(base: &Path) -> Vec<String> {
        let dir = base.join("slpkg/streamlib-release");
        let mut vs: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("manifest.json").is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        vs.sort();
        vs
    }

    /// The mid-publish window guarantee, exercised through the REAL seam
    /// (`build_and_flip`, the exact path `emit_static_registry` runs) with a
    /// CONCURRENT reader: while a new release is built, a reader of the
    /// served tree only ever observes a complete release set — the old
    /// `{0.5.0}` or the new `{0.5.1}` — never a partial or mixed one.
    ///
    /// Reader protocol (the file:// consumer shape): list the release
    /// channel, then read each listed version's manifest. A manifest read
    /// that fails is tolerated ONLY when a re-list shows the version gone
    /// (the atomic flip raced between the two syscalls); a version that
    /// stays listed without its manifest is a partial tree → panic.
    /// Mentally revert `build_and_flip`/`publish_staged_tree` to a
    /// copy-into-served loop: the half-copied `0.5.1` dir is listed without
    /// its manifest (manifest is written last) and stays listed → panic.
    #[test]
    #[cfg(target_os = "linux")] // relies on the gapless RENAME_EXCHANGE flip
    fn window_concurrent_reader_never_observes_partial_release() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let root = tempfile::tempdir().unwrap();
        let out = root.path().join("served");

        let write_release = |staging: &Path, ver: &str, slow: bool| -> Result<()> {
            std::fs::create_dir_all(staging.join("slpkg/payloads"))?;
            for i in 0..30 {
                std::fs::write(
                    staging.join(format!("slpkg/payloads/payload-{i}.bin")),
                    vec![0u8; 1024],
                )?;
                if slow {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            // Release manifest written LAST — the completion marker.
            let rel = staging.join("slpkg/streamlib-release").join(ver);
            std::fs::create_dir_all(&rel)?;
            std::fs::write(
                rel.join("manifest.json"),
                format!("{{\"release_version\":\"{ver}\"}}"),
            )?;
            Ok(())
        };

        // Old complete release, published through the real seam.
        build_and_flip(&out, |staging| write_release(staging, "0.5.0", false)).unwrap();

        let list = |base: &Path| -> Option<Vec<String>> {
            let rd = std::fs::read_dir(base.join("slpkg/streamlib-release")).ok()?;
            let mut vs: Vec<String> = rd
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            vs.sort();
            Some(vs)
        };

        let stop = Arc::new(AtomicBool::new(false));
        let out_r = out.clone();
        let stop_r = stop.clone();
        let reader = std::thread::spawn(move || {
            let mut saw_old = false;
            let mut saw_new = false;
            while !stop_r.load(Ordering::Relaxed) {
                // With RENAME_EXCHANGE the served path never vanishes.
                let listed = list(&out_r).expect("release channel must never vanish");
                match listed.iter().map(String::as_str).collect::<Vec<_>>()[..] {
                    ["0.5.0"] | ["0.5.1"] => {}
                    ref other => panic!("non-atomic release set observed: {other:?}"),
                }
                for v in &listed {
                    let manifest = out_r
                        .join("slpkg/streamlib-release")
                        .join(v)
                        .join("manifest.json");
                    match std::fs::read_to_string(&manifest) {
                        Ok(body) => {
                            // Never a torn/partial file: staged writes are
                            // invisible until the flip.
                            let parsed: serde_json::Value =
                                serde_json::from_str(&body).expect("manifest must be whole JSON");
                            assert_eq!(parsed["release_version"], v.as_str());
                            if v == "0.5.0" {
                                saw_old = true;
                            } else {
                                saw_new = true;
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            // Tolerable ONLY if the flip raced between list
                            // and read — the version must now be gone.
                            let relisted = list(&out_r).expect("channel must never vanish");
                            assert!(
                                !relisted.contains(v),
                                "version {v} listed without its manifest and still \
                                 listed on re-list — PARTIAL tree exposed"
                            );
                        }
                        Err(e) => panic!("manifest read failed: {e}"),
                    }
                }
            }
            (saw_old, saw_new)
        });

        // New release built + flipped through the real seam, slowly, while
        // the reader spins.
        build_and_flip(&out, |staging| write_release(staging, "0.5.1", true)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        stop.store(true, Ordering::Relaxed);
        let (saw_old, saw_new) = reader.join().unwrap();

        assert!(
            saw_old,
            "reader must have observed the old release during staging"
        );
        assert!(
            saw_new,
            "reader must have observed the flipped-in new release"
        );
        assert_eq!(complete_releases(&out), vec!["0.5.1".to_string()]);
    }

    /// A build that CRASHES (errors) mid-emit must leave the served tree
    /// untouched and clean up its staging dir — the failed-publish half of
    /// the window guarantee.
    #[test]
    fn crashed_build_leaves_served_tree_untouched() {
        let root = tempfile::tempdir().unwrap();
        let out = root.path().join("served");
        std::fs::create_dir_all(out.join("slpkg/streamlib-release/0.5.0")).unwrap();
        std::fs::write(
            out.join("slpkg/streamlib-release/0.5.0/manifest.json"),
            b"{\"release_version\":\"0.5.0\"}",
        )
        .unwrap();

        let err = build_and_flip(&out, |staging| {
            // Write half a release, then die before the manifest.
            std::fs::create_dir_all(staging.join("slpkg/streamlib-release/0.5.1"))?;
            std::fs::write(
                staging.join("slpkg/streamlib-release/0.5.1/partial.bin"),
                b"x",
            )?;
            anyhow::bail!("simulated mid-emit crash")
        })
        .unwrap_err();
        assert!(err.to_string().contains("simulated mid-emit crash"));

        // Served tree: exactly the old complete release, nothing else.
        assert_eq!(complete_releases(&out), vec!["0.5.0".to_string()]);
        assert!(!out.join("slpkg/streamlib-release/0.5.1").exists());
        // No staging remnant left beside the served tree.
        let remnants: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".staging."))
            .collect();
        assert!(
            remnants.is_empty(),
            "staging remnant left behind: {remnants:?}"
        );
    }

    /// Locks the gapless replace primitive: `publish_staged_tree` onto an
    /// EXISTING served tree swaps in the new one and drops the old, with no
    /// intermediate rename-aside gap (Linux `renameat2(RENAME_EXCHANGE)`).
    #[test]
    fn publish_staged_tree_replaces_existing_atomically() {
        let root = tempfile::tempdir().unwrap();
        let served = root.path().join("served");
        std::fs::create_dir_all(&served).unwrap();
        std::fs::write(served.join("MARKER"), b"old").unwrap();
        std::fs::write(served.join("only-in-old"), b"1").unwrap();

        let staging = sibling_temp(&served, "staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("MARKER"), b"new").unwrap();
        std::fs::write(staging.join("only-in-new"), b"1").unwrap();

        publish_staged_tree(&staging, &served).unwrap();

        assert_eq!(
            std::fs::read_to_string(served.join("MARKER")).unwrap(),
            "new"
        );
        assert!(served.join("only-in-new").is_file());
        assert!(
            !served.join("only-in-old").exists(),
            "old tree fully replaced"
        );
        assert!(!staging.exists(), "staging (old tree after swap) removed");
    }

    /// Write a `streamlib.yaml` into a fresh temp package dir and return it.
    fn package_dir_with_manifest(manifest_yaml: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("streamlib.yaml"), manifest_yaml).unwrap();
        dir
    }

    /// A publishable manifest (no `patch:` block) classifies as `Emit`.
    #[test]
    fn decide_package_emit_clean_manifest_emits() {
        let dir = package_dir_with_manifest(
            "package:\n  org: tatolab\n  name: clean-pkg\n  version: 1.0.0\n\
             dependencies:\n  \"@tatolab/core\": \"^1.0.0\"\n",
        );
        assert!(matches!(
            decide_package_emit(dir.path()).unwrap(),
            PackageEmitDecision::Emit
        ));
    }

    /// A manifest carrying a dev path-`patch:` block (the test-fixtures shape)
    /// classifies as `SkipNonDistributable`, naming the offending dependency.
    ///
    /// Mental revert: change `decide_package_emit`'s non-empty arm to
    /// `PackageEmitDecision::Emit` and this test fails — the classification is
    /// what the whole-tree emit's skip branch keys on, so the test locks the
    /// decision, not just the parse.
    #[test]
    fn decide_package_emit_path_patch_skips_and_names_offender() {
        let dir = package_dir_with_manifest(
            "package:\n  org: tatolab\n  name: fixture-pkg\n  version: 1.0.0\n\
             dependencies:\n  \"@tatolab/core\": \"^1.0.0\"\n\
             patch:\n  \"@tatolab/core\":\n    path: ../core\n",
        );
        match decide_package_emit(dir.path()).unwrap() {
            PackageEmitDecision::SkipNonDistributable(offenders) => {
                assert_eq!(offenders.len(), 1);
                assert!(
                    offenders[0].contains("@tatolab/core") && offenders[0].contains("../core"),
                    "offender should name the dep and the path: {offenders:?}"
                );
            }
            PackageEmitDecision::Emit => panic!("path-patch-carrying package must be skipped"),
        }
    }

    /// A clean `streamlib.yaml` (no path `patch:`) paired with a Cargo.toml
    /// carrying a dependency-table `path` dep classifies as
    /// `SkipNonDistributable`. The skip predicate must detect a Cargo path dep
    /// too — `ensure_no_path_artifacts` would otherwise hard-fail the whole
    /// emit on it — so the skip set must equal the rejection set.
    ///
    /// Mental revert: drop the `cargo_path_dep_offenders` half of
    /// `non_distributable_path_offenders` and this package classifies `Emit`
    /// (its streamlib.yaml carries no patch), reintroducing the "one
    /// non-publishable package fails the whole release" mode via the other
    /// gate.
    #[test]
    fn decide_package_emit_cargo_path_dep_skips_and_names_offender() {
        let dir = package_dir_with_manifest(
            "package:\n  org: tatolab\n  name: cargo-path-pkg\n  version: 1.0.0\n\
             dependencies:\n  \"@tatolab/core\": \"^1.0.0\"\n",
        );
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"cargo-path-pkg\"\nversion = \"1.0.0\"\n\
             [dependencies]\nsibling = { path = \"../sibling\", version = \"1.0\" }\n",
        )
        .unwrap();
        match decide_package_emit(dir.path()).unwrap() {
            PackageEmitDecision::SkipNonDistributable(offenders) => {
                assert!(
                    offenders.iter().any(|o| o.contains("sibling")),
                    "offender should name the Cargo path dep: {offenders:?}"
                );
            }
            PackageEmitDecision::Emit => {
                panic!("a Cargo.toml dependency-table path dep must be skipped")
            }
        }
    }

    /// A Cargo.toml TARGET path (`[[bin]].path` / `[lib].path`) is NOT a
    /// dependency path — a package whose only Cargo path keys are target
    /// paths (and whose streamlib.yaml carries no patch) still classifies
    /// `Emit`. Guards against an over-broad scan that would skip a publishable
    /// package for declaring a non-default `src/` layout.
    #[test]
    fn decide_package_emit_cargo_target_path_still_emits() {
        let dir = package_dir_with_manifest(
            "package:\n  org: tatolab\n  name: target-path-pkg\n  version: 1.0.0\n\
             dependencies:\n  \"@tatolab/core\": \"^1.0.0\"\n",
        );
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"target-path-pkg\"\nversion = \"1.0.0\"\n\
             [lib]\npath = \"src/lib.rs\"\n\
             [[bin]]\nname = \"tool\"\npath = \"src/bin/tool.rs\"\n\
             [dependencies]\nserde = { version = \"1\" }\n",
        )
        .unwrap();
        assert!(
            matches!(
                decide_package_emit(dir.path()).unwrap(),
                PackageEmitDecision::Emit
            ),
            "Cargo target paths ([[bin]].path / [lib].path) are not dependency \
             paths and must not trigger skip"
        );
    }

    /// A non-path (git) `patch:` override is a legitimate distributable
    /// override, not a dev path affordance — the skip is path-only, so the
    /// package still classifies as `Emit`.
    #[test]
    fn decide_package_emit_git_patch_still_emits() {
        let dir = package_dir_with_manifest(
            "package:\n  org: tatolab\n  name: git-pkg\n  version: 1.0.0\n\
             dependencies:\n  \"@tatolab/bar\": \"^2.0.0\"\n\
             patch:\n  \"@tatolab/bar\":\n    git: https://example.com/bar\n    rev: abc123\n",
        );
        assert!(
            matches!(
                decide_package_emit(dir.path()).unwrap(),
                PackageEmitDecision::Emit
            ),
            "a git-flavor patch is not a dev path override and must not be skipped"
        );
    }
}
