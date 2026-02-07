// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! StreamLib Runtime Binary
//!
//! Standalone process that hosts a StreamLib runtime with API server.
//! Spawned by the `streamlib run` CLI command (kubectl model).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use libloading::Library;
use streamlib::core::processors::PROCESSOR_REGISTRY;
use streamlib::{ApiServerConfig, ApiServerProcessor, StreamRuntime};
use streamlib_plugin_abi::{PluginDeclaration, STREAMLIB_ABI_VERSION};
use tracing_appender::non_blocking::WorkerGuard;

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

    /// Pipeline graph file to load (JSON)
    #[arg(long = "graph-file", value_name = "PATH")]
    graph_file: Option<PathBuf>,

    /// Plugin libraries to load (can be specified multiple times)
    #[arg(long = "plugin", value_name = "PATH")]
    plugins: Vec<PathBuf>,

    /// Directory containing plugin libraries
    #[arg(long = "plugin-dir", value_name = "DIR")]
    plugin_dir: Option<PathBuf>,

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
// Logging
// ---------------------------------------------------------------------------

fn get_logs_dir() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    Ok(home.join(".streamlib").join("logs"))
}

fn setup_file_logging(runtime_name: &str, daemon: bool) -> Result<WorkerGuard> {
    use tracing_subscriber::prelude::*;

    let logs_dir = get_logs_dir()?;
    std::fs::create_dir_all(&logs_dir)?;

    let file_appender =
        tracing_appender::rolling::never(&logs_dir, format!("{}.log", runtime_name));
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".parse().unwrap());

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false);

    let stdout_layer = (!daemon).then(tracing_subscriber::fmt::layer);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    Ok(guard)
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
// Plugin loading (duplicated from CLI plugin_loader.rs)
// ---------------------------------------------------------------------------

struct PluginLoader {
    loaded_libraries: Vec<Library>,
}

impl PluginLoader {
    fn new() -> Self {
        Self {
            loaded_libraries: Vec::new(),
        }
    }

    fn load_plugin(&mut self, path: &Path) -> Result<usize> {
        let lib = unsafe {
            Library::new(path)
                .with_context(|| format!("Failed to load plugin library: {}", path.display()))?
        };

        let decl: &PluginDeclaration = unsafe {
            let symbol = lib
                .get::<*const PluginDeclaration>(b"STREAMLIB_PLUGIN\0")
                .with_context(|| {
                    format!(
                        "Plugin '{}' missing STREAMLIB_PLUGIN symbol. \
                         Ensure the plugin uses the export_plugin! macro.",
                        path.display()
                    )
                })?;
            &**symbol
        };

        if decl.abi_version != STREAMLIB_ABI_VERSION {
            return Err(anyhow!(
                "ABI version mismatch for '{}': plugin has v{}, runtime expects v{}. \
                 Rebuild the plugin with a compatible streamlib-plugin-abi version.",
                path.display(),
                decl.abi_version,
                STREAMLIB_ABI_VERSION
            ));
        }

        let before_count = PROCESSOR_REGISTRY.list_registered().len();
        (decl.register)(&PROCESSOR_REGISTRY);
        let after_count = PROCESSOR_REGISTRY.list_registered().len();
        let registered_count = after_count - before_count;

        self.loaded_libraries.push(lib);

        Ok(registered_count)
    }

    fn load_plugin_dir(&mut self, dir: &Path) -> Result<usize> {
        let mut total_registered = 0;

        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("Failed to read plugin directory: {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if is_plugin_library(&path) {
                match self.load_plugin(&path) {
                    Ok(count) => {
                        tracing::info!(
                            "Loaded plugin '{}': {} processor(s) registered",
                            path.display(),
                            count
                        );
                        total_registered += count;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to load plugin '{}': {}", path.display(), e);
                    }
                }
            }
        }

        Ok(total_registered)
    }
}

fn is_plugin_library(path: &Path) -> bool {
    let extension = path.extension().and_then(|e| e.to_str());
    match extension {
        Some("dylib") => cfg!(target_os = "macos"),
        Some("so") => cfg!(target_os = "linux"),
        Some("dll") => cfg!(target_os = "windows"),
        _ => false,
    }
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
        std::env::set_var("_STREAMLIB_DAEMON_NAME", &runtime_name);
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

    let log_path = get_logs_dir()?.join(format!("{}.log", runtime_name));

    // Set runtime ID env var BEFORE creating runtime
    let runtime_id = format!("R{}", cuid2::create_id());
    std::env::set_var("STREAMLIB_RUNTIME_ID", &runtime_id);

    // Set up file-based logging (daemon mode skips stdout)
    let _log_guard = setup_file_logging(&runtime_name, args.daemon)?;

    tracing::info!("Starting runtime: {} ({})", runtime_name, runtime_id);
    tracing::info!("Log file: {}", log_path.display());

    // Load plugins BEFORE creating runtime (registers processors in global registry)
    let mut loader = PluginLoader::new();

    for plugin_path in &args.plugins {
        println!("Loading plugin: {}", plugin_path.display());
        let count = loader.load_plugin(plugin_path)?;
        println!("  Registered {} processor(s)", count);
    }

    if let Some(ref dir) = args.plugin_dir {
        println!("Loading plugins from: {}", dir.display());
        let count = loader.load_plugin_dir(dir)?;
        println!("  Registered {} processor(s) total", count);
    }

    let runtime = StreamRuntime::new()?;

    let config = ApiServerConfig {
        host: args.host.clone(),
        port: args.port,
        name: Some(runtime_name.clone()),
        log_path: Some(log_path.to_string_lossy().into_owned()),
    };
    runtime.add_processor(ApiServerProcessor::node(config))?;

    if let Some(ref path) = args.graph_file {
        println!("Loading pipeline: {}", path.display());
        runtime.load_graph_file_path(path)?;
    }

    runtime.start()?;

    if args.graph_file.is_none() {
        println!("Empty graph ready - use API to add processors");
    }

    if !args.daemon {
        println!("Press Ctrl+C to stop");
    }

    runtime.wait_for_signal()?;

    // Keep loader alive until runtime stops (libraries must remain loaded)
    drop(loader);

    Ok(())
}
