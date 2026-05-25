// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Minimal API Server Example
//!
//! Launches the API server processor and waits for shutdown signal (Ctrl+C).
//! Use this for testing the command and control web interface.
//!
//! The server runs on http://127.0.0.1:9000 with endpoints:
//! - GET /health - Health check
//! - GET /api/registry - List available processor types
//! - GET /api/graph - Get current runtime graph
//! - POST /api/processor - Create a processor
//! - DELETE /api/processors/:id - Remove a processor
//! - POST /api/connections - Create a connection
//! - DELETE /api/connections/:id - Remove a connection
//! - WS /ws/events - WebSocket event stream
//!
//! Run prerequisite: `cargo xtask build-plugins --package @tatolab/api-server`
//! so the runtime can find the staged cdylib at
//! `target/streamlib-plugins/tatolab__api-server/`.

use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;

#[tokio::main]
async fn main() -> streamlib::sdk::error::Result<()> {
    let runtime = Runner::new()?;

    runtime.load_workspace_packages(["@tatolab/api-server"])?;

    let config = serde_json::json!({
        "host": "127.0.0.1",
        "port": 9000,
    });
    runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "api-server", "ApiServer", "1.0.0"),
        config,
    ))?;
    runtime.start()?;

    println!("API server running at http://127.0.0.1:9000");
    println!("WebSocket events at ws://127.0.0.1:9000/ws/events");
    println!("Press Ctrl+C to stop");

    runtime.wait_for_signal()?;

    Ok(())
}
