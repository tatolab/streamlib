// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Tokio Integration Example
//!
//! Demonstrates StreamRuntime integration with existing tokio applications.
//! StreamRuntime::new() auto-detects the tokio context and works seamlessly.
//!
//! Previously, calling StreamRuntime::new() from within #[tokio::main] would panic.
//! With issue #92 implemented, it now auto-detects the tokio runtime and uses
//! the current handle instead of trying to create a new runtime.

use streamlib::{Result, StreamRuntime};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .init();

    println!("=== Tokio Integration Example ===\n");

    // Just call new() - it auto-detects the tokio context!
    // No special constructor needed. This used to panic, now it works.
    let runtime = StreamRuntime::new()?;
    println!("StreamRuntime created (auto-detected tokio context)");

    // Start the runtime
    runtime.start()?;
    println!("Runtime started");

    // Demonstrate that async tokio operations work alongside StreamRuntime
    println!("Running async operations...");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Get graph state using sync method (uses spawn + channel internally)
    let graph_json = runtime.to_json()?;
    println!(
        "Graph state: {} nodes",
        graph_json["nodes"].as_array().map(|a| a.len()).unwrap_or(0)
    );

    // Shutdown
    runtime.stop()?;
    println!("Runtime stopped");

    println!("\n=== Example Complete ===");
    Ok(())
}
