// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cargo-build orchestration helpers used by `streamlib pack` and
//! `streamlib-build-orchestrator`.
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
/// `streamlib-build-orchestrator` can pull it in without dragging the
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

/// A direct tatolab-registry cargo dependency pin: the crate `name`, the raw
/// cargo version requirement `req` (cargo semantics — a bare `0.5.0` means
/// caret), and the concrete floor `version` (leading range operators
/// `=` / `^` / `~` / `>=` stripped). The unit the release-completeness check
/// validates against a release manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TatolabRegistryPin {
    pub name: String,
    /// Raw cargo version requirement as written in the manifest
    /// (`"0.5.0"`, `"=0.4.36"`, `"^0.5.1"`, …).
    pub req: String,
    /// Floor version with the range operator stripped.
    pub version: String,
}

/// Strip a cargo version-req's leading range operator, yielding the concrete
/// floor version string (`=0.5.0` / `^0.5.0` / `>=0.5.0` → `0.5.0`).
fn strip_version_req_operator(req: &str) -> String {
    req.trim()
        .trim_start_matches(['=', '^', '~', '>', '<'])
        .trim()
        .to_string()
}

/// Collect tatolab-registry pins from one `[dependencies]`-shaped table into
/// `out`. The dep key is the crate name unless a `package = "..."` rename
/// overrides it.
fn collect_tatolab_pins_from_table(table: &toml::value::Table, out: &mut Vec<TatolabRegistryPin>) {
    for (key, value) in table {
        let Some(dep) = value.as_table() else {
            continue;
        };
        let is_tatolab = dep.get("registry").and_then(|r| r.as_str()) == Some("tatolab");
        if !is_tatolab {
            continue;
        }
        let Some(version) = dep.get("version").and_then(|v| v.as_str()) else {
            continue;
        };
        let name = dep
            .get("package")
            .and_then(|p| p.as_str())
            .unwrap_or(key.as_str())
            .to_string();
        out.push(TatolabRegistryPin {
            name,
            req: version.trim().to_string(),
            version: strip_version_req_operator(version),
        });
    }
}

/// Read a package's **direct** tatolab-registry cargo dependency pins from its
/// `Cargo.toml` — every `[dependencies]` / `[build-dependencies]` /
/// `[target.*.dependencies]` / `[target.*.build-dependencies]` entry declaring
/// `registry = "tatolab"`. `dev-dependencies` are excluded (they don't
/// participate in the release closure).
///
/// Returns `(name, floor-version)` pairs. `Ok(vec![])` when the package has no
/// `Cargo.toml` (schema-only package) — the check simply has nothing to
/// validate in that case.
pub fn read_tatolab_registry_pins(package_dir: &Path) -> Result<Vec<TatolabRegistryPin>> {
    let cargo_toml_path = package_dir.join("Cargo.toml");
    if !cargo_toml_path.exists() {
        return Ok(Vec::new());
    }
    let body = std::fs::read_to_string(&cargo_toml_path)
        .with_context(|| format!("Failed to read {}", cargo_toml_path.display()))?;
    let parsed: toml::Value = toml::from_str(&body)
        .with_context(|| format!("Failed to parse {}", cargo_toml_path.display()))?;

    let mut pins = Vec::new();
    for section in ["dependencies", "build-dependencies"] {
        if let Some(table) = parsed.get(section).and_then(|v| v.as_table()) {
            collect_tatolab_pins_from_table(table, &mut pins);
        }
    }
    // `[target.'cfg(...)'.{dependencies,build-dependencies}]`.
    if let Some(targets) = parsed.get("target").and_then(|v| v.as_table()) {
        for cfg_tbl in targets.values() {
            let Some(cfg_tbl) = cfg_tbl.as_table() else {
                continue;
            };
            for section in ["dependencies", "build-dependencies"] {
                if let Some(table) = cfg_tbl.get(section).and_then(|v| v.as_table()) {
                    collect_tatolab_pins_from_table(table, &mut pins);
                }
            }
        }
    }
    Ok(pins)
}

/// Cargo build profile selector for [`run_cargo_build`].
///
/// `Release` is the production-distribution shape (`streamlib pack`
/// uses it). `Dev` skips optimization for a faster inner loop
/// (`streamlib-build-orchestrator` defaults to it).
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

    let stdout = String::from_utf8(output.stdout)
        .with_context(|| format!("cargo build output for {} was not valid UTF-8", cargo_name))?;

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
/// processor — the predicate `streamlib-build-orchestrator` uses to
/// decide which packages need a `cargo build`.
pub fn has_rust_runtime_processors(config: &ProjectConfigMinimal) -> bool {
    config
        .processors
        .iter()
        .any(|p| matches!(p.runtime.language, ProcessorLanguage::Rust))
}

/// Whether a parsed manifest declares at least one Python runtime processor —
/// the predicate the orchestrator uses to decide it must ensure the Python
/// subprocess native host (`libstreamlib_python_native`) is built + cached.
pub fn has_python_runtime_processors(config: &ProjectConfigMinimal) -> bool {
    config
        .processors
        .iter()
        .any(|p| matches!(p.runtime.language, ProcessorLanguage::Python))
}

/// Whether a parsed manifest declares at least one TypeScript runtime
/// processor — the predicate the orchestrator uses to decide it must ensure
/// the Deno subprocess native host (`libstreamlib_deno_native`) is built +
/// cached.
pub fn has_typescript_runtime_processors(config: &ProjectConfigMinimal) -> bool {
    config
        .processors
        .iter()
        .any(|p| matches!(p.runtime.language, ProcessorLanguage::TypeScript))
}

/// Canonical staged-directory name for a package: `<org>__<name>` with
/// no `@` literal so the path is filesystem-safe across every
/// supported host. The corresponding wire-form id is `@<org>/<name>`.
///
/// `streamlib-build-orchestrator` and the runtime's
/// `ModuleResolverStrategy::WorkspaceStaged` resolver MUST agree on
/// this format — keep the conversion in one place.
pub fn staged_package_dir_name(org: &str, name: &str) -> String {
    format!("{}__{}", org, name)
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
    fn read_tatolab_registry_pins_collects_direct_pins_and_strips_operators() {
        // Mixed dep table: tatolab pins (with `=` / bare / cfg-target /
        // build-dep / renamed) must be collected with operators stripped;
        // non-tatolab deps (serde) and dev-deps must be excluded. Reverting the
        // `registry == "tatolab"` filter would slurp serde; reverting the
        // operator strip would leave `=` on the version and mismatch the
        // manifest's stamped string.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            r#"
[package]
name = "streamlib-jpeg"
version = "1.0.7"

[build-dependencies]
streamlib-jtd-codegen = {version = "=0.5.1", registry = "tatolab"}

[dependencies]
streamlib-plugin-sdk = {version = "0.5.1", registry = "tatolab"}
streamlib-macros = {version = "^0.5.1", registry = "tatolab"}
serde = {version = "1.0", features = ["derive"]}
renamed-dep = {version = "=0.5.1", registry = "tatolab", package = "streamlib-plugin-abi"}

[dev-dependencies]
streamlib-test-fixtures = {version = "0.5.1", registry = "tatolab"}

[target.'cfg(target_os = "linux")'.dependencies]
vulkan-jpeg = {version = ">=0.5.1", registry = "tatolab"}
"#,
        )
        .unwrap();

        let mut pins = read_tatolab_registry_pins(dir.path()).unwrap();
        pins.sort_by(|a, b| a.name.cmp(&b.name));
        let got: Vec<(String, String, String)> = pins
            .into_iter()
            .map(|p| (p.name, p.req, p.version))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "streamlib-jtd-codegen".to_string(),
                    "=0.5.1".to_string(),
                    "0.5.1".to_string()
                ),
                (
                    "streamlib-macros".to_string(),
                    "^0.5.1".to_string(),
                    "0.5.1".to_string()
                ),
                // renamed via `package = "streamlib-plugin-abi"`
                (
                    "streamlib-plugin-abi".to_string(),
                    "=0.5.1".to_string(),
                    "0.5.1".to_string()
                ),
                (
                    "streamlib-plugin-sdk".to_string(),
                    "0.5.1".to_string(),
                    "0.5.1".to_string()
                ),
                (
                    "vulkan-jpeg".to_string(),
                    ">=0.5.1".to_string(),
                    "0.5.1".to_string()
                ),
            ],
            "tatolab pins must include normal/build/cfg-target deps (renamed via \
             `package`), carry the raw req, strip range operators for the floor, \
             and exclude serde + dev-deps"
        );
    }

    #[test]
    fn read_tatolab_registry_pins_empty_without_cargo_toml() {
        let dir = tempdir().unwrap();
        assert!(read_tatolab_registry_pins(dir.path()).unwrap().is_empty());
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
        let err =
            read_cargo_package_name(dir.path()).expect_err("workspace-only Cargo.toml must error");
        let msg = format!("{err}");
        assert!(
            msg.contains("[package].name"),
            "error must point at the missing field, got: {msg}"
        );
    }

    #[test]
    fn read_cargo_package_name_errors_when_cargo_toml_missing() {
        let dir = tempdir().unwrap();
        let err = read_cargo_package_name(dir.path()).expect_err("missing Cargo.toml must error");
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
        let found = parse_cargo_artifact_for_cdylib(json, "grayscale-plugin", "so").unwrap();
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
        let found = parse_cargo_artifact_for_cdylib(json, "grayscale-plugin", "so").unwrap();
        assert_eq!(found, Some(PathBuf::from("/tmp/libgrayscale_plugin.so")));
    }

    #[test]
    fn parse_cargo_artifact_for_cdylib_ignores_unrelated_crate_artifacts() {
        let json = r#"
{"reason":"compiler-artifact","target":{"name":"some-other","kind":["cdylib"]},"filenames":["/tmp/libother.so"]}
"#;
        let found = parse_cargo_artifact_for_cdylib(json, "grayscale-plugin", "so").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn parse_cargo_artifact_for_cdylib_returns_none_when_no_cdylib_built() {
        // rlib-only build: no cdylib should be picked even though the
        // crate name matches.
        let json = r#"
{"reason":"compiler-artifact","target":{"name":"grayscale-plugin","kind":["lib"]},"filenames":["/tmp/libgrayscale_plugin.rlib"]}
"#;
        let found = parse_cargo_artifact_for_cdylib(json, "grayscale-plugin", "so").unwrap();
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
        let found = parse_cargo_artifact_for_cdylib(json, "grayscale-plugin", "so").unwrap();
        assert_eq!(found, Some(PathBuf::from("/tmp/libgrayscale_plugin.so")));
    }

    #[test]
    fn staged_package_dir_name_uses_double_underscore_no_at_literal() {
        // The wire-form id `@tatolab/core` becomes the
        // filesystem-safe dir name `tatolab__core`. Both the xtask
        // and the runtime's WorkspaceStaged resolver must compute it
        // the same way — reverting the format would silently break
        // the resolver's lookup, so the test pins the literal shape.
        assert_eq!(staged_package_dir_name("tatolab", "core"), "tatolab__core");
        assert_eq!(
            staged_package_dir_name("vendor", "fancy-plugin"),
            "vendor__fancy-plugin",
        );
    }
}
