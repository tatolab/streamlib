// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cargo-build orchestration helpers used by `streamlib pack` and
//! `cargo xtask build-plugins`.
//!
//! The two callers need the same battle-tested cdylib-discovery /
//! target-triple-staging logic — host target triple probe, dylib
//! extension probe, `cargo build --message-format=json` invoker,
//! artifact-path parser, package-directory walker, per-package
//! stage step. This crate is the single source of truth so the
//! pack and xtask paths cannot diverge.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

pub use streamlib_processor_schema::{ProcessorLanguage, ProjectConfigMinimal};

/// Host target triple (e.g. `x86_64-unknown-linux-gnu`), captured
/// at build time from cargo's `TARGET` env var.
///
/// `streamlib-engine`'s `core::runtime::host_target_triple()` carries
/// the same value via its own `build.rs`. The duplication is
/// deliberate: this crate intentionally has zero engine deps so
/// `cargo xtask build-plugins` can pull it in without dragging the
/// full RHI / IPC / runtime layers. Both copies read the same cargo
/// `TARGET` env var at compile time and therefore agree by
/// construction.
pub fn host_target_triple() -> &'static str {
    env!("STREAMLIB_CARGO_BUILD_HOST_TARGET")
}

/// Dylib extension for the current host OS (`so` / `dylib` / `dll`).
pub fn host_dylib_extension() -> &'static str {
    if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    }
}

/// Enumerate dylibs in `lib_dir` whose extension matches `dylib_ext`.
/// Returns an empty Vec when the directory does not exist or contains
/// no matching files — callers decide whether that's an error.
pub fn collect_host_dylibs_in_lib(lib_dir: &Path, dylib_ext: &str) -> Result<Vec<PathBuf>> {
    if !lib_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir(lib_dir)
        .with_context(|| format!("Failed to read lib/ directory: {}", lib_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == dylib_ext) {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

/// Read `[package].name` from a directory's `Cargo.toml`. This is the
/// value `cargo build -p <name>` accepts; it is **not** the same string
/// as `streamlib.yaml`'s `package.name` (the two plugin examples
/// deliberately use different names — see
/// `examples/camera-rust-plugin/plugin/`).
pub fn read_cargo_package_name(package_dir: &Path) -> Result<String> {
    let cargo_toml_path = package_dir.join("Cargo.toml");
    let body = std::fs::read_to_string(&cargo_toml_path).with_context(|| {
        format!(
            "Failed to read {} — auto-build requires a Cargo.toml \
             alongside streamlib.yaml so cargo can locate the crate",
            cargo_toml_path.display()
        )
    })?;
    let parsed: toml::Value = toml::from_str(&body)
        .with_context(|| format!("Failed to parse {}", cargo_toml_path.display()))?;
    let name = parsed
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} has no [package].name — auto-build needs a named Cargo \
                 crate to invoke `cargo build -p <name>`",
                cargo_toml_path.display()
            )
        })?;
    Ok(name.to_string())
}

/// Cargo build profile selector for [`run_cargo_build`].
///
/// `Release` is the production-distribution shape (`streamlib pack`
/// uses it). `Dev` skips optimization for a faster inner loop
/// (`cargo xtask build-plugins` defaults to it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CargoProfile {
    Dev,
    Release,
}

impl CargoProfile {
    /// Human-facing label for log lines.
    pub fn label(self) -> &'static str {
        match self {
            CargoProfile::Dev => "dev",
            CargoProfile::Release => "release",
        }
    }
}

/// Invoke `cargo build [--release] -p <cargo_name>
/// --message-format=json` from `package_dir` and parse the JSON
/// output for the produced host-OS cdylib path.
///
/// `--message-format=json` is the canonical way to discover Cargo
/// artifact paths — it survives `CARGO_TARGET_DIR` overrides,
/// workspace `[build].target-dir` config, custom `[profile]` settings,
/// and anything else that would invalidate a hardcoded
/// `<workspace>/target/release/<file>` assumption.
///
/// Cargo's progress output (the `Compiling foo …` lines and compiler
/// diagnostics) is left inherited on stderr so a cold build does not
/// appear hung. Only stdout — the JSON message stream — is captured.
pub fn run_cargo_build(
    package_dir: &Path,
    cargo_name: &str,
    dylib_ext: &str,
    profile: CargoProfile,
) -> Result<PathBuf> {
    let profile_label = profile.label();
    tracing::info!(
        "Building {} ({} profile, cargo build -p {})",
        cargo_name,
        profile_label,
        cargo_name,
    );
    let mut command = Command::new("cargo");
    command.arg("build");
    if matches!(profile, CargoProfile::Release) {
        command.arg("--release");
    }
    let output = command
        .arg("--message-format=json")
        .arg("-p")
        .arg(cargo_name)
        .current_dir(package_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| {
            format!(
                "Failed to invoke `cargo build {} -p {}` in {}",
                if matches!(profile, CargoProfile::Release) {
                    "--release"
                } else {
                    ""
                },
                cargo_name,
                package_dir.display()
            )
        })?;

    if !output.status.success() {
        anyhow::bail!(
            "cargo build ({}) -p {} failed (run from {}). \
             See cargo's output above.",
            profile_label,
            cargo_name,
            package_dir.display(),
        );
    }

    let stdout = String::from_utf8(output.stdout).with_context(|| {
        format!(
            "cargo build output for {} was not valid UTF-8",
            cargo_name
        )
    })?;

    parse_cargo_artifact_for_cdylib(&stdout, cargo_name, dylib_ext)?.ok_or_else(|| {
        anyhow::anyhow!(
            "cargo build ({}) -p {} completed but produced no \
             host-OS cdylib (`*.{}`). Confirm the crate declares \
             `crate-type = [\"cdylib\"]` in [lib].",
            profile_label,
            cargo_name,
            dylib_ext
        )
    })
}

/// Back-compat wrapper for [`run_cargo_build`] with [`CargoProfile::Release`].
/// `streamlib pack` retains this entry point so the release-default shape
/// for distribution artifacts is preserved.
pub fn run_cargo_build_release(
    package_dir: &Path,
    cargo_name: &str,
    dylib_ext: &str,
) -> Result<PathBuf> {
    run_cargo_build(package_dir, cargo_name, dylib_ext, CargoProfile::Release)
}

/// Scan one stream of `--message-format=json` cargo output for the
/// host cdylib artifact belonging to `cargo_name`. Returns the
/// absolute path of the matching dylib if any `compiler-artifact`
/// message lists a cdylib produced for the named crate.
///
/// Cargo normalizes crate-target names by replacing dashes with
/// underscores (so the package `grayscale-plugin` builds the cdylib
/// target `grayscale_plugin`). The comparison accepts both forms.
pub fn parse_cargo_artifact_for_cdylib(
    cargo_json_output: &str,
    cargo_name: &str,
    dylib_ext: &str,
) -> Result<Option<PathBuf>> {
    let dot_ext = format!(".{}", dylib_ext);
    let normalized = cargo_name.replace('-', "_");
    for line in cargo_json_output.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let msg: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // Non-JSON lines (rare with --message-format=json) are skipped.
        };
        if msg.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let target = msg.get("target");
        let target_name = target.and_then(|t| t.get("name")).and_then(|n| n.as_str());
        let name_matches = matches!(target_name, Some(t) if t == cargo_name || t == normalized);
        if !name_matches {
            continue;
        }
        let is_cdylib = target
            .and_then(|t| t.get("kind"))
            .and_then(|k| k.as_array())
            .map(|arr| arr.iter().any(|v| v.as_str() == Some("cdylib")))
            .unwrap_or(false);
        if !is_cdylib {
            continue;
        }
        let filenames = msg
            .get("filenames")
            .and_then(|f| f.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();
        for filename in filenames {
            if filename.ends_with(&dot_ext) {
                return Ok(Some(PathBuf::from(filename)));
            }
        }
    }
    Ok(None)
}

/// Read `<package_dir>/streamlib.yaml` and parse the minimal subset
/// (`package:` + `processors:`). Returns `Ok(None)` when no yaml is
/// present — the caller distinguishes "non-package directory" from
/// "malformed package".
pub fn read_minimal_project_config(package_dir: &Path) -> Result<Option<ProjectConfigMinimal>> {
    let yaml_path = package_dir.join("streamlib.yaml");
    if !yaml_path.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(&yaml_path)
        .with_context(|| format!("Failed to read {}", yaml_path.display()))?;
    let config: ProjectConfigMinimal = serde_yaml::from_str(&body)
        .with_context(|| format!("Failed to parse {}", yaml_path.display()))?;
    Ok(Some(config))
}

/// Whether a parsed manifest declares at least one Rust runtime
/// processor — the predicate `cargo xtask build-plugins` uses to
/// decide which packages need a `cargo build`.
pub fn has_rust_runtime_processors(config: &ProjectConfigMinimal) -> bool {
    config
        .processors
        .iter()
        .any(|p| matches!(p.runtime.language, ProcessorLanguage::Rust))
}

/// Walk the immediate children of each root directory looking for
/// `streamlib.yaml` files. Returns the parent directories that own
/// one. Used to find `packages/<name>/`-shape packages in the
/// workspace.
pub fn discover_package_dirs(roots: &[&Path]) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(root)
            .with_context(|| format!("Failed to read {}", root.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path.join("streamlib.yaml").exists() {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Filter `roots` to the package directories whose `streamlib.yaml`
/// declares at least one Rust runtime processor. The set
/// `cargo xtask build-plugins` iterates over.
pub fn discover_rust_impl_packages(roots: &[&Path]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for dir in discover_package_dirs(roots)? {
        let Some(config) = read_minimal_project_config(&dir)? else {
            continue;
        };
        if has_rust_runtime_processors(&config) {
            out.push(dir);
        }
    }
    Ok(out)
}

/// Canonical staged-directory name for a package: `<org>__<name>` with
/// no `@` literal so the path is filesystem-safe across every
/// supported host. The corresponding wire-form id is `@<org>/<name>`.
///
/// `cargo xtask build-plugins` and `Runner::load_workspace_packages`
/// MUST agree on this format — keep the conversion in one place.
pub fn staged_package_dir_name(org: &str, name: &str) -> String {
    format!("{}__{}", org, name)
}

/// Stage a workspace package into `staged_root/<org>__<name>/` so
/// `Runner::load_workspace_packages` can find it without depending on
/// the original `packages/` layout.
///
/// Copies the package's `streamlib.yaml` and `schemas/` directory into
/// the staged dir, rewriting every `patch:` entry's path so it
/// resolves to a sibling staged dir (`../<dep_org>__<dep_name>`)
/// rather than the source-tree path. When `built_cdylib` is
/// `Some(path)`, the cdylib is staged into
/// `<staged_dir>/lib/<host_triple>/<filename>` matching the
/// `Runner::load_project` triple-keyed convention. Schemas-only
/// packages pass `None`.
///
/// Returns the staged package directory (absolute path).
pub fn stage_package_for_dev_load(
    package_dir: &Path,
    staged_root: &Path,
    built_cdylib: Option<&Path>,
    host_triple: &str,
) -> Result<PathBuf> {
    use streamlib_idents::Manifest;
    use streamlib_processor_schema::StreamlibYaml;

    let manifest_path = package_dir.join(Manifest::FILE_NAME);
    let manifest_body = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    // Parse as the full `StreamlibYaml` schema (not the slimmer
    // `streamlib_idents::Manifest`) so the `processors:` list survives
    // the re-serialize step. Dropping that list during staging silently
    // turns a Rust-impl package into a schemas-only one and the
    // runtime never registers the cdylib's processors.
    let mut manifest: StreamlibYaml = serde_yaml::from_str(&manifest_body)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;
    let package = manifest.package.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "{} has no [package] section — cannot determine staged dir name",
            manifest_path.display()
        )
    })?;
    let org = package.org.as_str().to_string();
    let name = package.name.as_str().to_string();

    // Rewrite every `patch:` entry to resolve to a sibling staged dir
    // (`../<dep_org>__<dep_name>`). Same for path-flavor `dependencies:`
    // entries (rare but legal). Registry / git entries pass through
    // unchanged.
    rewrite_patches_to_staged_siblings(&mut manifest.patch);
    rewrite_patches_to_staged_siblings(&mut manifest.dependencies);

    let staged_dir = staged_root.join(staged_package_dir_name(&org, &name));
    // Wipe the staged dir before re-staging so removed schemas / renamed
    // patches don't linger. Hermetic regeneration is the contract — if
    // a user clobbers files into staged_dir manually, they lose them on
    // the next stage.
    if staged_dir.exists() {
        std::fs::remove_dir_all(&staged_dir).with_context(|| {
            format!(
                "Failed to clear stale staged dir {}",
                staged_dir.display()
            )
        })?;
    }
    std::fs::create_dir_all(&staged_dir)
        .with_context(|| format!("Failed to create {}", staged_dir.display()))?;

    // Write the rewritten manifest into the staged dir.
    let rewritten_yaml = serde_yaml::to_string(&manifest)
        .with_context(|| format!("Failed to serialize rewritten {}", Manifest::FILE_NAME))?;
    std::fs::write(staged_dir.join(Manifest::FILE_NAME), rewritten_yaml)
        .with_context(|| format!("Failed to write staged {}", Manifest::FILE_NAME))?;

    // Copy schemas/ if it exists.
    let src_schemas = package_dir.join("schemas");
    if src_schemas.is_dir() {
        let dst_schemas = staged_dir.join("schemas");
        copy_dir_recursive(&src_schemas, &dst_schemas)?;
    }

    // Stage the cdylib if provided. Schemas-only packages skip this.
    if let Some(cdylib_src) = built_cdylib {
        stage_built_cdylib(&staged_dir, cdylib_src, host_triple)?;
    }

    Ok(staged_dir)
}

fn rewrite_patches_to_staged_siblings(
    entries: &mut std::collections::BTreeMap<
        streamlib_idents::PackageRef,
        streamlib_idents::DependencySpec,
    >,
) {
    use streamlib_idents::{DependencySpec, PathDependency};
    for (dep_ref, spec) in entries.iter_mut() {
        if let DependencySpec::Path(_) = spec {
            let sibling = PathBuf::from("..").join(staged_package_dir_name(
                dep_ref.org.as_str(),
                dep_ref.name.as_str(),
            ));
            *spec = DependencySpec::Path(PathDependency { path: sibling });
        }
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)
        .with_context(|| format!("Failed to create {}", dst.display()))?;
    for entry in std::fs::read_dir(src)
        .with_context(|| format!("Failed to read {}", src.display()))?
    {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).with_context(|| {
                format!(
                    "Failed to copy {} → {}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
        }
    }
    Ok(())
}

/// Stage a freshly-built cdylib at `<package_dir>/lib/<host_triple>/<filename>`
/// so `Runner::load_project` can resolve it by the triple-keyed
/// convention. Creates the directory as needed. Returns the staged
/// destination path. Copies (not symlinks) so a subsequent
/// `cargo clean` doesn't invalidate the staged artifact.
pub fn stage_built_cdylib(
    package_dir: &Path,
    built_cdylib: &Path,
    host_triple: &str,
) -> Result<PathBuf> {
    let triple_dir = package_dir.join("lib").join(host_triple);
    std::fs::create_dir_all(&triple_dir).with_context(|| {
        format!(
            "Failed to create staging directory {}",
            triple_dir.display()
        )
    })?;
    let filename = built_cdylib
        .file_name()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Built cdylib path {} has no filename component",
                built_cdylib.display()
            )
        })?;
    let dest = triple_dir.join(filename);
    std::fs::copy(built_cdylib, &dest).with_context(|| {
        format!(
            "Failed to stage {} to {}",
            built_cdylib.display(),
            dest.display()
        )
    })?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_cargo_toml(dir: &Path, name: &str) {
        let body = format!(
            r#"
[package]
name = "{}"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]
"#,
            name
        );
        std::fs::write(dir.join("Cargo.toml"), body).unwrap();
    }

    #[test]
    fn host_dylib_extension_matches_target_os() {
        let ext = host_dylib_extension();
        #[cfg(target_os = "macos")]
        assert_eq!(ext, "dylib");
        #[cfg(target_os = "windows")]
        assert_eq!(ext, "dll");
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        assert_eq!(ext, "so");
    }

    #[test]
    fn host_target_triple_is_non_empty_and_well_formed() {
        // build.rs captures the cargo TARGET env var into this crate's
        // rustc-env. Reverting the build.rs `println!` would still
        // compile (env!() would point at the empty string) but the
        // returned value must look like a target triple — at minimum
        // arch-vendor-os, separated by dashes.
        let triple = host_target_triple();
        assert!(!triple.is_empty(), "host_target_triple must not be empty");
        assert!(
            triple.matches('-').count() >= 2,
            "host_target_triple must look like a triple (arch-vendor-os…), got: {triple}"
        );
    }

    #[test]
    fn collect_host_dylibs_in_lib_returns_empty_when_dir_missing() {
        let dir = tempdir().unwrap();
        let lib = dir.path().join("lib");
        let found = collect_host_dylibs_in_lib(&lib, "so").unwrap();
        assert!(
            found.is_empty(),
            "missing lib/ dir should produce empty list, got: {:?}",
            found
        );
    }

    #[test]
    fn collect_host_dylibs_in_lib_filters_by_extension() {
        // Populate lib/ with one host-OS dylib and one non-matching
        // file; the helper must pick the host file and skip the rest.
        // Mentally reverting the extension filter would slurp every
        // file in lib/ and ship junk inside the slpkg.
        let dir = tempdir().unwrap();
        let lib = dir.path().join("lib");
        std::fs::create_dir(&lib).unwrap();
        std::fs::write(lib.join("libfoo.so"), b"so-bytes").unwrap();
        std::fs::write(lib.join("libfoo.dylib"), b"dylib-bytes").unwrap();
        std::fs::write(lib.join("README.md"), b"docs").unwrap();

        let so_only = collect_host_dylibs_in_lib(&lib, "so").unwrap();
        assert_eq!(so_only.len(), 1);
        assert!(so_only[0].ends_with("libfoo.so"));

        let dylib_only = collect_host_dylibs_in_lib(&lib, "dylib").unwrap();
        assert_eq!(dylib_only.len(), 1);
        assert!(dylib_only[0].ends_with("libfoo.dylib"));
    }

    #[test]
    fn read_cargo_package_name_extracts_name_from_cargo_toml() {
        // Mentally reverting the toml parse to a string-grep would
        // happen to pass this case but break when a `[dependencies]`
        // block carries a `name = "..."` line — the parser is the
        // contract.
        let dir = tempdir().unwrap();
        write_cargo_toml(dir.path(), "grayscale-plugin");
        let name = read_cargo_package_name(dir.path()).unwrap();
        assert_eq!(name, "grayscale-plugin");
    }

    #[test]
    fn read_cargo_package_name_errors_without_package_section() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            r#"
[workspace]
members = ["foo"]
"#,
        )
        .unwrap();
        let err = read_cargo_package_name(dir.path())
            .expect_err("workspace-only Cargo.toml must error");
        let msg = format!("{err}");
        assert!(
            msg.contains("[package].name"),
            "error must point at the missing field, got: {msg}"
        );
    }

    #[test]
    fn read_cargo_package_name_errors_when_cargo_toml_missing() {
        let dir = tempdir().unwrap();
        let err = read_cargo_package_name(dir.path())
            .expect_err("missing Cargo.toml must error");
        let msg = format!("{err}");
        assert!(
            msg.contains("Cargo.toml"),
            "error must name Cargo.toml, got: {msg}"
        );
    }

    #[test]
    fn parse_cargo_artifact_for_cdylib_returns_matching_host_dylib() {
        // Real cargo-output shape sampled from
        // `cargo build --release -p grayscale-plugin --message-format=json`
        // against examples/camera-rust-plugin/plugin/: Cargo
        // normalizes dashes-to-underscores in target.name, so a
        // package `grayscale-plugin` emits target name
        // `grayscale_plugin`. The filter has to accept BOTH spellings
        // — match against just the dashed form (or just the
        // underscore form) would silently fail. Reverting the kind /
        // name / extension checks would pick the wrong crate or a
        // non-cdylib file (rlib/intermediate).
        let json = r#"
{"reason":"compiler-artifact","target":{"name":"other-crate","kind":["lib"]},"filenames":["/tmp/target/release/libother.rlib"]}
{"reason":"compiler-artifact","target":{"name":"grayscale_plugin","kind":["cdylib"]},"filenames":["/tmp/target/release/libgrayscale_plugin.so","/tmp/target/release/libgrayscale_plugin.d"]}
{"reason":"build-finished","success":true}
"#;
        let found =
            parse_cargo_artifact_for_cdylib(json, "grayscale-plugin", "so").unwrap();
        assert_eq!(
            found,
            Some(PathBuf::from("/tmp/target/release/libgrayscale_plugin.so"))
        );
    }

    #[test]
    fn parse_cargo_artifact_for_cdylib_matches_dash_form_target_name() {
        // A crate whose [lib].name explicitly retains a dash (rare
        // but legal — Cargo allows it via `[lib].name = "foo-bar"`)
        // emits a dash-form target.name. The filter must accept that
        // too.
        let json = r#"
{"reason":"compiler-artifact","target":{"name":"grayscale-plugin","kind":["cdylib"]},"filenames":["/tmp/libgrayscale_plugin.so"]}
"#;
        let found =
            parse_cargo_artifact_for_cdylib(json, "grayscale-plugin", "so").unwrap();
        assert_eq!(found, Some(PathBuf::from("/tmp/libgrayscale_plugin.so")));
    }

    #[test]
    fn parse_cargo_artifact_for_cdylib_ignores_unrelated_crate_artifacts() {
        let json = r#"
{"reason":"compiler-artifact","target":{"name":"some-other","kind":["cdylib"]},"filenames":["/tmp/libother.so"]}
"#;
        let found =
            parse_cargo_artifact_for_cdylib(json, "grayscale-plugin", "so").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn parse_cargo_artifact_for_cdylib_returns_none_when_no_cdylib_built() {
        // rlib-only build: no cdylib should be picked even though the
        // crate name matches.
        let json = r#"
{"reason":"compiler-artifact","target":{"name":"grayscale-plugin","kind":["lib"]},"filenames":["/tmp/libgrayscale_plugin.rlib"]}
"#;
        let found =
            parse_cargo_artifact_for_cdylib(json, "grayscale-plugin", "so").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn parse_cargo_artifact_for_cdylib_ignores_non_json_lines() {
        // Cargo's `--message-format=json` is usually clean JSON-per-
        // line, but build scripts can leak stray output. The parser
        // must skip non-JSON lines without erroring.
        let json = r#"warning: unused variable: `foo`
{"reason":"compiler-artifact","target":{"name":"grayscale_plugin","kind":["cdylib"]},"filenames":["/tmp/libgrayscale_plugin.so"]}
not-json-at-all
"#;
        let found =
            parse_cargo_artifact_for_cdylib(json, "grayscale-plugin", "so").unwrap();
        assert_eq!(found, Some(PathBuf::from("/tmp/libgrayscale_plugin.so")));
    }

    #[test]
    fn discover_package_dirs_finds_packages_with_streamlib_yaml() {
        // Walk pattern: each direct child of root that owns a
        // streamlib.yaml is a package. Children without the yaml
        // are skipped — they're unrelated source dirs (Rust crates
        // without a manifest, README dirs, etc.).
        let root = tempdir().unwrap();
        let pkg_a = root.path().join("alpha");
        let pkg_b = root.path().join("beta");
        let not_pkg = root.path().join("readme");
        std::fs::create_dir(&pkg_a).unwrap();
        std::fs::create_dir(&pkg_b).unwrap();
        std::fs::create_dir(&not_pkg).unwrap();
        std::fs::write(pkg_a.join("streamlib.yaml"), "package:\n  name: alpha\n").unwrap();
        std::fs::write(pkg_b.join("streamlib.yaml"), "package:\n  name: beta\n").unwrap();
        std::fs::write(not_pkg.join("README.md"), "no manifest").unwrap();

        let found = discover_package_dirs(&[root.path()]).unwrap();
        assert_eq!(found.len(), 2, "expected 2 packages, got: {:?}", found);
        assert!(found.iter().any(|p| p.ends_with("alpha")));
        assert!(found.iter().any(|p| p.ends_with("beta")));
        assert!(
            !found.iter().any(|p| p.ends_with("readme")),
            "discover_package_dirs must skip directories without streamlib.yaml"
        );
    }

    #[test]
    fn discover_rust_impl_packages_filters_by_runtime_language() {
        // Mixed workspace: one Rust-runtime package, one Python-runtime
        // package, one schemas-only package. Only the Rust one is
        // returned — `cargo xtask build-plugins` doesn't build Python
        // or schemas-only packages.
        let root = tempdir().unwrap();
        let rust_pkg = root.path().join("rust-pkg");
        let py_pkg = root.path().join("py-pkg");
        let schema_pkg = root.path().join("schema-pkg");
        for p in [&rust_pkg, &py_pkg, &schema_pkg] {
            std::fs::create_dir(p).unwrap();
        }
        std::fs::write(
            rust_pkg.join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: rust-pkg
  version: 0.1.0
processors:
  - name: RustProc
    version: 1.0.0
    description: "rust"
    runtime: rust
    execution: manual
    inputs: []
    outputs: []
"#,
        )
        .unwrap();
        std::fs::write(
            py_pkg.join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: py-pkg
  version: 0.1.0
processors:
  - name: PyProc
    version: 1.0.0
    description: "py"
    runtime: python
    execution: manual
    entrypoint: "py_proc:PyProc"
    inputs: []
    outputs: []
"#,
        )
        .unwrap();
        std::fs::write(
            schema_pkg.join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: schema-pkg
  version: 0.1.0
"#,
        )
        .unwrap();

        let found = discover_rust_impl_packages(&[root.path()]).unwrap();
        assert_eq!(found.len(), 1, "expected only rust-pkg, got: {:?}", found);
        assert!(found[0].ends_with("rust-pkg"));
    }

    #[test]
    fn stage_built_cdylib_copies_into_triple_keyed_dir() {
        // The staged path must land at
        // `<package_dir>/lib/<host_triple>/<filename>` because that's
        // the convention `Runner::load_project` resolves against.
        // Reverting the `create_dir_all` would error when the triple
        // subdir is missing on a fresh workspace — the test covers
        // that path by intentionally not pre-creating the destination.
        let dir = tempdir().unwrap();
        let built = dir.path().join("target-release-libfoo.so");
        std::fs::write(&built, b"cdylib-bytes").unwrap();

        let triple = "x86_64-unknown-linux-gnu";
        let staged = stage_built_cdylib(dir.path(), &built, triple).unwrap();

        let expected = dir
            .path()
            .join("lib")
            .join(triple)
            .join("target-release-libfoo.so");
        assert_eq!(staged, expected);
        assert!(expected.exists(), "staged artifact missing at {}", expected.display());
        let staged_bytes = std::fs::read(&expected).unwrap();
        assert_eq!(staged_bytes, b"cdylib-bytes");
        // Source is preserved — staging copies, never moves; a
        // subsequent `cargo clean` mustn't invalidate the staged copy.
        assert!(built.exists(), "source artifact must remain after staging");
    }

    #[test]
    fn staged_package_dir_name_uses_double_underscore_no_at_literal() {
        // The wire-form id `@tatolab/core` becomes the
        // filesystem-safe dir name `tatolab__core`. Both the xtask
        // and Runner::load_workspace_packages must compute it the same
        // way — reverting the format would silently break the load
        // helper's lookup, so the test pins the literal shape.
        assert_eq!(staged_package_dir_name("tatolab", "core"), "tatolab__core");
        assert_eq!(
            staged_package_dir_name("vendor", "fancy-plugin"),
            "vendor__fancy-plugin",
        );
    }

    fn write_minimal_manifest(dir: &Path, org: &str, name: &str, body: &str) {
        let yaml = format!(
            "package:\n  org: {org}\n  name: {name}\n  version: 1.0.0\n{body}"
        );
        std::fs::write(dir.join("streamlib.yaml"), yaml).unwrap();
    }

    #[test]
    fn stage_package_for_dev_load_schemas_only_copies_yaml_and_schemas_no_lib() {
        // Schemas-only packages — like `@tatolab/core` — must stage
        // cleanly without a `lib/` directory. Reverting the
        // `if let Some(cdylib_src) = built_cdylib` gate would either
        // panic (cdylib_src is None) or silently create an empty
        // lib/ — both shapes are wrong and load_workspace_packages
        // would mis-resolve.
        let src = tempdir().unwrap();
        write_minimal_manifest(src.path(), "tatolab", "core", "");
        let schemas_dir = src.path().join("schemas");
        std::fs::create_dir(&schemas_dir).unwrap();
        std::fs::write(
            schemas_dir.join("video_frame.yaml"),
            "metadata:\n  type: VideoFrame\n",
        )
        .unwrap();

        let staged_root = tempdir().unwrap();
        let staged = stage_package_for_dev_load(
            src.path(),
            staged_root.path(),
            None, /* schemas-only */
            "x86_64-unknown-linux-gnu",
        )
        .unwrap();

        assert_eq!(staged, staged_root.path().join("tatolab__core"));
        assert!(staged.join("streamlib.yaml").exists());
        assert!(staged.join("schemas/video_frame.yaml").exists());
        assert!(!staged.join("lib").exists(), "schemas-only stage must not create lib/");
    }

    #[test]
    fn stage_package_for_dev_load_with_cdylib_lands_under_triple_keyed_lib() {
        // Rust-impl packages stage both the yaml/schemas AND the cdylib
        // (under `lib/<host_triple>/<filename>`). The staged dir is the
        // self-contained mini-project Runner::load_workspace_packages
        // points at.
        let src = tempdir().unwrap();
        write_minimal_manifest(src.path(), "tatolab", "camera", "");
        let cdylib = src.path().join("libstreamlib_camera.so");
        std::fs::write(&cdylib, b"camera-cdylib-bytes").unwrap();

        let staged_root = tempdir().unwrap();
        let staged = stage_package_for_dev_load(
            src.path(),
            staged_root.path(),
            Some(&cdylib),
            "x86_64-unknown-linux-gnu",
        )
        .unwrap();

        assert_eq!(staged, staged_root.path().join("tatolab__camera"));
        let staged_cdylib = staged
            .join("lib")
            .join("x86_64-unknown-linux-gnu")
            .join("libstreamlib_camera.so");
        assert!(staged_cdylib.exists(), "expected staged cdylib at {}", staged_cdylib.display());
        let bytes = std::fs::read(&staged_cdylib).unwrap();
        assert_eq!(bytes, b"camera-cdylib-bytes");
    }

    #[test]
    fn stage_package_for_dev_load_rewrites_patches_to_sibling_staged_dirs() {
        // The whole point of staging is that the staged manifest's
        // `patch:` entries resolve to sibling staged dirs, not back
        // into the workspace source tree. Mentally reverting the
        // rewrite step would leave `path: ../core` in the staged yaml
        // and `load_workspace_packages` would either find the wrong
        // package (the source tree one, missing its built cdylib) or
        // fail outright.
        let src = tempdir().unwrap();
        write_minimal_manifest(
            src.path(),
            "tatolab",
            "camera",
            r#"dependencies:
  "@tatolab/core": "^1.0.0"
patch:
  "@tatolab/core":
    path: ../core
"#,
        );

        let staged_root = tempdir().unwrap();
        let staged = stage_package_for_dev_load(
            src.path(),
            staged_root.path(),
            None,
            "x86_64-unknown-linux-gnu",
        )
        .unwrap();

        // Re-parse the staged yaml and assert the patch path is the
        // sibling-staged-dir form.
        let body = std::fs::read_to_string(staged.join("streamlib.yaml")).unwrap();
        let restaged: streamlib_idents::Manifest = serde_yaml::from_str(&body).unwrap();
        let core_ref = restaged
            .patch
            .keys()
            .find(|k| k.org.as_str() == "tatolab" && k.name.as_str() == "core")
            .expect("patch must retain the @tatolab/core key");
        match &restaged.patch[core_ref] {
            streamlib_idents::DependencySpec::Path(p) => {
                assert_eq!(
                    p.path,
                    PathBuf::from("..").join("tatolab__core"),
                    "patch path must be rewritten to sibling staged dir, got: {}",
                    p.path.display()
                );
            }
            other => panic!("expected Path-flavor patch, got: {:?}", other),
        }
    }

    #[test]
    fn stage_package_for_dev_load_preserves_processors_and_env_lists() {
        // Regression: staging must NOT drop the `processors:` list or
        // any other field outside `streamlib_idents::Manifest`'s slim
        // schema. Earlier shape parsed-and-reserialized via
        // `streamlib_idents::Manifest` which has no `processors` field
        // (nor `env`) — the round-trip silently turned every Rust-impl
        // package into a schemas-only one and the runtime saw zero
        // processors to register. Mentally reverting to parsing as
        // Manifest would re-emit a yaml without `processors:`, and
        // `load_workspace_packages` would proceed without ever
        // dlopening the cdylib's processor registrations, surfacing
        // as `UnknownProcessorType` at the first `add_processor`
        // call. Asserting on BOTH `processors` and `env` widens the
        // regression net — any future intermediate struct that
        // captures one but not the other would fail this test.
        let src = tempdir().unwrap();
        write_minimal_manifest(
            src.path(),
            "tatolab",
            "demo",
            r#"processors:
  - name: DemoProcessor
    version: 1.0.0
    description: "demo"
    runtime:
      language: rust
    execution: manual
    inputs: []
    outputs: []
env:
  DEMO_KEY: "demo-value"
"#,
        );

        let staged_root = tempdir().unwrap();
        let staged = stage_package_for_dev_load(
            src.path(),
            staged_root.path(),
            None,
            "x86_64-unknown-linux-gnu",
        )
        .unwrap();

        let body = std::fs::read_to_string(staged.join("streamlib.yaml")).unwrap();
        let restaged: streamlib_processor_schema::StreamlibYaml =
            serde_yaml::from_str(&body).unwrap();
        assert_eq!(restaged.processors.len(), 1, "processors list must round-trip");
        assert_eq!(restaged.processors[0].name, "DemoProcessor");
        assert_eq!(
            restaged.env.get("DEMO_KEY").map(String::as_str),
            Some("demo-value"),
            "env map must round-trip"
        );
    }

    #[test]
    fn stage_package_for_dev_load_wipes_stale_files_on_restage() {
        // Hermetic restaging — a file present in the previous staged
        // dir but absent in the current source must disappear. Without
        // the `remove_dir_all` step, removed schemas or renamed
        // patches linger and `load_workspace_packages` resolves stale
        // content. Reverting the wipe would let stale_file survive
        // here.
        let src = tempdir().unwrap();
        write_minimal_manifest(src.path(), "tatolab", "stub", "");
        let staged_root = tempdir().unwrap();
        let staged_dir = staged_root.path().join("tatolab__stub");
        std::fs::create_dir(&staged_dir).unwrap();
        std::fs::write(staged_dir.join("stale.txt"), b"residue from a prior stage").unwrap();

        let staged = stage_package_for_dev_load(
            src.path(),
            staged_root.path(),
            None,
            "x86_64-unknown-linux-gnu",
        )
        .unwrap();

        assert_eq!(staged, staged_dir);
        assert!(
            !staged_dir.join("stale.txt").exists(),
            "stage must wipe stale files left over from a prior stage"
        );
        assert!(staged_dir.join("streamlib.yaml").exists());
    }
}
