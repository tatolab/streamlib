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

use streamlib::{ApiServerConfig, ApiServerProcessor, Result, StreamRuntime};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .init();

    let runtime = StreamRuntime::new()?;

    let config = ApiServerConfig {
        host: "127.0.0.1".to_string(),
        port: 9000,
    };

    runtime.add_processor(ApiServerProcessor::node(config))?;
    runtime.start()?;

    println!("API server running at http://127.0.0.1:9000");
    println!("WebSocket events at ws://127.0.0.1:9000/ws/events");
    println!("Press Ctrl+C to stop");

    runtime.wait_for_signal()?;

    Ok(())
}
