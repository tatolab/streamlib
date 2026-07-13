// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib registry use` / `streamlib registry serve` — thin CLI wrappers
//! over the programmatic [`streamlib::sdk::registry`] so the CLI and any
//! embedding host share one toolchain-config flow.
//!
//! `use` points a consumer at a single registry location: it writes the cargo
//! `[source]` replacement into `.cargo/config.toml` (serverless `local-registry`
//! for a local folder, sparse mirror for an HTTP mount) and derives the pypi /
//! npm channels. `serve` starts a localhost static mount so npm — the one
//! ecosystem with no `file://` registry story — can resolve `@tatolab`, writing
//! the `.npmrc` scope so it isn't hand-edited.

use std::path::Path;

use anyhow::{Context, Result};
use streamlib::sdk::registry::{
    self, CargoReplacementSource, ServeRegistryOptions, UseRegistryOptions,
};

/// `streamlib registry use <tree>` — configure this consumer's cargo/pypi/npm
/// channels from one registry location (a local folder, `file://`, or
/// `http(s)://` mount) with a single command.
pub fn use_registry(tree_ref: &str) -> Result<()> {
    let consumer_root = std::env::current_dir().context("resolve current working directory")?;
    let report = registry::use_registry(&consumer_root, tree_ref, &UseRegistryOptions::default())?;

    println!(
        "Configured consumer against registry: {}",
        report.registry.base_url
    );
    println!();
    println!("cargo — wrote {}", report.cargo_config_path.display());
    match &report.cargo_replacement {
        CargoReplacementSource::LocalRegistry(dir) => {
            println!("  serverless local-registry mirror: {}", dir.display());
            println!("  resolves with `cargo build --offline` — no server needed.");
        }
        CargoReplacementSource::SparseMirror(index) => {
            println!("  sparse mirror source: {index}");
        }
    }
    println!();
    println!("registry seed — export so `.slpkg` resolution + the build orchestrator's");
    println!("UV_INDEX derivation + in-process schema codegen all key on this one tree:");
    println!(
        "  export STREAMLIB_REGISTRY_URL=\"{}\"",
        report.registry.base_url
    );
    println!();
    println!("pypi (uv) — the orchestrator derives this from the seed above; for a direct");
    println!("`uv` invocation set it explicitly:");
    println!("  export UV_INDEX=\"{}\"", report.pypi_index_url);
    println!();
    if report.npm_needs_serve {
        println!("npm — a local `file://` tree has no npm registry story; serve it:");
        println!(
            "  streamlib registry serve {tree_ref}   # serves npm on localhost + writes .npmrc"
        );
    } else {
        println!("npm — add to .npmrc:");
        println!("  @tatolab:registry={}", report.npm_registry_url);
    }
    Ok(())
}

/// `streamlib registry serve <tree> [--port N]` — serve a local registry tree
/// over a dumb localhost static mount for npm (`@tatolab`), write the `.npmrc`
/// scope so it isn't hand-edited, and block until Ctrl-C.
pub fn serve(tree_dir: &Path, port: Option<u16>) -> Result<()> {
    let consumer_root = std::env::current_dir().context("resolve current working directory")?;
    let mut handle = registry::serve_registry(tree_dir, &ServeRegistryOptions { port })?;
    let npmrc = registry::write_npmrc_scope(&consumer_root, &handle.npm_scope_line)
        .context("write .npmrc npm scope")?;

    println!("Serving {} at {}", tree_dir.display(), handle.base_url);
    println!("  npm scope written to {}:", npmrc.display());
    println!("    {}", handle.npm_scope_line);
    println!();
    println!("cargo / pypi / .slpkg resolve serverless (local-registry + file://) —");
    println!("this server is npm-only. Ctrl-C to stop.");

    // Block for the serve session; Ctrl-C reaches the child via the foreground
    // process group. The handle's Drop kills the child on any early return.
    let _ = handle.wait();
    Ok(())
}
