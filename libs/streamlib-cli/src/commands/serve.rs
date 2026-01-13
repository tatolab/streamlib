// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;

use anyhow::Result;
use streamlib::{ApiServerConfig, ApiServerProcessor, StreamRuntime};

use crate::plugin_loader::PluginLoader;

// Force linkage of streamlib-python to ensure Python processors are registered via inventory
extern crate streamlib_python;

/// Start a StreamLib runtime.
pub async fn run(
    host: String,
    port: u16,
    no_api: bool,
    graph_file: Option<PathBuf>,
    plugins: Vec<PathBuf>,
    plugin_dir: Option<PathBuf>,
) -> Result<()> {
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

    // Add API server unless opted out
    if !no_api {
        let config = ApiServerConfig {
            host: host.clone(),
            port,
        };

        runtime.add_processor(ApiServerProcessor::node(config))?;

        println!("API server: http://{}:{}", host, port);
    }

    // Load graph file if provided
    if let Some(ref path) = graph_file {
        println!("Loading pipeline: {}", path.display());
        runtime.load_graph_file_path(path)?;
    }

    runtime.start()?;

    if graph_file.is_none() && !no_api {
        println!("Empty graph ready - use API to add processors");
    }

    println!("Press Ctrl+C to stop");

    runtime.wait_for_signal()?;

    // Keep loader alive until runtime stops (libraries must remain loaded)
    drop(loader);

    Ok(())
}
