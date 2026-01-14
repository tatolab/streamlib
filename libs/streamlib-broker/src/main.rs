// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! StreamLib Broker Service
//!
//! This binary runs as a launchd service on macOS, providing:
//! - XPC endpoint exchange for runtime ↔ subprocess connections
//! - gRPC interface for diagnostics and monitoring

use std::sync::Arc;

use clap::Parser;
use tracing::info;

#[cfg(target_os = "macos")]
use streamlib_broker::{
    proto::broker_service_server::BrokerServiceServer, BrokerGrpcService, BrokerState,
    XpcBrokerListener,
};

#[derive(Parser)]
#[command(name = "streamlib-broker")]
#[command(about = "StreamLib broker service for cross-process coordination")]
struct Cli {
    /// Port for the gRPC server
    #[arg(long, default_value_t = streamlib_broker::GRPC_PORT)]
    port: u16,
}

#[cfg(target_os = "macos")]
fn main() {
    let cli = Cli::parse();

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .init();

    info!(
        "[Broker] Starting StreamLib broker service v{} (PID: {})",
        streamlib_broker::VERSION,
        std::process::id()
    );

    // Create shared state for diagnostics
    let state = BrokerState::new();

    // Start gRPC server in a separate thread
    let grpc_state = state.clone();
    let grpc_port = cli.port;
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime for gRPC");

        rt.block_on(async {
            if let Err(e) = start_grpc_server(grpc_state, grpc_port).await {
                tracing::error!("[Broker] gRPC server error: {}", e);
            }
        });
    });

    // Start XPC listener (blocks forever)
    let listener = Arc::new(XpcBrokerListener::new(state));

    match listener.start_listener() {
        Ok(()) => {
            // start_listener never returns on success (infinite loop)
            unreachable!()
        }
        Err(e) => {
            tracing::error!("[Broker] Failed to start broker listener: {}", e);
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "macos")]
async fn start_grpc_server(
    state: BrokerState,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    use tonic::transport::Server;

    let addr = format!("127.0.0.1:{}", port).parse()?;
    let service = BrokerGrpcService::new(state);

    info!("[Broker] Starting gRPC server on {}", addr);

    Server::builder()
        .add_service(BrokerServiceServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("StreamLib broker is only supported on macOS.");
    eprintln!("On other platforms, broker functionality is not required.");
    std::process::exit(1);
}
