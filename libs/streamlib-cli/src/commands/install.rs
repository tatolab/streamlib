// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib install` — resolve + materialize + lock an application's
//! package tree.
//!
//! Delegates to the programmatic [`streamlib::sdk::runtime::install`] so the
//! CLI and any embedding host share one install flow. Resolves the project's
//! `streamlib.yaml` range→concrete over the full transitive tree,
//! materializes every package into the installed-package cache (building
//! cdylibs / provisioning venvs / pre-building the subprocess native hosts),
//! and writes the application lockfile. A subsequent locked run
//! (`Runner::add_modules_from_lockfile`) then loads the pinned set offline.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use streamlib::sdk::runtime::{install, BuildEvent, BuildEventSink, BuildStream, InstallOptions};
use streamlib::sdk::PolyglotBuildOrchestrator;

/// Routes the orchestrator's build diagnostics to the CLI's stdout/stderr
/// during `install` (mirrors `streamlib add`'s interactive progress).
struct CliBuildSink;

impl BuildEventSink for CliBuildSink {
    fn emit(&self, event: BuildEvent) {
        match event {
            BuildEvent::Started { language } => println!("    [{language}] build started"),
            BuildEvent::Line { stream, line } => match stream {
                BuildStream::Stdout => println!("    {line}"),
                BuildStream::Stderr => eprintln!("    {line}"),
            },
            BuildEvent::Finished { language } => println!("    [{language}] build finished"),
            _ => {}
        }
    }
}

/// Resolve + materialize + lock the project rooted at `project_dir` (default:
/// the current working directory). Writes the application lockfile to
/// `lockfile_path` (default: `<project_dir>/streamlib-app.lock`).
pub fn run(project_dir: Option<&Path>, lockfile_path: Option<PathBuf>) -> Result<()> {
    let root = match project_dir {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().context("resolve current working directory")?,
    };
    if !root.join("streamlib.yaml").exists() {
        anyhow::bail!(
            "no streamlib.yaml at {} — run `streamlib install` from a project root, \
             or pass the project directory",
            root.display()
        );
    }

    // Link-mode provenance: a whole-tree link redirects the language
    // toolchains (cargo/uv/deno) at the checkout. Surface it so the operator
    // knows the lockfile reflects a linked tree (path-declared deps are
    // recorded as `path:` sources in the lockfile).
    if let Some(marker) = streamlib_pack::link_marker::find_active_link_marker(&root) {
        println!(
            "note: installing inside an active streamlib link (marker: {}) — \
             local checkout overrides are in effect",
            marker.display()
        );
    }

    println!("Resolving + materializing package graph at {}...", root.display());
    let orchestrator = PolyglotBuildOrchestrator::default();
    let sink = CliBuildSink;
    let options = InstallOptions {
        lockfile_path,
        ..Default::default()
    };
    let report = install(&root, &orchestrator, &sink, &options)
        .map_err(|e| anyhow::anyhow!("install failed: {e}"))?;

    println!();
    println!(
        "Installed {} package(s); lockfile written to {}",
        report.packages.len(),
        report.lockfile_path.display()
    );
    for (pkg, version) in &report.packages {
        println!("  {pkg} v{version}");
    }
    Ok(())
}
