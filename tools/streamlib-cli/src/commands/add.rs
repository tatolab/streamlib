// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib add <source>` / `streamlib remove <name>` — package adoption.
//!
//! `streamlib add` is context-sensitive on the directory it runs in:
//!
//! - **Package-authoring dir** (a `streamlib.yaml` with a `package:` block):
//!   `streamlib add @org/name@<version>` records a caret dependency
//!   (`^<version>`) into that package's own `dependencies:` — the schema-tier
//!   analog of `cargo add`. `pkg build` then reconciles the declared set
//!   against what the code references.
//! - **Consumer / app dir** (no `package:` block): `add` takes any valid
//!   streamlib package byte source — a folder, an archive (`.slpkg` / `.zip`
//!   / `.tar.gz`), or a URL — materializes it into `streamlib_modules/@org/name/`
//!   beside the app, and records it in the app's `streamlib.lock`. Identity is
//!   read from the package's own manifest, never supplied as a lookup
//!   coordinate. This flows through the programmatic
//!   [`streamlib::sdk::runtime::AppModulesDir`] twin so the CLI and any
//!   embedding host share one adoption flow.
//!
//! `remove` reverses the consumer-dir flow. Neither builds anything.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use streamlib::sdk::runtime::{
    AddPackageOptions, AddPackageReport, AddPackageSource, AppModulesDir, LinkPackageReport,
};
use streamlib_idents::{
    DependencySpec, Manifest, PackageRef, RegistryDependency, SemVerRange,
};
use streamlib_processor_schema::StreamlibYaml;

/// `streamlib add <spec>` — records a dependency range when run in a
/// package-authoring dir, otherwise materializes a byte source into the app's
/// `streamlib_modules/`.
pub fn add(spec: &str, dir: Option<&Path>, expect_sha256: Option<&str>) -> Result<()> {
    let anchor = anchor_dir(dir)?;
    if let Some(manifest_path) = package_authoring_manifest(&anchor)? {
        if expect_sha256.is_some() {
            eprintln!(
                "warning: --expect-sha256 is ignored when recording a dependency range in a \
                 package's streamlib.yaml; it applies only to archive and URL byte sources"
            );
        }
        return record_dependency_range(&manifest_path, spec);
    }

    let app = app_modules_dir(dir)?;
    let source = AddPackageSource::detect(spec).map_err(|e| anyhow::anyhow!("{e}"))?;
    // A folder source has no archive bytes to hash, so `--expect-sha256` is a
    // no-op there — warn rather than silently ignoring it.
    if expect_sha256.is_some() && matches!(source, AddPackageSource::Folder { .. }) {
        eprintln!(
            "warning: --expect-sha256 is ignored for a folder source (no archive bytes to \
             verify); it applies only to archive and URL sources"
        );
    }
    println!("Adding {spec}…");

    let options = AddPackageOptions {
        expected_archive_sha256: expect_sha256.map(|s| s.to_string()),
    };
    let report = app
        .add_package(&source, &options)
        .map_err(|e| anyhow::anyhow!("add failed: {e}"))?;

    print_add_report(&report);
    Ok(())
}

/// Remove one package by its canonical `@org/name` ref — delete its
/// `streamlib_modules/@org/name/` folder and drop its `streamlib.lock` entry.
pub fn remove(name: &str, dir: Option<&Path>) -> Result<()> {
    let app = app_modules_dir(dir)?;
    let pkg_ref = parse_canonical_package_ref(name)?;
    let report = app
        .remove_package(&pkg_ref)
        .map_err(|e| anyhow::anyhow!("remove failed: {e}"))?;
    match report.version {
        Some(version) => println!("Removed {} v{}", report.package, version),
        None => println!("Removed {}", report.package),
    }
    if report.package_dir_removed {
        println!("  Deleted: {}", report.package_dir.display());
    }
    Ok(())
}

/// Symlink a local package checkout into the app's `streamlib_modules/` —
/// `add`, but as a live symlink instead of a copy, so checkout edits are
/// picked up on the next run. Identity comes from the checkout's manifest.
pub fn link(path: &Path, dir: Option<&Path>) -> Result<()> {
    let app = app_modules_dir(dir)?;
    println!("Linking {}…", path.display());
    let report = app
        .link_package(path)
        .map_err(|e| anyhow::anyhow!("link failed: {e}"))?;
    print_link_report(&report);
    Ok(())
}

/// Remove a package's `streamlib_modules/` symlink by its canonical
/// `@org/name` ref, dropping its `streamlib.lock` entry. The linked checkout
/// on disk is untouched.
pub fn unlink(name: &str, dir: Option<&Path>) -> Result<()> {
    let app = app_modules_dir(dir)?;
    let pkg_ref = parse_canonical_package_ref(name)?;
    let report = app
        .unlink_package(&pkg_ref)
        .map_err(|e| anyhow::anyhow!("unlink failed: {e}"))?;
    println!("Unlinked {}", report.package);
    if let Some(target) = &report.link_target {
        println!("  Was linked to: {}", target.display());
    }
    if report.link_removed {
        println!("  Removed link:  {}", report.package_dir.display());
    }
    Ok(())
}

/// The app-modules anchor: `--dir` when given, else the exact CWD.
fn app_modules_dir(dir: Option<&Path>) -> Result<AppModulesDir> {
    match dir {
        Some(root) => Ok(AppModulesDir::at(root)),
        None => AppModulesDir::from_cwd().map_err(|e| anyhow::anyhow!("{e}")),
    }
}

/// The directory `add` anchors on: `--dir` when given, else the exact CWD
/// (no walk-up — mirrors [`app_modules_dir`]).
fn anchor_dir(dir: Option<&Path>) -> Result<PathBuf> {
    match dir {
        Some(root) => Ok(root.to_path_buf()),
        None => std::env::current_dir().context("resolve current working directory"),
    }
}

/// The `streamlib.yaml` path when `dir` holds a **package-authoring** manifest
/// (one with a `package:` block), else `None`. A missing manifest or a
/// project-flavor manifest (no `package:`) routes `add` to the consumer flow.
fn package_authoring_manifest(dir: &Path) -> Result<Option<PathBuf>> {
    let manifest_path = dir.join(Manifest::FILE_NAME);
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let manifest = Manifest::load(dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(manifest.is_package_flavor().then_some(manifest_path))
}

/// Record `spec` (`@org/name@<version>`) as a caret dependency in the package
/// manifest at `manifest_path`, preserving every other manifest field and the
/// leading `# yaml-language-server` / comment header.
fn record_dependency_range(manifest_path: &Path, spec: &str) -> Result<()> {
    let (pkg_ref, version) = parse_authoring_spec(spec)?;
    let range = SemVerRange::from_str(&format!("^{version}"))
        .map_err(|e| anyhow::anyhow!("invalid version `{version}` in `{spec}`: {e}"))?;

    let raw = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let (header, body) = split_leading_comment_header(&raw);
    let mut manifest: StreamlibYaml =
        serde_yaml::from_str(&body).with_context(|| format!("parse {}", manifest_path.display()))?;

    let replaced = manifest
        .dependencies
        .insert(
            pkg_ref.clone(),
            DependencySpec::Registry(RegistryDependency {
                version: range.clone(),
                runtime: false,
            }),
        )
        .is_some();

    let serialized =
        serde_yaml::to_string(&manifest).context("serialize streamlib.yaml")?;
    std::fs::write(manifest_path, format!("{header}{serialized}"))
        .with_context(|| format!("write {}", manifest_path.display()))?;

    let verb = if replaced { "Updated" } else { "Added" };
    println!("{verb} dependency {pkg_ref} {range}");
    println!("  Manifest: {}", manifest_path.display());
    Ok(())
}

/// Parse an authoring-mode spec into a canonical [`PackageRef`] and its
/// version. Accepts `@org/name@<version>`; the version is required in
/// authoring mode (the caret range is anchored on it — there is no registry
/// lookup here). Returns a CLI-friendly error otherwise.
fn parse_authoring_spec(spec: &str) -> Result<(PackageRef, String)> {
    let inner = spec.strip_prefix('@').ok_or_else(|| {
        anyhow::anyhow!(
            "expected `@org/name@<version>` (e.g. `@tatolab/core@1.0.0`); got `{spec}`"
        )
    })?;
    let (name_part, version) = inner.split_once('@').ok_or_else(|| {
        anyhow::anyhow!(
            "missing version in `{spec}` — recording a dependency requires a version to \
             anchor the caret range on: `streamlib add @org/name@<version>`"
        )
    })?;
    if version.is_empty() {
        anyhow::bail!("empty version in `{spec}`: `streamlib add @org/name@<version>`");
    }
    let pkg_ref = parse_canonical_package_ref(&format!("@{name_part}"))?;
    Ok((pkg_ref, version.to_string()))
}

/// Split a `streamlib.yaml` into its leading comment/blank header (the
/// `# yaml-language-server: $schema=` magic comment and friends) and the rest,
/// so a serde round-trip can re-prepend the header it would otherwise drop.
fn split_leading_comment_header(raw: &str) -> (String, String) {
    let mut split_at = 0;
    for line in raw.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            split_at += line.len();
        } else {
            break;
        }
    }
    (raw[..split_at].to_string(), raw[split_at..].to_string())
}

/// Pretty-print the add outcome plus a manifest-derived processor summary.
fn print_add_report(report: &AddPackageReport) {
    println!();
    let verb = if report.replaced_existing {
        "Replaced"
    } else {
        "Added"
    };
    println!("{verb} {} v{}", report.package, report.version);
    println!("  Folder: {}", report.package_dir.display());
    println!("  Lock:   {}", report.lockfile_path.display());

    print_processor_summary(&report.package_dir);
}

/// Pretty-print the link outcome plus a manifest-derived processor summary.
fn print_link_report(report: &LinkPackageReport) {
    println!();
    let verb = if report.replaced_existing {
        "Relinked"
    } else {
        "Linked"
    };
    println!("{verb} {} v{}", report.package, report.version);
    println!("  Link:   {}", report.package_dir.display());
    println!("  Target: {}", report.link_target.display());
    println!("  Lock:   {}", report.lockfile_path.display());

    print_processor_summary(&report.package_dir);
}

/// Print the processors the added package contributes, read from its own
/// materialized `streamlib.yaml` (no network, no catalog).
fn print_processor_summary(package_dir: &Path) {
    use streamlib::engine_internal::core::ProjectConfig;
    let config = match ProjectConfig::load(package_dir) {
        Ok(config) => config,
        Err(e) => {
            // The manifest already validated during the add; a read failure
            // here only degrades the summary, never the add itself.
            tracing::warn!(
                dir = %package_dir.display(),
                error = %e,
                "add: reading the materialized manifest for the summary failed"
            );
            return;
        }
    };

    if config.processors.is_empty() {
        println!();
        println!("The package declares no processors (schema-only package).");
        return;
    }
    println!();
    println!("Processors ({}):", config.processors.len());
    for proc in &config.processors {
        let desc = proc
            .description
            .as_deref()
            .map(|d| format!(" — {d}"))
            .unwrap_or_default();
        println!("  {}{}  [{:?}]", proc.name, desc, proc.runtime.language);
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
            println!("    Config: {} ({})", config_ref.name, config_ref.schema);
        }
    }
}

/// Convert a canonical-form string (`@org/name`) into a typed [`PackageRef`]
/// via the official Deserialize path, wrapping the round-trip with a
/// CLI-friendly error.
fn parse_canonical_package_ref(arg: &str) -> Result<PackageRef> {
    serde_yaml::from_value::<PackageRef>(serde_yaml::Value::String(arg.to_string())).with_context(
        || {
            format!(
                "Invalid canonical package reference '{arg}'. Expected `@org/name` form \
                 (e.g. `@tatolab/core`)."
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_canonical_package_ref_rejects_bare_name() {
        // The canonical `@org/name` form is required — a bare short name is
        // ambiguous across orgs and must be rejected.
        assert!(parse_canonical_package_ref("foo").is_err());
        assert!(parse_canonical_package_ref("@tatolab/foo").is_ok());
    }

    #[test]
    fn detect_routes_registry_coordinate_to_guidance_error() {
        // The old registry-coordinate arm is gone; an `@org/name` spec gets
        // the typed guidance error from the source detector.
        let err = AddPackageSource::detect("@tatolab/camera").expect_err("must be rejected");
        let message = err.to_string();
        assert!(
            message.contains("registry coordinate"),
            "guidance missing: {message}"
        );
    }

    #[test]
    fn detect_classifies_url_specs() {
        assert!(matches!(
            AddPackageSource::detect("https://example.com/pkg.slpkg").unwrap(),
            AddPackageSource::Url { .. }
        ));
        assert!(AddPackageSource::detect("ftp://example.com/pkg.slpkg").is_err());
    }

    #[test]
    fn parse_authoring_spec_requires_org_name_and_version() {
        let (pkg_ref, version) = parse_authoring_spec("@tatolab/core@1.2.3").unwrap();
        assert_eq!(pkg_ref.to_string(), "@tatolab/core");
        assert_eq!(version, "1.2.3");
        // A bare `@org/name` has no version to anchor the caret on.
        assert!(parse_authoring_spec("@tatolab/core").is_err());
        // A non-`@`-prefixed spec is not a package coordinate.
        assert!(parse_authoring_spec("tatolab/core@1.0.0").is_err());
    }

    #[test]
    fn package_authoring_manifest_detects_only_package_flavor() {
        let dir = tempfile::tempdir().unwrap();
        // No manifest → consumer flow.
        assert!(package_authoring_manifest(dir.path()).unwrap().is_none());
        // Project-flavor (no `package:`) → consumer flow.
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "dependencies:\n  '@tatolab/core': ^1.0.0\n",
        )
        .unwrap();
        assert!(package_authoring_manifest(dir.path()).unwrap().is_none());
        // Package-flavor → authoring.
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: widget\n  version: 0.1.0\n",
        )
        .unwrap();
        assert!(package_authoring_manifest(dir.path()).unwrap().is_some());
    }

    #[test]
    fn record_dependency_range_writes_caret_and_preserves_fields() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("streamlib.yaml");
        std::fs::write(
            &manifest_path,
            "# yaml-language-server: $schema=./schemas/streamlib.schema.json\n\
             package:\n  org: tatolab\n  name: widget\n  version: 0.1.0\n\
             processors:\n- name: Widget\n  version: 1.0.0\n  runtime: rust\n  execution: reactive\n",
        )
        .unwrap();

        record_dependency_range(&manifest_path, "@tatolab/core@1.4.0").unwrap();

        let written = std::fs::read_to_string(&manifest_path).unwrap();
        // Leading magic comment preserved.
        assert!(
            written.starts_with("# yaml-language-server:"),
            "header dropped: {written}"
        );
        // Caret range recorded.
        let reparsed: StreamlibYaml = serde_yaml::from_str(&written).unwrap();
        let core = parse_canonical_package_ref("@tatolab/core").unwrap();
        match reparsed.dependencies.get(&core).unwrap() {
            DependencySpec::Registry(r) => {
                assert_eq!(r.version, SemVerRange::from_str("^1.4.0").unwrap());
                assert!(!r.runtime);
            }
            other => panic!("expected registry dep, got {other:?}"),
        }
        // Runtime fields (processors) survive the round-trip.
        assert_eq!(reparsed.processors.len(), 1);
        assert_eq!(reparsed.processors[0].name, "Widget");
    }
}
