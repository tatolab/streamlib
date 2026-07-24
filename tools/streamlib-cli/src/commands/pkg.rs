// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Package management commands.
//!
//! Single-package adoption (installing a published package, removing one) lives
//! in the top-level `streamlib add` / `streamlib remove` verbs
//! ([`super::add`]); `pkg` here is scoped to authoring artifacts of THIS
//! package — build, publish, clean, inspect — plus `list`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use streamlib::engine_internal::core::ProjectConfig;
use streamlib::sdk::runtime::{AppModulesDir, parse_lockfile_package_key};
use streamlib_idents::{PackageSourceClient, PackageSource};
use streamlib_pack::catalog::{build_package_catalog, build_sibling_versions};
use streamlib_pack::static_package_source::{merge_catalog_index_lines, write_package_catalog};
use streamlib_pack::{
    AssembleOptions, AssembleTarget, CargoProfile, PathDepPolicy, assemble_artifact,
};

/// Build THIS package (the current working directory) into a source-only
/// `.slpkg`. Pure source bundling — no compilation, no prebuilt cdylib,
/// nothing path-related (the assembler refuses a path dep / path patch). The
/// artifact is a hand-off bundle; the consumer builds it from source.
pub fn build(output: Option<&Path>) -> Result<()> {
    let package_dir = std::env::current_dir().context("resolve current working directory")?;
    // Early friendly check; the load-bearing guard runs again inside
    // `assemble_artifact`'s Slpkg branch (streamlib-pack owns the seam).
    streamlib_idents::link_marker::ensure_no_active_link_for_pack(&package_dir)?;
    let output_path = resolve_slpkg_output(&package_dir, output)?;
    let outcome = assemble_source_slpkg(&package_dir, &output_path)?;
    println!("Built source-only package: {}", output_path.display());
    println!("  {} v{}", outcome.package_name, outcome.package_version);
    if outcome.schemas > 0 {
        println!("  Schemas: {}", outcome.schemas);
    }
    if outcome.processors > 0 {
        println!("  Processors: {}", outcome.processors);
    }
    Ok(())
}

/// Publish THIS package (the current working directory) into the package
/// source (a static `.slpkg` tree) generic store. Always repacks a fresh
/// source-only `.slpkg` to a temp file (never trusts a pre-existing artifact),
/// writes it by version, refreshes the package's version index, and emits the
/// same catalog artifacts a whole-tree `static-package-source emit` would — the
/// per-package `<name>.catalog.json` + owned schema JTDs beside the `.slpkg`,
/// plus a merge into the tree-wide `catalog/index.ndjson` — so a package source
/// populated purely by `pkg publish` serves a catalog-backed discovery summary,
/// not "no metadata". The package source tree root comes from
/// `STREAMLIB_PACKAGE_SOURCE` and must be a `file://` tree — publishing writes
/// files; a static HTTP mount is read-only.
pub fn publish() -> Result<()> {
    let package_dir = std::env::current_dir().context("resolve current working directory")?;
    // Early friendly check; the load-bearing guard runs again inside
    // `assemble_artifact`'s Slpkg branch (streamlib-pack owns the seam).
    streamlib_idents::link_marker::ensure_no_active_link_for_pack(&package_dir)?;
    // Lightweight manifest read — package metadata only, NO dependency
    // resolution (which would require the package source just to read
    // name/version).
    let config = streamlib_cargo_build::read_minimal_project_config(&package_dir)
        .context("Failed to read streamlib.yaml")?
        .ok_or_else(|| anyhow::anyhow!("no streamlib.yaml at {}", package_dir.display()))?;
    let package = config
        .package
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("streamlib.yaml missing [package] section"))?;

    let package_source = PackageSource::from_env().ok_or_else(|| {
        anyhow::anyhow!(
            "no package source configured: set STREAMLIB_PACKAGE_SOURCE to a file:// package \
             source tree (e.g. file:///path/to/slpkg-tree) to publish"
        )
    })?;

    // Assemble the publish-time catalog up front, so an unresolvable external
    // schema ref fails BEFORE any bytes land in the tree. External refs resolve
    // against the sibling packages discoverable next to this one — mirroring the
    // whole-tree emit's `packages/` enumeration; a genuinely unresolvable ref
    // surfaces a typed `CatalogError` (e.g. `ExternalDepMissing`) here.
    let siblings = build_sibling_versions(&sibling_package_dirs(&package_dir))
        .map_err(|e| anyhow::anyhow!("assembling the catalog resolution universe: {}", e))?;
    let catalog_artifacts = build_package_catalog(&package_dir, &siblings)
        .map_err(|e| anyhow::anyhow!("building the package catalog: {}", e))?;

    // Always repack fresh into a temp file — publish never trusts a
    // pre-existing artifact (pack runs independently, at any time).
    let tmp = tempfile::Builder::new()
        .prefix("streamlib-publish-")
        .suffix(".slpkg")
        .tempfile()
        .context("create temp .slpkg")?;
    let outcome = assemble_source_slpkg(&package_dir, tmp.path())?;
    let bytes = std::fs::read(tmp.path()).context("read packed .slpkg")?;

    let pkg_ref = streamlib_idents::PackageRef::new(package.org.clone(), package.name.clone());
    let client = PackageSourceClient::new(&package_source);
    println!(
        "Publishing {} v{} ({} bytes) to {}…",
        outcome.package_name,
        outcome.package_version,
        bytes.len(),
        package_source.base_url
    );
    let url = client
        .upload_slpkg(&pkg_ref, package.version, &bytes)
        .map_err(|e| anyhow::anyhow!("upload failed: {}", e))?;
    println!("Published → {url}");

    // Publish the catalog alongside the `.slpkg`. `upload_slpkg` already proved
    // the tree is a writable `file://` root, so deriving the on-disk root here is
    // sound.
    let tree_root = file_tree_root(&package_source.base_url)?;
    let slpkg_dir = tree_root.join("slpkg");
    write_package_catalog(&slpkg_dir, &catalog_artifacts)
        .map_err(|e| anyhow::anyhow!("writing the package catalog: {}", e))?;
    merge_catalog_index_lines(
        &tree_root,
        &pkg_ref,
        &package.version,
        &catalog_artifacts.index_lines,
    )
    .map_err(|e| anyhow::anyhow!("updating the catalog index: {}", e))?;
    println!(
        "  Catalog: {} processor(s), {} owned schema(s)",
        catalog_artifacts.index_lines.len(),
        catalog_artifacts.schema_jtd.len()
    );
    Ok(())
}

/// The sibling package directories catalog assembly resolves external schema
/// references against — the entries of the directory that holds the package
/// being published, mirroring the whole-tree emit's enumeration of `packages/`.
/// [`build_sibling_versions`] skips any entry without a `[package]` block, so
/// non-package siblings are harmless. Falls back to just the package itself when
/// it has no parent or the parent can't be read (a self-contained package with
/// no external refs still resolves; a package that imports an external schema
/// then surfaces a typed `ExternalDepMissing`).
fn sibling_package_dirs(package_dir: &Path) -> Vec<std::path::PathBuf> {
    let read_siblings = package_dir.parent().and_then(|parent| {
        std::fs::read_dir(parent).ok().map(|entries| {
            let mut dirs: Vec<std::path::PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_dir())
                .collect();
            // Sort so the resolution universe is deterministic — matching the
            // whole-tree emit, which sorts its `packages/` entries before
            // building siblings (`build_sibling_versions` is last-write-wins on
            // a duplicate `@org/name`, degenerate but order-sensitive).
            dirs.sort();
            dirs
        })
    });
    match read_siblings {
        Some(dirs) if !dirs.is_empty() => dirs,
        _ => vec![package_dir.to_path_buf()],
    }
}

/// The on-disk tree root a `file://` package source base URL points at. `pkg
/// publish` only reaches this after [`PackageSourceClient::upload_slpkg`] has
/// already required the `file://` scheme, so a non-`file://` base here is an
/// internal invariant violation rather than a user-facing case.
fn file_tree_root(base_url: &str) -> Result<std::path::PathBuf> {
    base_url
        .strip_prefix("file://")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "internal: publishing the catalog requires a file:// package source tree, got `{base_url}`"
            )
        })
}

/// Remove THIS package's build/pack artifacts from the current working
/// directory: any `*.slpkg`, the prebuilt `lib/` dir, and the generated
/// `_generated_/` wire-vocabulary trees (root + `python/`). All are
/// regenerated on the next build/pack.
pub fn clean() -> Result<()> {
    let dir = std::env::current_dir().context("resolve current working directory")?;
    let mut removed: Vec<String> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("slpkg") {
                if std::fs::remove_file(&p).is_ok() {
                    removed.push(
                        p.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
    }
    let lib = dir.join("lib");
    if lib.is_dir() && std::fs::remove_dir_all(&lib).is_ok() {
        removed.push("lib/".to_string());
    }
    for cand in [
        dir.join("_generated_"),
        dir.join("python").join("_generated_"),
    ] {
        if cand.is_dir() && std::fs::remove_dir_all(&cand).is_ok() {
            let rel = cand.strip_prefix(&dir).unwrap_or(&cand);
            removed.push(format!("{}/", rel.display()));
        }
    }

    if removed.is_empty() {
        println!("Nothing to clean.");
    } else {
        println!("Removed: {}", removed.join(", "));
    }
    Ok(())
}

/// Reclaim regenerable build scratch across every materialized package slot,
/// keeping the loadable artifact (`lib/<triple>/*.so` + `.venv/` +
/// `_generated_/` + manifest). Reclaims each slot's `cargo` `target/` plus any
/// orphaned `.tmp-*` / `.old-*` staging residue across the app's co-located
/// `streamlib_modules/@org/name/` slots — the only place materialized slots
/// live post-#1506.
///
/// Distinct from [`clean`], which cleans the CURRENT package's own source dir:
/// `cache-gc` is a whole-cache reclaim of on-the-box build output, the disk
/// counterpart to compile-on-install.
pub fn cache_gc(dir: Option<&Path>) -> Result<()> {
    let mut reclaimed: Vec<PathBuf> = Vec::new();

    // Co-located app modules: <app_root>/streamlib_modules/@org/name/.
    let app_root = match dir {
        Some(root) => root.to_path_buf(),
        None => std::env::current_dir().context("resolve current working directory")?,
    };
    let modules = app_root.join(streamlib_idents::app_modules::APP_MODULES_DIR_NAME);
    reclaimed.extend(reclaim_package_target_dirs(&modules));
    reclaimed.extend(sweep_staging_residue(&modules));

    if reclaimed.is_empty() {
        println!("Nothing to reclaim.");
    } else {
        println!("Reclaimed {} build-scratch dir(s):", reclaimed.len());
        for path in &reclaimed {
            println!("  {}", path.display());
        }
    }
    Ok(())
}

/// Walk `root` and, for every package slot found (a directory carrying a
/// `streamlib.yaml`), remove its `target/` build scratch. Never recurses into
/// a package slot, so a nested `target/` inside package source is untouched.
/// Returns the reclaimed `target/` paths.
fn reclaim_package_target_dirs(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, reclaimed: &mut Vec<PathBuf>) {
        if dir.join("streamlib.yaml").is_file() {
            let target = dir.join("target");
            if target.is_dir() && std::fs::remove_dir_all(&target).is_ok() {
                reclaimed.push(target);
            }
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                walk(&entry.path(), reclaimed);
            }
        }
    }
    let mut reclaimed = Vec::new();
    if root.is_dir() {
        walk(root, &mut reclaimed);
    }
    reclaimed
}

/// A staging-residue dir younger than this is treated as possibly in-flight and
/// left alone even when its embedded pid can't be confirmed live (pid reuse, a
/// non-Linux host, or an unparseable name) — the mtime-age safety net beneath
/// the pid-liveness check, so `cache-gc` is safe to run during a concurrent
/// build. Generous because a long cargo compile runs in the package source dir,
/// not the `.tmp-*` stage dir, so the stage dir's mtime can lag the build.
const STAGING_RESIDUE_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Remove orphaned `.tmp-*` (interrupted build stage) and `.old-*` (interrupted
/// atomic-swap displacement) residue directly under `parent`. A residue dir
/// owned by a LIVE build — its name embeds the builder's pid (`.tmp-<name>-<pid>-<seq>`)
/// and the process is still alive, or the dir is younger than
/// [`STAGING_RESIDUE_MIN_AGE`] — is skipped, so the verb never races a running
/// build into a corrupt slot. Returns the removed paths.
fn sweep_staging_residue(parent: &Path) -> Vec<PathBuf> {
    sweep_staging_residue_filtered(parent, staging_residue_is_in_flight)
}

/// [`sweep_staging_residue`] with an injectable in-flight predicate (test seam):
/// production passes [`staging_residue_is_in_flight`]; tests pass a stub to
/// exercise the pure sweep mechanics without depending on real pids / mtimes.
fn sweep_staging_residue_filtered(
    parent: &Path,
    is_in_flight: impl Fn(&str, &Path) -> bool,
) -> Vec<PathBuf> {
    let mut swept = Vec::new();
    let Ok(entries) = std::fs::read_dir(parent) else {
        return swept;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with(".tmp-") || name.starts_with(".old-")) {
            continue;
        }
        let path = entry.path();
        if is_in_flight(&name, &path) {
            continue;
        }
        if std::fs::remove_dir_all(&path).is_ok() {
            swept.push(path);
        }
    }
    swept
}

/// Whether a `.tmp-*` / `.old-*` residue dir may belong to a still-running build
/// and so must NOT be swept: its embedded pid is a live process, or the dir is
/// younger than [`STAGING_RESIDUE_MIN_AGE`].
fn staging_residue_is_in_flight(name: &str, path: &Path) -> bool {
    if parse_embedded_pid(name).is_some_and(pid_is_alive) {
        return true;
    }
    let Ok(elapsed) = path
        .metadata()
        .and_then(|meta| meta.modified())
        .and_then(|modified| modified.elapsed().map_err(std::io::Error::other))
    else {
        // Unreadable / future mtime: err toward keeping it.
        return true;
    };
    elapsed < STAGING_RESIDUE_MIN_AGE
}

/// Extract the builder pid from a `.tmp-<name>-<pid>-<seq>` /
/// `.old-<name>-<pid>-<seq>` residue name. The package name+version embedded in
/// `<name>` may itself carry dashes, so the pid is the second-to-last
/// dash-delimited segment and the seq the last — both numeric.
fn parse_embedded_pid(name: &str) -> Option<u32> {
    let mut segments = name.rsplit('-');
    let _seq: u64 = segments.next()?.parse().ok()?;
    segments.next()?.parse().ok()
}

/// Whether `pid` is a currently-live process. Linux-only via `/proc/<pid>`; on
/// other hosts liveness can't be probed dep-free, so it returns `false` and the
/// mtime-age net in [`staging_residue_is_in_flight`] is the sole guard there.
fn pid_is_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
    }
}

/// Resolve the default `.slpkg` output path (`{name}-{version}.slpkg` in the
/// package dir) when `--output` isn't given.
fn resolve_slpkg_output(package_dir: &Path, output: Option<&Path>) -> Result<std::path::PathBuf> {
    match output {
        Some(p) => Ok(p.to_path_buf()),
        None => {
            let config = streamlib_cargo_build::read_minimal_project_config(package_dir)
                .context("Failed to read streamlib.yaml")?
                .ok_or_else(|| anyhow::anyhow!("no streamlib.yaml at {}", package_dir.display()))?;
            let package = config
                .package
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("streamlib.yaml missing [package] section"))?;
            Ok(package_dir.join(format!(
                "{}-{}.slpkg",
                package.name.as_str(),
                package.version
            )))
        }
    }
}

/// Assemble a source-only `.slpkg` at `output_path`. The `Slpkg` target makes
/// `assemble_artifact` ship source only (no cdylib build) and enforce the
/// no-path contract; `no_build` / `profile` are inert on this path.
fn assemble_source_slpkg(
    package_dir: &Path,
    output_path: &Path,
) -> Result<streamlib_pack::AssembleOutcome> {
    assemble_artifact(
        package_dir,
        &AssembleTarget::Slpkg(output_path.to_path_buf()),
        &AssembleOptions {
            no_build: false,
            profile: CargoProfile::Release,
            path_deps: PathDepPolicy::RejectPathPatches,
            ignore_in_tree_prebuilt_cdylib: false,
        },
        &(),
    )
    .map_err(|e| anyhow::anyhow!("pack failed: {}", e))
}

/// Inspect a .slpkg package without installing it.
pub fn inspect(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("File not found: {}", path.display());
    }

    let file =
        std::fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("Failed to read ZIP archive: {}", path.display()))?;

    // Find and read streamlib.yaml from the archive
    let yaml_content = {
        let mut entry = archive
            .by_name("streamlib.yaml")
            .with_context(|| "Package missing streamlib.yaml")?;
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut entry, &mut buf)?;
        buf
    };

    let config: ProjectConfig =
        serde_yaml::from_str(&yaml_content).with_context(|| "Failed to parse streamlib.yaml")?;

    let package = config
        .package
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Package missing [package] section"))?;

    println!("Package: {} v{}", package.name, package.version);
    if let Some(desc) = &package.description {
        println!("Description: {}", desc);
    }
    if let Some(sv) = &package.streamlib_version {
        println!("Requires: streamlib {}", sv);
    }

    if !config.processors.is_empty() {
        println!();
        println!("Processors ({}):", config.processors.len());
        for proc in &config.processors {
            println!("  {}", proc.name);
            if let Some(desc) = &proc.description {
                println!("    {}", desc);
            }
            println!("    Runtime:   {:?}", proc.runtime.language);
            println!("    Execution: {:?}", proc.execution);
            if !proc.inputs.is_empty() {
                println!("    Inputs:");
                for input in &proc.inputs {
                    println!("      - {} ({})", input.name, input.schema);
                }
            }
            if !proc.outputs.is_empty() {
                println!("    Outputs:");
                for output in &proc.outputs {
                    println!("      - {} ({})", output.name, output.schema);
                }
            }
            if let Some(config_ref) = &proc.config {
                println!("    Config:    {} ({})", config_ref.name, config_ref.schema);
            }
        }
    }

    // List files in archive
    println!();
    println!("Files ({}):", archive.len());
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            println!("  {}", entry.name());
        }
    }

    Ok(())
}

/// List the app's installed packages, read from its `streamlib.lock` — the
/// record `streamlib add`/`link`/`install` maintain beside the app's
/// `streamlib_modules/` folder. `dir` is the app root (default: CWD).
pub fn list(dir: Option<&Path>) -> Result<()> {
    let app = match dir {
        Some(root) => AppModulesDir::at(root),
        None => AppModulesDir::from_cwd().map_err(|e| anyhow::anyhow!("{e}"))?,
    };
    let lockfile = app
        .read_lockfile()
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", app.lockfile_path().display(), e))?;

    if lockfile.packages.is_empty() {
        println!("No packages installed in {}.", app.modules_dir().display());
        println!();
        println!("Add a package with:");
        println!("  streamlib add @org/name@^1.0.0     # by version from the package source");
        println!("  streamlib add ./path/to.slpkg      # from a local artifact");
        println!("  streamlib add https://…/pkg.slpkg  # from a URL");
        return Ok(());
    }

    println!("Installed packages ({}):\n", lockfile.packages.len());

    for (pkg_ref, entry) in &lockfile.packages {
        let present = parse_lockfile_package_key(pkg_ref)
            .map(|package| app.package_dir(&package).is_dir())
            .unwrap_or(false);
        println!("  {} v{}", pkg_ref, entry.version);
        println!("    Source: {}", describe_lock_source(&entry.source));
        if !present {
            println!("    (slot missing on disk — run `streamlib install` to reproduce)");
        }
        println!();
    }

    Ok(())
}

/// One-line human description of a lockfile source for `pkg list`.
fn describe_lock_source(source: &streamlib_idents::LockfileSource) -> String {
    use streamlib_idents::LockfileSource;
    match source {
        LockfileSource::Path { path } => format!("path:{}", path.display()),
        LockfileSource::Archive { path, .. } => format!("archive:{}", path.display()),
        LockfileSource::Url { url, .. } => format!("url:{url}"),
        LockfileSource::Link { path } => format!("link:{}", path.display()),
        LockfileSource::ByVersion { url } => format!("by-version:{url}"),
        LockfileSource::Git { url, rev } => format!("git:{url}@{rev}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a package slot at `dir` (a `streamlib.yaml`), give it a loadable
    /// `lib/<triple>/*.so` artifact, and a regenerable `target/` scratch tree.
    fn write_slot_with_scratch(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("streamlib.yaml"), b"package:\n  name: x\n").unwrap();
        let lib = dir.join("lib").join("host-triple");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("libx.so"), b"cdylib").unwrap();
        let target = dir.join("target").join("debug");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("junk.o"), b"scratch").unwrap();
    }

    #[test]
    fn reclaim_drops_target_across_nested_slot_shapes_keeps_artifacts() {
        // The walker reclaims `target/` for a package slot wherever it nests —
        // both a flat `<name>/` and the co-located `streamlib_modules/@org/name/`
        // shape — and its `lib/` artifact survives. Mentally-revert the `return`
        // after reclaiming a slot and the walk would descend INTO package source
        // looking for more `target/` dirs — this asserts the artifact (which is
        // NOT scratch) is never touched.
        let root = tempfile::tempdir().unwrap();

        let flat = root.path().join("some-slot");
        write_slot_with_scratch(&flat);

        let modules = root.path().join("streamlib_modules").join("@org").join("bar");
        write_slot_with_scratch(&modules);

        let reclaimed = reclaim_package_target_dirs(root.path());

        assert_eq!(reclaimed.len(), 2, "both slots' target/ reclaimed: {reclaimed:?}");
        for slot in [&flat, &modules] {
            assert!(!slot.join("target").exists(), "target/ must be reclaimed");
            assert!(
                slot.join("lib").join("host-triple").join("libx.so").is_file(),
                "cdylib artifact must survive the reclaim"
            );
            assert!(slot.join("streamlib.yaml").is_file(), "manifest must survive");
        }
    }

    #[test]
    fn reclaim_ignores_a_slot_without_target() {
        let root = tempfile::tempdir().unwrap();
        let slot = root.path().join("pkg");
        std::fs::create_dir_all(&slot).unwrap();
        std::fs::write(slot.join("streamlib.yaml"), b"package:\n").unwrap();
        assert!(reclaim_package_target_dirs(root.path()).is_empty());
    }

    #[test]
    fn sweep_removes_only_staging_residue() {
        // `.tmp-*` (interrupted stage) and `.old-*` (interrupted swap) are
        // reclaimed; a real slot dir is left alone. The in-flight filter is
        // stubbed off here (nothing is treated as live) to pin the prefix
        // guard alone. Mentally-revert the prefix guard and the real slot
        // would be swept too.
        let parent = tempfile::tempdir().unwrap();
        for name in [".tmp-pkg-1-0", ".old-pkg-2-1"] {
            std::fs::create_dir_all(parent.path().join(name)).unwrap();
        }
        std::fs::create_dir_all(parent.path().join("real-slot")).unwrap();

        let swept = sweep_staging_residue_filtered(parent.path(), |_, _| false);

        assert_eq!(swept.len(), 2, "both residue dirs swept: {swept:?}");
        assert!(parent.path().join("real-slot").is_dir(), "a real slot must survive");
        assert!(!parent.path().join(".tmp-pkg-1-0").exists());
        assert!(!parent.path().join(".old-pkg-2-1").exists());
    }

    #[test]
    fn sweep_skips_a_live_in_flight_build_residue() {
        // A `.tmp-*` a concurrent build still owns must survive `cache-gc`.
        // A freshly-created residue is younger than STAGING_RESIDUE_MIN_AGE, so
        // the REAL predicate keeps it via the mtime-age net regardless of pid.
        // Mentally-revert the in-flight guard in `sweep_staging_residue` (sweep
        // unconditionally) and this fresh residue is destroyed mid-build.
        let parent = tempfile::tempdir().unwrap();
        let live = parent.path().join(".tmp-pkg-4242-0");
        std::fs::create_dir_all(&live).unwrap();

        let swept = sweep_staging_residue(parent.path());

        assert!(swept.is_empty(), "a fresh in-flight residue must not be swept: {swept:?}");
        assert!(live.is_dir(), "the live build's stage dir must survive cache-gc");
    }

    #[test]
    fn embedded_pid_parses_from_dashed_residue_names() {
        // `<name>` carries the package name+version, itself dash-laden — the pid
        // is the second-to-last dash segment, the seq the last.
        assert_eq!(parse_embedded_pid(".tmp-pkg-1-0"), Some(1));
        assert_eq!(parse_embedded_pid(".old-my-pkg-0.1.0-98765-12"), Some(98765));
        // Non-numeric trailing segments ⇒ no pid (fall back to the age net).
        assert_eq!(parse_embedded_pid(".tmp-pkg-abc-def"), None);
        assert_eq!(parse_embedded_pid(".tmp-pkg"), None);
    }

    #[test]
    fn residue_in_flight_keeps_fresh_dirs_even_with_a_dead_pid() {
        // A just-created residue whose embedded pid is not live is still kept by
        // the age net (younger than the threshold) — the guard that makes
        // `cache-gc` safe when pid liveness can't be confirmed.
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join(".tmp-pkg-0-0");
        std::fs::create_dir_all(&fresh).unwrap();
        assert!(
            staging_residue_is_in_flight(".tmp-pkg-0-0", &fresh),
            "a fresh residue must read as in-flight via the mtime-age net"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pid_liveness_tracks_proc() {
        // Reverting `/proc/<pid>` to a constant `false` flips the live case.
        assert!(pid_is_alive(1), "pid 1 (init) is always live on Linux");
        assert!(
            !pid_is_alive(u32::MAX),
            "an impossible pid must read as dead"
        );
    }
}
