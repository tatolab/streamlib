// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;

use anyhow::Result;
use streamlib::{ApiServerConfig, ApiServerProcessor, StreamRuntime};
use tracing_appender::non_blocking::WorkerGuard;

use crate::plugin_loader::PluginLoader;

/// Docker-style adjectives for runtime name generation.
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

/// Docker-style nouns for runtime name generation.
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

/// Generate a Docker-style random name (adjective-noun).
pub fn generate_runtime_name() -> String {
    let adj = ADJECTIVES[fastrand::usize(..ADJECTIVES.len())];
    let noun = NOUNS[fastrand::usize(..NOUNS.len())];
    format!("{}-{}", adj, noun)
}

/// Get the streamlib logs directory (~/.streamlib/logs).
fn get_logs_dir() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    Ok(home.join(".streamlib").join("logs"))
}

/// Set up file-based logging and return the guard (must be kept alive).
/// When `daemon` is true, only logs to file (no stdout).
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

    // Optional stdout layer - None in daemon mode
    let stdout_layer = (!daemon).then(tracing_subscriber::fmt::layer);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    Ok(guard)
}

/// Start a StreamLib runtime.
pub async fn run(
    host: String,
    port: u16,
    graph_file: Option<PathBuf>,
    plugins: Vec<PathBuf>,
    plugin_dir: Option<PathBuf>,
    name: Option<String>,
    daemon: bool,
) -> Result<()> {
    // Generate or use provided runtime name
    let runtime_name = name.unwrap_or_else(generate_runtime_name);

    let log_path = get_logs_dir()?.join(format!("{}.log", runtime_name));

    // Set runtime ID env var BEFORE creating runtime
    // StreamRuntime::new() reads STREAMLIB_RUNTIME_ID from env
    let runtime_id = format!("R{}", cuid2::create_id());
    std::env::set_var("STREAMLIB_RUNTIME_ID", &runtime_id);

    // Set up file-based logging (daemon mode skips stdout)
    let _log_guard = setup_file_logging(&runtime_name, daemon)?;

    tracing::info!("Starting runtime: {} ({})", runtime_name, runtime_id);
    tracing::info!("Log file: {}", log_path.display());

    // Load plugins BEFORE creating runtime (registers processors in global registry)
    let mut loader = PluginLoader::new();

    // Load individual plugins
    for plugin_path in &plugins {
        println!("Loading plugin: {}", plugin_path.display());
        let count = loader.load_plugin(plugin_path)?;
        println!("  Registered {} processor(s)", count);
    }

    // Load all plugins from directory
    if let Some(ref dir) = plugin_dir {
        println!("Loading plugins from: {}", dir.display());
        let count = loader.load_plugin_dir(dir)?;
        println!("  Registered {} processor(s) total", count);
    }

    let runtime = StreamRuntime::new()?;

    // Add API server with name and log_path for broker registration
    let config = ApiServerConfig {
        host: host.clone(),
        port,
        name: Some(runtime_name.clone()),
        log_path: Some(log_path.clone()),
    };
    runtime.add_processor(ApiServerProcessor::node(config))?;

    // Load graph file if provided
    if let Some(ref path) = graph_file {
        println!("Loading pipeline: {}", path.display());
        runtime.load_graph_file_path(path)?;
    }

    runtime.start()?;

    if graph_file.is_none() {
        println!("Empty graph ready - use API to add processors");
    }

    if !daemon {
        println!("Press Ctrl+C to stop");
    }

    runtime.wait_for_signal()?;

    // Keep loader alive until runtime stops (libraries must remain loaded)
    drop(loader);

    Ok(())
}
