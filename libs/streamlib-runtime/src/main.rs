// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! StreamLib Runtime Binary
//!
//! Standalone process that hosts a StreamLib runtime with API server.
//! Spawned by the `streamlib run` CLI command (kubectl model).
//!
//! Boots as bare engine substrate: `Runner::with_auto_build()` starts an
//! empty registry, then the core module set (the API server) is loaded
//! through the all-dynamic module loader via [`Runner::add_module_with`]
//! against the package source on disk. There is no `dlopen` plugin loader
//! — third-party processors arrive the same way, as `.slpkg` / source
//! modules loaded at runtime.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Parser;
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{BuildPolicy, Runner, Strategy};
use streamlib::sdk::schema_ident;
use streamlib::sdk::RunnerAutoBuild;

// ---------------------------------------------------------------------------
// CLI arguments
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "streamlib-runtime")]
#[command(author, version, about = "StreamLib runtime process", long_about = None)]
struct Args {
    /// Host address to bind to
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port for the API server
    #[arg(short, long, default_value = "9000")]
    port: u16,

    /// Runtime name (auto-generated if not specified)
    #[arg(long)]
    name: Option<String>,

    /// Pipeline graph snapshot to load (JSON)
    #[arg(long = "snapshot", value_name = "PATH")]
    snapshot: Option<PathBuf>,

    /// Run as a background daemon
    #[arg(short = 'd', long)]
    daemon: bool,
}

// ---------------------------------------------------------------------------
// Name generation (duplicated from CLI serve.rs)
// ---------------------------------------------------------------------------

const ADJECTIVES: &[&str] = &[
    "admiring",
    "brave",
    "clever",
    "dazzling",
    "eager",
    "fancy",
    "graceful",
    "happy",
    "inspiring",
    "jolly",
    "keen",
    "lively",
    "merry",
    "noble",
    "optimistic",
    "peaceful",
    "quirky",
    "radiant",
    "serene",
    "trusting",
    "upbeat",
    "vibrant",
    "witty",
    "xenial",
    "youthful",
    "zealous",
];

const NOUNS: &[&str] = &[
    "albatross",
    "beaver",
    "cheetah",
    "dolphin",
    "eagle",
    "falcon",
    "gazelle",
    "hawk",
    "ibis",
    "jaguar",
    "koala",
    "leopard",
    "meerkat",
    "nightingale",
    "otter",
    "panther",
    "quail",
    "raven",
    "sparrow",
    "tiger",
    "urchin",
    "viper",
    "walrus",
    "xerus",
    "yak",
    "zebra",
];

fn generate_runtime_name() -> String {
    let adj = ADJECTIVES[fastrand::usize(..ADJECTIVES.len())];
    let noun = NOUNS[fastrand::usize(..NOUNS.len())];
    format!("{}-{}", adj, noun)
}

// ---------------------------------------------------------------------------
// Core package source resolution
// ---------------------------------------------------------------------------

/// Resolve the directory holding core package sources (the `packages/`
/// dir alongside the runtime). The install / clone is collocated: the
/// runtime binary and the package sources share a root, so the runtime
/// loads its core modules from `<root>/packages/<name>` and rebuilds
/// them from source when they change on disk.
///
/// Resolution order:
/// 1. `STREAMLIB_PACKAGES_DIR` (explicit override — tests, custom deploys)
/// 2. Walk up from the running binary to the first ancestor containing a
///    `packages/api-server/streamlib.yaml` (the dev workspace root under
///    `cargo run`, the install prefix once packaged).
fn resolve_packages_dir() -> Result<PathBuf> {
    fn is_packages_dir(dir: &std::path::Path) -> bool {
        dir.join("api-server").join("streamlib.yaml").exists()
    }

    if let Ok(dir) = std::env::var("STREAMLIB_PACKAGES_DIR") {
        let candidate = PathBuf::from(dir);
        if is_packages_dir(&candidate) {
            return Ok(candidate);
        }
        bail!(
            "STREAMLIB_PACKAGES_DIR={} does not contain api-server/streamlib.yaml",
            candidate.display()
        );
    }

    let exe = std::env::current_exe().context("could not determine the running executable path")?;
    let mut ancestor = exe.parent();
    while let Some(dir) = ancestor {
        let candidate = dir.join("packages");
        if is_packages_dir(&candidate) {
            return Ok(candidate);
        }
        ancestor = dir.parent();
    }

    bail!(
        "could not locate the streamlib `packages/` directory.\n\
         Searched STREAMLIB_PACKAGES_DIR and every ancestor of {} for \
         packages/api-server/streamlib.yaml.\n\
         Set STREAMLIB_PACKAGES_DIR to the directory containing the core \
         package sources.",
        exe.display()
    )
}

// ---------------------------------------------------------------------------
// Daemonization
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn daemonize_if_requested(name: &str, port: u16, host: &str) -> Result<()> {
    use daemonize::Daemonize;

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let logs_dir = home.join(".streamlib").join("logs");
    let pids_dir = home.join(".streamlib").join("pids");

    std::fs::create_dir_all(&logs_dir)?;
    std::fs::create_dir_all(&pids_dir)?;

    let pid_path = pids_dir.join(format!("{}.pid", name));

    println!("runtime/{} started", name);
    println!("  API: http://{}:{}", host, port);
    println!();
    println!("Next steps:");
    println!("  streamlib logs -r {} -f", name);
    println!("  streamlib runtimes list");

    let daemonize = Daemonize::new()
        .pid_file(&pid_path)
        .working_directory(std::env::current_dir()?);

    daemonize.start().context("Failed to daemonize")?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let args = Args::parse();

    // Handle daemon mode BEFORE creating tokio runtime
    #[cfg(unix)]
    if args.daemon {
        let runtime_name = args.name.clone().unwrap_or_else(generate_runtime_name);
        // SAFETY: set_var is called before the tokio runtime starts (single-threaded
        // context, no concurrent reads of env). Edition 2024 requires the explicit block.
        unsafe { std::env::set_var("_STREAMLIB_DAEMON_NAME", &runtime_name) };
        daemonize_if_requested(&runtime_name, args.port, &args.host)?;
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(args))
}

async fn run(args: Args) -> Result<()> {
    let runtime_name = if args.daemon {
        std::env::var("_STREAMLIB_DAEMON_NAME")
            .ok()
            .or(args.name)
            .unwrap_or_else(generate_runtime_name)
    } else {
        args.name.unwrap_or_else(generate_runtime_name)
    };

    // Set runtime ID env var BEFORE creating runtime. Runner::with_auto_build
    // picks it up via RuntimeUniqueId::from_env_or_generate and owns the
    // JSONL log file going forward.
    let runtime_id = format!("R{}", cuid2::create_id());
    // SAFETY: early init, before processor threads spawn; no concurrent env reads.
    unsafe { std::env::set_var("STREAMLIB_RUNTIME_ID", &runtime_id) };
    // In daemon mode stdout is about to be closed; ask the logging
    // pathway to skip the pretty mirror so no records are lost to a
    // dev/null sink. JSONL keeps writing regardless.
    if args.daemon {
        unsafe { std::env::set_var("STREAMLIB_QUIET", "1") };
    }

    tracing::info!("Starting runtime: {} ({})", runtime_name, runtime_id);

    // Bare engine substrate with an injected build orchestrator so core
    // modules can be built from source on demand. Starts with an empty
    // registry — every processor / schema arrives through the all-dynamic
    // module loader.
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
    tracing::info!(
        log_path = log_path.as_deref().unwrap_or("(none)"),
        "runtime JSONL log path"
    );

    let mut api_config = serde_json::Map::new();
    api_config.insert("host".into(), serde_json::Value::from(args.host.clone()));
    api_config.insert("port".into(), serde_json::Value::from(args.port));
    api_config.insert("name".into(), serde_json::Value::from(runtime_name.clone()));
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
        println!("Empty graph ready - use API to add processors");
    }

    if !args.daemon {
        println!("Press Ctrl+C to stop");
    }

    runtime.wait_for_signal()?;

    Ok(())
}
