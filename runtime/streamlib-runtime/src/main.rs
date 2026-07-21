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
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;

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
    // `PROCESSOR_REGISTRY`. This registers the `ApiServer` processor type;
    // the instance is added below.
    PROCESSOR_REGISTRY.register::<streamlib_api_server::ApiServerProcessor::Processor>();

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
        // Resolving variant: pull + build any referenced package from the
        // registry so a snapshot is self-contained (the runtime only
        // registers the api-server in-process at boot).
        runtime
            .load_graph_snapshot_from_path_with_resolving(path)
            .await?;
    }

    runtime.start()?;

    if args.snapshot.is_none() {
        println!("Empty graph ready — use the API to add processors");
    }
    println!("Press Ctrl+C to stop");

    runtime.wait_for_signal()?;

    Ok(())
}
