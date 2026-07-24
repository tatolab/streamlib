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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use streamlib::sdk::runtime::{
    AddPackageOptions, AddPackageReport, AddPackageSource, AppModulesDir, BuildPolicy,
    LinkPackageReport,
};
use streamlib_idents::{DependencySpec, Manifest, PackageRef, VersionDependency, SemVerRange};
use streamlib_processor_schema::StreamlibYaml;

use super::build_on_place::{
    ConsoleBuildEventSink, build_added_slot_or_rollback, default_orchestrator,
};

/// `streamlib add <spec>` — records a dependency range when run in a
/// package-authoring dir, otherwise materializes a byte source into the app's
/// `streamlib_modules/` and compiles it on-the-box.
///
/// `no_build` skips the on-the-box compile (placement/reproduce only). Otherwise
/// the just-placed slot is compiled in place with [`BuildPolicy::IfStale`] and a
/// compile failure rolls the placement back — restoring the prior version when
/// the add replaced one.
pub fn add(
    spec: &str,
    dir: Option<&Path>,
    expect_sha256: Option<&str>,
    no_build: bool,
) -> Result<()> {
    let anchor = anchor_dir(dir)?;
    if let Some(manifest) = load_anchor_manifest(&anchor)? {
        if manifest.is_package_flavor() {
            if expect_sha256.is_some() {
                eprintln!(
                    "warning: --expect-sha256 is ignored when recording a dependency range in a \
                     package's streamlib.yaml; it applies only to archive and URL byte sources"
                );
            }
            return record_dependency_range(&anchor.join(Manifest::FILE_NAME), spec);
        }
        // A project-flavor (app) manifest that declares `dependencies:` is a
        // phantom-dependency list — an app resolves refs against its installed
        // set, not a manifest. Reject before touching streamlib_modules/.
        if let Some(count) = manifest.app_dependency_violation_count() {
            anyhow::bail!(
                streamlib::sdk::error::Error::AppManifestDeclaresDependencies {
                    manifest_path: anchor.join(Manifest::FILE_NAME).display().to_string(),
                    declared_count: count,
                }
            );
        }
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
        // When we're about to compile, retain a displaced prior slot so a
        // compile failure can restore the previously-working version rather
        // than leaving the operator with nothing.
        retain_replaced_slot_backup: !no_build,
    };
    let report = app
        .add_package(&source, &options)
        .map_err(|e| anyhow::anyhow!("add failed: {e}"))?;

    if !no_build {
        build_added_slot_or_rollback(
            &app,
            &report,
            &default_orchestrator(),
            &ConsoleBuildEventSink,
            BuildPolicy::IfStale,
        )?;
    }

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

/// Load the `streamlib.yaml` at `dir`, or `None` when the dir carries no
/// manifest (the common consumer-app case — an app is code, not a manifest).
fn load_anchor_manifest(dir: &Path) -> Result<Option<Manifest>> {
    if !dir.join(Manifest::FILE_NAME).is_file() {
        return Ok(None);
    }
    Manifest::load(dir)
        .map(Some)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Record `spec` (`@org/name@<version>`) as a caret dependency in the package
/// manifest at `manifest_path`. Only the top-level `dependencies:` block is
/// rewritten — every other line (key order, blank lines, and inline comments in
/// the hand-authored manifest) is left byte-for-byte intact.
fn record_dependency_range(manifest_path: &Path, spec: &str) -> Result<()> {
    let (pkg_ref, version) = parse_authoring_spec(spec)?;
    let range = SemVerRange::from_str(&format!("^{version}"))
        .map_err(|e| anyhow::anyhow!("invalid version `{version}` in `{spec}`: {e}"))?;

    let raw = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let mut manifest: StreamlibYaml = serde_yaml::from_str(&raw)
        .with_context(|| format!("parse {}", manifest_path.display()))?;

    let replaced = manifest
        .dependencies
        .insert(
            pkg_ref.clone(),
            DependencySpec::Version(VersionDependency {
                version: range.clone(),
                runtime: false,
            }),
        )
        .is_some();

    let updated = splice_dependencies_block(&raw, &manifest.dependencies)
        .context("rewrite the dependencies block in streamlib.yaml")?;
    std::fs::write(manifest_path, updated)
        .with_context(|| format!("write {}", manifest_path.display()))?;

    let verb = if replaced { "Updated" } else { "Added" };
    println!("{verb} dependency {pkg_ref} {range}");
    println!("  Manifest: {}", manifest_path.display());
    Ok(())
}

/// Re-emit only the top-level `dependencies:` block of a `streamlib.yaml`,
/// splicing it into `raw` in place of the existing block. When the manifest has
/// no `dependencies:` block yet, a fresh one is inserted directly after the
/// `package:` block (or appended when there is no `package:` block). Everything
/// outside the block — comments, key order, and blank lines — is preserved
/// verbatim.
fn splice_dependencies_block(
    raw: &str,
    dependencies: &BTreeMap<PackageRef, DependencySpec>,
) -> Result<String> {
    let block = render_dependencies_block(dependencies)?;
    let lines: Vec<&str> = raw.split_inclusive('\n').collect();

    if let Some((start, end)) = top_level_block_span(&lines, "dependencies") {
        return Ok(splice_lines(&lines, start, end, &block));
    }
    if let Some((_, package_end)) = top_level_block_span(&lines, "package") {
        return Ok(splice_lines(&lines, package_end, package_end, &block));
    }

    let mut out = raw.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&block);
    Ok(out)
}

/// Serialize a bare `dependencies:` mapping to YAML — the block that gets spliced
/// into the manifest. Ends with a trailing newline.
fn render_dependencies_block(
    dependencies: &BTreeMap<PackageRef, DependencySpec>,
) -> Result<String> {
    #[derive(serde::Serialize)]
    struct DependenciesOnly<'a> {
        dependencies: &'a BTreeMap<PackageRef, DependencySpec>,
    }
    serde_yaml::to_string(&DependenciesOnly { dependencies }).context("serialize dependencies block")
}

/// Replace `lines[start..end]` with `block`, reassembling the surrounding lines
/// (each of which still carries its own newline from `split_inclusive`) unchanged.
fn splice_lines(lines: &[&str], start: usize, end: usize, block: &str) -> String {
    let mut out = String::new();
    out.push_str(&lines[..start].concat());
    out.push_str(block);
    out.push_str(&lines[end..].concat());
    out
}

/// The `[start, end)` line span of a top-level `<key>:` block — the key line plus
/// its indented children, minus any trailing blank lines (so a blank separator
/// before the next block is preserved). `None` when the key is absent.
fn top_level_block_span(lines: &[&str], key: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|line| is_top_level_key(line, key))?;
    let mut end = start + 1;
    while end < lines.len() && !is_top_level_line(lines[end]) {
        end += 1;
    }
    while end > start + 1 && line_body(lines[end - 1]).trim().is_empty() {
        end -= 1;
    }
    Some((start, end))
}

/// Whether `line` opens a top-level `<key>:` mapping entry (column 0, `key`
/// immediately followed by `:`). Matches both `dependencies:` and the inline
/// `dependencies: { ... }` flow form.
fn is_top_level_key(line: &str, key: &str) -> bool {
    let body = line_body(line);
    match body.strip_prefix(key) {
        Some(rest) => rest.starts_with(':'),
        None => false,
    }
}

/// Whether `line` starts a new top-level construct — a non-blank line at column 0
/// (a key or a column-0 comment). Blank lines and indented lines belong to the
/// current block.
fn is_top_level_line(line: &str) -> bool {
    let body = line_body(line);
    !body.is_empty() && !body.starts_with(|c: char| c.is_whitespace())
}

/// A line with its trailing newline stripped.
fn line_body(line: &str) -> &str {
    line.strip_suffix('\n').unwrap_or(line)
}

/// Parse an authoring-mode spec into a canonical [`PackageRef`] and its
/// version. Accepts `@org/name@<version>`; the version is required in
/// authoring mode (the caret range is anchored on it — there is no package
/// source lookup here). Returns a CLI-friendly error otherwise.
fn parse_authoring_spec(spec: &str) -> Result<(PackageRef, String)> {
    let inner = spec.strip_prefix('@').ok_or_else(|| {
        anyhow::anyhow!("expected `@org/name@<version>` (e.g. `@tatolab/core@1.0.0`); got `{spec}`")
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
    fn detect_routes_version_coordinate_to_guidance_error() {
        // The old version-coordinate arm is gone; an `@org/name` spec gets
        // the typed guidance error from the source detector.
        let err = AddPackageSource::detect("@tatolab/camera").expect_err("must be rejected");
        let message = err.to_string();
        assert!(
            message.contains("version coordinate"),
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
    fn load_anchor_manifest_reads_flavor_and_deps() {
        let dir = tempfile::tempdir().unwrap();
        // No manifest → None (consumer flow).
        assert!(load_anchor_manifest(dir.path()).unwrap().is_none());

        // Project-flavor (no `package:`) with deps → flagged as an app-deps
        // violation, so `add` rejects rather than routing to consumer flow.
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "dependencies:\n  '@tatolab/core': ^1.0.0\n",
        )
        .unwrap();
        let app = load_anchor_manifest(dir.path()).unwrap().unwrap();
        assert!(!app.is_package_flavor());
        assert_eq!(app.app_dependency_violation_count(), Some(1));

        // Package-flavor → authoring dir (deps are legitimate there).
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: widget\n  version: 0.1.0\n",
        )
        .unwrap();
        let pkg = load_anchor_manifest(dir.path()).unwrap().unwrap();
        assert!(pkg.is_package_flavor());
        assert_eq!(pkg.app_dependency_violation_count(), None);
    }

    #[test]
    fn add_rejects_app_dir_manifest_declaring_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        // Project-flavor (no `package:`) manifest carrying a phantom
        // `dependencies:` block — an app resolves refs against its installed
        // set, so `add` must reject before touching streamlib_modules/.
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "dependencies:\n  '@tatolab/core': ^1.0.0\n",
        )
        .unwrap();

        let err = add("./anything", Some(dir.path()), None, false)
            .expect_err("add must reject an app-dir manifest that declares dependencies");
        match err.downcast_ref::<streamlib::sdk::error::Error>() {
            Some(streamlib::sdk::error::Error::AppManifestDeclaresDependencies {
                declared_count,
                ..
            }) => assert_eq!(*declared_count, 1),
            other => panic!("expected AppManifestDeclaresDependencies, got {other:?}"),
        }
        // The rejection is before any adoption side-effect.
        assert!(
            !dir.path().join("streamlib_modules").exists(),
            "add must not create streamlib_modules/ when it rejects the manifest"
        );
    }

    #[test]
    fn record_dependency_range_writes_caret_and_preserves_fields() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("streamlib.yaml");
        std::fs::write(
            &manifest_path,
            "# yaml-language-server: $schema=./schemas/streamlib.schema.json\n\
             package:\n  org: tatolab\n  name: widget\n  version: 0.1.0\n\
             processors:\n- name: Widget\n  runtime: rust\n  execution: reactive\n",
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
            DependencySpec::Version(r) => {
                assert_eq!(r.version, SemVerRange::from_str("^1.4.0").unwrap());
                assert!(!r.runtime);
            }
            other => panic!("expected version dep, got {other:?}"),
        }
        // Runtime fields (processors) survive the round-trip.
        assert_eq!(reparsed.processors.len(), 1);
        assert_eq!(reparsed.processors[0].name, "Widget");
    }

    #[test]
    fn record_dependency_range_is_format_preserving() {
        // A hand-authored manifest with comments and a deliberately
        // non-alphabetical top-level key order (`schemas:` before `processors:`)
        // must survive `streamlib add` with *only* the `dependencies:` block
        // added — no key reordering, no dropped comments.
        let original = "\
# yaml-language-server: $schema=./schemas/streamlib.schema.json
# Hand-authored — key order and comments below are intentional.
package:
  org: tatolab
  name: widget  # our widget package
  version: 0.1.0
schemas:
  WidgetConfig:
    file: schemas/widget_config.yaml
processors:
- name: Widget
  runtime: rust
  execution: reactive
";
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("streamlib.yaml");
        std::fs::write(&manifest_path, original).unwrap();

        record_dependency_range(&manifest_path, "@tatolab/core@1.4.0").unwrap();
        let written = std::fs::read_to_string(&manifest_path).unwrap();

        // Leading + inline comments survive verbatim.
        assert!(
            written.contains("# yaml-language-server:"),
            "magic-comment header dropped:\n{written}"
        );
        assert!(
            written.contains("# Hand-authored — key order and comments below are intentional."),
            "leading comment dropped:\n{written}"
        );
        assert!(
            written.contains("name: widget  # our widget package"),
            "inline comment dropped:\n{written}"
        );

        // Non-alphabetical top-level order preserved: `schemas:` stays before
        // `processors:` (a full reserialize would alphabetize them).
        let schemas_pos = written.find("\nschemas:").unwrap();
        let processors_pos = written.find("\nprocessors:").unwrap();
        let package_pos = written.find("\npackage:").unwrap();
        let deps_pos = written.find("\ndependencies:").unwrap();
        assert!(
            schemas_pos < processors_pos,
            "top-level keys were reordered:\n{written}"
        );
        // The new block lands right after `package:`.
        assert!(
            package_pos < deps_pos && deps_pos < schemas_pos,
            "dependencies block not placed after package:\n{written}"
        );

        // The caret range is recorded and the rest of the manifest reparses.
        let reparsed: StreamlibYaml = serde_yaml::from_str(&written).unwrap();
        let core = parse_canonical_package_ref("@tatolab/core").unwrap();
        match reparsed.dependencies.get(&core).unwrap() {
            DependencySpec::Version(r) => {
                assert_eq!(r.version, SemVerRange::from_str("^1.4.0").unwrap());
            }
            other => panic!("expected version dep, got {other:?}"),
        }

        // Nothing but the `dependencies:` block was added: deleting that block
        // (its key line plus every indented child) reproduces the original.
        let mut kept: Vec<&str> = Vec::new();
        let mut in_dependencies = false;
        for line in written.lines() {
            if line == "dependencies:" {
                in_dependencies = true;
                continue;
            }
            if in_dependencies {
                if line.starts_with(char::is_whitespace) {
                    continue;
                }
                in_dependencies = false;
            }
            kept.push(line);
        }
        assert_eq!(
            kept.join("\n"),
            original.trim_end_matches('\n'),
            "content outside the dependencies block changed:\n{written}"
        );
    }
}
