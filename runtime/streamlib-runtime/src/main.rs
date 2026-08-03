// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! StreamLib Runtime Binary
//!
//! Bare engine substrate. `Runner::with_auto_build()` starts an empty
//! registry; the always-present control plane — the API server — is a host
//! (it drives `RuntimeOperations`, the processor registry, pubsub, and the
//! graph API), not a loadable plugin, so it is statically linked into this
//! binary and registered in-process on the shared `PROCESSOR_REGISTRY`.
//! Every other processor / schema arrives at runtime through the
//! all-dynamic module loader. Run the executable directly — there is no
//! `dlopen` plugin loader and no launcher in front of it.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::runtime::Runner;
use streamlib_api_server::control_plane_host::{
    ApiServerControlPlaneHostConfig, register_api_server_control_plane_processor_on_runtime,
};

#[derive(Parser)]
#[command(name = "streamlib-runtime")]
#[command(author, version, about = "StreamLib runtime process", long_about = None)]
struct Args {
    /// Host address to bind the API server to
    #[arg(long, default_value = "0.0.0.0")]
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
    // control plane — a host, not a loadable plugin — so it is statically
    // linked into this binary and registered in-process on the shared
    // `PROCESSOR_REGISTRY`.
    register_api_server_control_plane_processor_on_runtime(
        &runtime,
        ApiServerControlPlaneHostConfig {
            bind_host: args.host,
            bind_port: args.port,
            node_name: args.name,
        },
    )?;

    if let Some(ref path) = args.snapshot {
        println!("Loading pipeline: {}", path.display());
        // Resolving variant: pull + build any referenced package from the
        // registry so a snapshot is self-contained (the runtime only
        // registers the api-server in-process at boot).
        runtime
            .load_graph_snapshot_from_path_with_resolving(path)
            .await?;
    }

    if args.snapshot.is_none() {
        println!("Starting with an empty graph — use the API to add processors");
    }
    println!("Press Ctrl+C to stop");

    runtime.start_and_wait_for_shutdown()?;

    Ok(())
}
