// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib add <source>` / `streamlib remove <name>` — per-app package
//! adoption, the node_modules model for streamlib packages.
//!
//! Thin CLI wrappers over the programmatic
//! [`streamlib::sdk::runtime::AppModulesDir`] twins so the CLI and any
//! embedding host share one adoption flow. `add` takes any valid streamlib
//! package byte source — a folder, an archive (`.slpkg` / `.zip` /
//! `.tar.gz`), or a URL — materializes it into `streamlib_modules/@org/name/`
//! beside the app, and records it in the app's `streamlib.lock`. Identity is
//! read from the package's own manifest, never supplied as a lookup
//! coordinate. `remove` reverses it. Neither builds anything.

use std::path::Path;

use anyhow::{Context, Result};
use streamlib::sdk::runtime::{
    AddPackageOptions, AddPackageReport, AddPackageSource, AppModulesDir,
};
use streamlib_idents::PackageRef;

/// Add one package source (folder | archive | URL) into the app's
/// `streamlib_modules/` + `streamlib.lock`.
pub fn add(spec: &str, dir: Option<&Path>, expect_sha256: Option<&str>) -> Result<()> {
    let app = app_modules_dir(dir)?;
    let source = AddPackageSource::detect(spec).map_err(|e| anyhow::anyhow!("{e}"))?;
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

/// The app-modules anchor: `--dir` when given, else the exact CWD.
fn app_modules_dir(dir: Option<&Path>) -> Result<AppModulesDir> {
    match dir {
        Some(root) => Ok(AppModulesDir::at(root)),
        None => AppModulesDir::from_cwd().map_err(|e| anyhow::anyhow!("{e}")),
    }
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
}
