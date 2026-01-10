// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;

use anyhow::Result;
use streamlib::{ApiServerConfig, ApiServerProcessor, StreamRuntime};

// Force linkage of streamlib-python to ensure Python processors are registered via inventory
extern crate streamlib_python;

/// Start a StreamLib runtime.
pub async fn run(host: String, port: u16, no_api: bool, graph_file: Option<PathBuf>) -> Result<()> {
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

    Ok(())
}
