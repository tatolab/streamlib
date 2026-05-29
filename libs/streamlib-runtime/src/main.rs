// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! StreamLib Runtime Binary
//!
//! Bare engine substrate. `Runner::with_auto_build()` starts an empty
//! registry; the core module set (the API server) is loaded through the
//! all-dynamic module loader via [`Runner::add_module_with`] against the
//! package source on disk, building it from source when it changes. Run
//! the executable directly — there is no `dlopen` plugin loader and no
//! launcher in front of it.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Parser;
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{BuildPolicy, Runner, Strategy};
use streamlib::sdk::schema_ident;
use streamlib::sdk::RunnerAutoBuild;

#[derive(Parser)]
#[command(name = "streamlib-runtime")]
#[command(author, version, about = "StreamLib runtime process", long_about = None)]
struct Args {
    /// Host address to bind the API server to
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port for the API server
    #[arg(short, long, default_value = "9000")]
    port: u16,

    /// Runtime name (used for surface-share registration; auto-generated
    /// by the API server when omitted)
    #[arg(long)]
    name: Option<String>,

    /// Pipeline graph snapshot to load (JSON)
    #[arg(long = "snapshot", value_name = "PATH")]
    snapshot: Option<PathBuf>,
}

/// Resolve the `packages/` directory that ships the core module sources,
/// independent of where the binary is invoked from. The binary sits
/// alongside `packages/` in both supported layouts — a cargo workspace
/// (`<workspace>/target/<profile>/streamlib-runtime`) and an installed
/// app folder (`<prefix>/bin/streamlib-runtime`, reached through a PATH
/// symlink that `current_exe()` resolves to its real location) — so
/// walking up from the binary to the first ancestor containing a
/// `packages/` directory finds the root either way. `STREAMLIB_PACKAGES_DIR`
/// overrides the search. Whether a given module exists under the resolved
/// directory is the module loader's concern, not this function's.
fn resolve_packages_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("STREAMLIB_PACKAGES_DIR") {
        return Ok(PathBuf::from(dir));
    }

    let exe = std::env::current_exe().context("could not determine the running executable path")?;
    let mut ancestor = exe.parent();
    while let Some(dir) = ancestor {
        let candidate = dir.join("packages");
        if candidate.is_dir() {
            return Ok(candidate);
        }
        ancestor = dir.parent();
    }

    bail!(
        "could not locate the streamlib `packages/` directory by walking up from {}. \
         Set STREAMLIB_PACKAGES_DIR to the install root's packages directory.",
        exe.display()
    )
}

fn main() -> Result<()> {
    let args = Args::parse();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(args))
}

async fn run(args: Args) -> Result<()> {
    // Stamp the runtime ID before the runtime is built; `Runner` picks it
    // up via `RuntimeUniqueId::from_env_or_generate` and owns the JSONL log
    // file from there.
    let runtime_id = format!("R{}", cuid2::create_id());
    // SAFETY: early init, before processor threads spawn; no concurrent env reads.
    unsafe { std::env::set_var("STREAMLIB_RUNTIME_ID", &runtime_id) };

    tracing::info!("Starting runtime ({runtime_id})");

    // Bare engine substrate with an injected build orchestrator so core
    // modules can be built from source on demand. Starts empty — every
    // processor / schema arrives through the all-dynamic module loader.
    let runtime = Runner::with_auto_build()?;

    // Seed the core module set. The API server is the always-present
    // control plane; load it from its package source on disk so the
    // runtime stays in sync with the filesystem (build-if-stale rebuilds
    // it when the package changes, caching the staged artifact).
    let packages_dir = resolve_packages_dir()?;
    runtime
        .add_module_with(
            module_ident_any_version!("tatolab", "api-server"),
            Strategy::Path {
                path: packages_dir.join("api-server"),
                build: BuildPolicy::IfStale,
            },
        )
        .await?;

    let log_path = runtime
        .jsonl_log_path()
        .map(|p| p.to_string_lossy().into_owned());

    let mut api_config = serde_json::Map::new();
    api_config.insert("host".into(), serde_json::Value::from(args.host.clone()));
    api_config.insert("port".into(), serde_json::Value::from(args.port));
    if let Some(name) = args.name {
        api_config.insert("name".into(), serde_json::Value::from(name));
    }
    if let Some(path) = log_path {
        api_config.insert("log_path".into(), serde_json::Value::from(path));
    }
    runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "api-server", "ApiServer", "1.0.0"),
        serde_json::Value::Object(api_config),
    ))?;

    if let Some(ref path) = args.snapshot {
        println!("Loading pipeline: {}", path.display());
        runtime.load_graph_snapshot_from_path(path)?;
    }

    runtime.start()?;

    if args.snapshot.is_none() {
        println!("Empty graph ready — use the API to add processors");
    }
    println!("Press Ctrl+C to stop");

    runtime.wait_for_signal()?;

    Ok(())
}
