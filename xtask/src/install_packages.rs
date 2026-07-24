// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `xtask install-packages` — the ground-truth enumerator + driver for the
//! `install-packages` CI gate (#1509).
//!
//! Standalone `packages/*` are NOT workspace members, so `cargo test --lib`
//! never compiles them — that is how a package rots undetected between releases.
//! This driver closes the gap by exercising the SAME on-box user install path a
//! consumer hits: it links the engine into a scratch consumer via
//! `streamlib link --engine` and then, for every distributable package, runs
//! `streamlib add <pkg_dir>` — which materializes the package into
//! `streamlib_modules/` and compiles the placed slot in place, failing with the
//! real per-package compiler error. It compiles then discards; it NEVER ships a
//! prebuilt (that is the distribution path, not the install path). CPU-only —
//! it stops at the compiled artifact, with no dlopen and no GPU.
//!
//! The distributable set is not a re-derived YAML list: it is driven from the
//! single [`streamlib_pack::non_distributable_path_offenders`] predicate — the
//! exact one the whole-tree static package-source emit skips on and the single-package
//! `streamlib pkg build` hard-fails on — so the CI skip set equals the emit skip
//! set by construction. A package carrying a `streamlib.yaml` path-`patch:`
//! block or a `Cargo.toml` dependency-table `path` dep (e.g. `api-server`,
//! `clap`, the test fixtures) is skipped exactly as the emit skips it. A TARGET
//! path (`[lib].path` / `[[bin]].path`) is not a dependency path and never
//! counts, so a schema-only package like `core` — whose test-only `Cargo.toml`
//! carries a `[lib].path` and workspace-inherited fields but no `processors:`
//! block — is DISTRIBUTABLE, not a path-predicate skip. It (like `escalate`)
//! drives through the install path as a no-op: the on-box compile is gated on
//! the manifest's `processors:` set, not on the presence of a `Cargo.toml`, so
//! a package with no Rust processors never builds its Cargo unit and never
//! fails.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// One package the enumerator classified as distributable and drove through the
/// install path, paired with the outcome of its on-box compile.
struct PackageCompileOutcome {
    /// `packages/<name>` directory the package was compiled from.
    package_dir: PathBuf,
    /// The captured `streamlib add` output when the compile failed; `None` on
    /// success. Carries the compiler error so CI names it per package.
    failure_output: Option<String>,
}

/// Drive every distributable `packages/*` through the on-box install path,
/// failing (and naming each package with its compiler error) when any stops
/// compiling.
///
/// `streamlib_bin` is the pre-built `streamlib` CLI. The engine is resolved via
/// `streamlib link --engine <workspace_root>` into a throwaway scratch consumer
/// (`[patch.crates-io]` path overrides + `STREAMLIB_LINK_CHECKOUT`), so package
/// crate deps that pin `version = "0.7.x"` resolve to the local checkout — there
/// is no crates registry and no publish step.
pub fn run(workspace_root: &Path, streamlib_bin: &Path) -> Result<()> {
    if !streamlib_bin.is_file() {
        bail!(
            "streamlib CLI binary not found at {} — build it first with `cargo build -p streamlib-cli`",
            streamlib_bin.display()
        );
    }
    let streamlib_bin = streamlib_bin
        .canonicalize()
        .with_context(|| format!("canonicalize {}", streamlib_bin.display()))?;

    let packages_dir = workspace_root.join("packages");
    let (distributable, skipped) = partition_packages(&packages_dir)
        .with_context(|| format!("enumerating {}", packages_dir.display()))?;

    for (pkg_dir, offenders) in &skipped {
        tracing::info!(
            package = %package_label(pkg_dir),
            offenders = %offenders.join(", "),
            "install-packages: skipping non-distributable package (path-patch / path-dep); it is \
             skipped by the whole-tree emit for the same reason"
        );
    }

    if distributable.is_empty() {
        bail!(
            "no distributable packages found under {} — the enumerator would validate nothing",
            packages_dir.display()
        );
    }

    // The scratch consumer holds the engine link marker + `[patch.crates-io]`
    // cargo config and every materialized `streamlib_modules/` slot; the whole
    // tree is discarded on drop (compile-then-discard, never ship a prebuilt).
    let scratch = tempfile::tempdir().context("creating the scratch consumer directory")?;
    establish_engine_link(&streamlib_bin, workspace_root, scratch.path())
        .context("linking the engine into the scratch consumer")?;

    tracing::info!(
        count = distributable.len(),
        "install-packages: compiling every distributable package via the on-box install path"
    );

    let mut outcomes: Vec<PackageCompileOutcome> = Vec::with_capacity(distributable.len());
    for pkg_dir in &distributable {
        tracing::info!(package = %package_label(pkg_dir), "install-packages: streamlib add (compile-on-place)");
        let failure_output = compile_via_install_path(&streamlib_bin, scratch.path(), pkg_dir)?;
        if failure_output.is_some() {
            tracing::error!(package = %package_label(pkg_dir), "install-packages: FAILED to compile");
        }
        outcomes.push(PackageCompileOutcome {
            package_dir: pkg_dir.clone(),
            failure_output,
        });
    }

    report_outcomes(&outcomes)
}

/// Partition `packages/*` (each a dir with a `streamlib.yaml`) into the
/// distributable set (compiled through the install path) and the skipped set
/// (each with the offenders that make it non-distributable), keyed on the exact
/// [`streamlib_pack::non_distributable_path_offenders`] predicate.
fn partition_packages(
    packages_dir: &Path,
) -> Result<(Vec<PathBuf>, Vec<(PathBuf, Vec<String>)>)> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(packages_dir)
        .with_context(|| format!("read_dir {}", packages_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("streamlib.yaml").is_file())
        .collect();
    entries.sort();

    let mut distributable = Vec::new();
    let mut skipped = Vec::new();
    for pkg_dir in entries {
        let offenders = streamlib_pack::non_distributable_path_offenders(&pkg_dir)
            .with_context(|| format!("classifying {}", pkg_dir.display()))?;
        if offenders.is_empty() {
            distributable.push(pkg_dir);
        } else {
            skipped.push((pkg_dir, offenders));
        }
    }
    Ok((distributable, skipped))
}

/// `streamlib link --engine <workspace_root> --skip-verify` in the scratch
/// consumer. `--skip-verify` because the scratch consumer is a bare directory
/// with no `Cargo.toml` to resolve against — the link only needs to write the
/// `[patch.crates-io]` cargo config + `.streamlib/link.json` marker the
/// per-package compile discovers.
fn establish_engine_link(
    streamlib_bin: &Path,
    workspace_root: &Path,
    scratch: &Path,
) -> Result<()> {
    let output = Command::new(streamlib_bin)
        .arg("link")
        .arg("--engine")
        .arg(workspace_root)
        .arg("--skip-verify")
        .current_dir(scratch)
        .output()
        .with_context(|| format!("spawning {} link --engine", streamlib_bin.display()))?;
    if !output.status.success() {
        bail!(
            "`streamlib link --engine {}` failed:\n{}",
            workspace_root.display(),
            combined_output(&output)
        );
    }
    Ok(())
}

/// Compile one package through the install path: `streamlib add <pkg_dir>` in
/// the scratch consumer. `add` materializes the package into
/// `streamlib_modules/@org/name/` and compiles the placed slot in place, rolling
/// the placement back on a compile failure. Returns `Some(output)` carrying the
/// compiler error when the compile failed, `None` on success (schema-only
/// packages compile trivially — a no-op, still `None`). Process cwd is the
/// scratch consumer so the build orchestrator discovers the engine link marker.
fn compile_via_install_path(
    streamlib_bin: &Path,
    scratch: &Path,
    pkg_dir: &Path,
) -> Result<Option<String>> {
    let output = Command::new(streamlib_bin)
        .arg("add")
        .arg(pkg_dir)
        .arg("--dir")
        .arg(scratch)
        .current_dir(scratch)
        .output()
        .with_context(|| format!("spawning {} add {}", streamlib_bin.display(), pkg_dir.display()))?;
    if output.status.success() {
        Ok(None)
    } else {
        Ok(Some(combined_output(&output)))
    }
}

/// Aggregate every compile outcome into a single pass/fail verdict: on any
/// failure, bail with each broken package named alongside its compiler error so
/// the CI log points straight at the regression.
fn report_outcomes(outcomes: &[PackageCompileOutcome]) -> Result<()> {
    use std::fmt::Write;

    let failures: Vec<&PackageCompileOutcome> =
        outcomes.iter().filter(|o| o.failure_output.is_some()).collect();
    let compiled = outcomes.len() - failures.len();

    if failures.is_empty() {
        tracing::info!(
            compiled,
            "install-packages: every distributable package compiled via the on-box install path"
        );
        return Ok(());
    }

    let mut message = format!(
        "install-packages: {} of {} distributable package(s) failed to compile via the on-box \
         install path:",
        failures.len(),
        outcomes.len()
    );
    for outcome in &failures {
        write!(
            message,
            "\n\n=== {} ===\n{}",
            package_label(&outcome.package_dir),
            outcome.failure_output.as_deref().unwrap_or("")
        )
        .ok();
    }
    bail!(message)
}

/// `packages/<name>` → `<name>` for log lines.
fn package_label(pkg_dir: &Path) -> String {
    pkg_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| pkg_dir.display().to_string())
}

/// Stdout + stderr of a captured subprocess, concatenated for a diagnostic.
fn combined_output(output: &std::process::Output) -> String {
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    combined
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a minimal package dir: a `streamlib.yaml` and, optionally, a
    /// `Cargo.toml` carrying a `path` dependency (which makes it
    /// non-distributable through the shared predicate).
    fn write_package(root: &Path, name: &str, with_path_dep: bool) -> PathBuf {
        let dir = root.join("packages").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("streamlib.yaml"),
            format!("package:\n  org: tatolab\n  name: {name}\n  version: 0.1.0\n"),
        )
        .unwrap();
        if with_path_dep {
            std::fs::write(
                dir.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                     [dependencies]\nlocal-thing = {{ path = \"../local-thing\" }}\n"
                ),
            )
            .unwrap();
        }
        dir
    }

    #[test]
    fn partition_drives_skip_set_from_the_shared_offender_predicate() {
        // A distributable package (no path artifacts) is compiled; a package
        // carrying a Cargo.toml path dep is skipped — the exact set the
        // whole-tree emit skips, keyed on the one shared predicate. Mentally
        // reverting to "compile every packages/* dir" would push the path-dep
        // package into the distributable list; this asserts against that.
        let root = tempfile::tempdir().unwrap();
        let dist = write_package(root.path(), "camera", false);
        let skip = write_package(root.path(), "api-server", true);

        let (distributable, skipped) =
            partition_packages(&root.path().join("packages")).unwrap();

        assert_eq!(distributable, vec![dist]);
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].0, skip);
        assert!(
            !skipped[0].1.is_empty(),
            "a skipped package must name its offenders"
        );
    }

    #[test]
    fn partition_ignores_dirs_without_a_manifest() {
        let root = tempfile::tempdir().unwrap();
        write_package(root.path(), "camera", false);
        // A stray dir with no streamlib.yaml is not a package.
        std::fs::create_dir_all(root.path().join("packages").join("not-a-package")).unwrap();

        let (distributable, skipped) =
            partition_packages(&root.path().join("packages")).unwrap();

        assert_eq!(distributable.len(), 1);
        assert!(skipped.is_empty());
    }

    #[test]
    fn report_outcomes_names_each_broken_package_with_its_error() {
        let outcomes = vec![
            PackageCompileOutcome {
                package_dir: PathBuf::from("packages/camera"),
                failure_output: None,
            },
            PackageCompileOutcome {
                package_dir: PathBuf::from("packages/webrtc"),
                failure_output: Some("error[E0599]: no method named `frobnicate`".to_string()),
            },
        ];
        let err = report_outcomes(&outcomes).expect_err("a broken package must fail the gate");
        let message = err.to_string();
        assert!(message.contains("webrtc"), "must name the broken package: {message}");
        assert!(
            message.contains("no method named `frobnicate`"),
            "must carry the compiler error: {message}"
        );
        assert!(
            !message.contains("packages/camera"),
            "must not name the green package: {message}"
        );
    }

    #[test]
    fn report_outcomes_passes_when_all_green() {
        let outcomes = vec![PackageCompileOutcome {
            package_dir: PathBuf::from("packages/camera"),
            failure_output: None,
        }];
        report_outcomes(&outcomes).unwrap();
    }
}
