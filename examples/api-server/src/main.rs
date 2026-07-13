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
//! There is no module-loading call: `@tatolab/api-server` lives in this
//! app's `streamlib_modules/` folder (populated by `./setup.sh`) and the
//! runtime lazily discovers + loads it on the first `processor_type_ref!`
//! reference. The reference site carries no version — `processor_type_ref!`
//! resolves to the installed provider.

use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::processor_type_ref;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;

#[tokio::main]
async fn main() -> streamlib::sdk::error::Result<()> {
    let runtime = Runner::with_auto_build()?;

    let config = serde_json::json!({
        "host": "127.0.0.1",
        "port": 9000,
    });
    runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "api-server", "ApiServer"),
        config,
    ))?;
    runtime.start()?;

    println!("API server running at http://127.0.0.1:9000");
    println!("WebSocket events at ws://127.0.0.1:9000/ws/events");
    println!("Press Ctrl+C to stop");

    runtime.wait_for_signal()?;

    Ok(())
}
