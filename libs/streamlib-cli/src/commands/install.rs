// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib install` — reproduce this app's `streamlib_modules/` folder from
//! its committed `streamlib.lock`, exactly and offline.
//!
//! Thin CLI wrapper over the programmatic
//! [`streamlib::sdk::runtime::AppModulesDir::install_from_lockfile`] twin so the
//! CLI and any embedding host share one reproduction flow. Install is the seam
//! between acquisition and reproduction: `add`/`link` decide what's in the
//! environment and record it in `streamlib.lock`; `install` reproduces that
//! decision elsewhere (a fresh checkout, a container image build) with no
//! resolution decisions. Each byte-source entry (folder / archive / URL) is
//! re-materialized and re-verified against its recorded content hash; a linked
//! entry's symlink is re-created (a gone checkout target is a typed error — a
//! dev link isn't reproducible on another machine). Never builds.

use std::path::Path;

use anyhow::Result;
use streamlib::sdk::runtime::{AppModulesDir, InstallFromLockfileReport, InstalledFromLockKind};

/// Reproduce the app's `streamlib_modules/` from its `streamlib.lock`.
pub fn install(dir: Option<&Path>) -> Result<()> {
    let app = app_modules_dir(dir)?;
    println!("Installing from {}…", app.lockfile_path().display());
    let report = app
        .install_from_lockfile()
        .map_err(|e| anyhow::anyhow!("install failed: {e}"))?;
    print_install_report(&report);
    Ok(())
}

/// The app-modules anchor: `--dir` when given, else the exact CWD.
fn app_modules_dir(dir: Option<&Path>) -> Result<AppModulesDir> {
    match dir {
        Some(root) => Ok(AppModulesDir::at(root)),
        None => AppModulesDir::from_cwd().map_err(|e| anyhow::anyhow!("{e}")),
    }
}

/// Pretty-print the reproduction outcome, one line per package.
fn print_install_report(report: &InstallFromLockfileReport) {
    println!();
    println!(
        "Reproduced {} package(s) into {}",
        report.packages.len(),
        report.modules_dir.display()
    );
    for pkg in &report.packages {
        let how = match pkg.kind {
            InstalledFromLockKind::Materialized => "materialized",
            InstalledFromLockKind::Linked => "linked",
        };
        let verb = if pkg.replaced_existing {
            "replaced"
        } else {
            "reproduced"
        };
        println!(
            "  {} v{}  [{how}, {verb}]  {}",
            pkg.package,
            pkg.version,
            pkg.package_dir.display()
        );
    }
    if report.packages.is_empty() {
        println!("  (lockfile records no packages)");
    }
}
