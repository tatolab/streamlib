// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! macOS XPC-based subprocess RHI implementation.
//!
//! This module provides zero-copy cross-process frame transport using:
//! - XPC anonymous listeners for direct runtime-subprocess connections
//! - IOSurface XPC for GPU frame sharing (VideoFrame)
//! - xpc_shmem for CPU frame sharing (AudioFrame/DataFrame)
//!
//! ## Automatic Broker Management
//!
//! The XPC broker is automatically managed - when any streamlib app starts,
//! it checks if it was launched by launchd as the broker service. If so,
//! it runs the broker and never returns to normal app execution.
//!
//! This is completely transparent to users - they never need to know about
//! the broker or configure anything.

mod block_helpers;
mod broker_grpc_service;
mod broker_state;
pub(crate) mod proto;
mod xpc_broker;
mod xpc_channel;
mod xpc_frame_transport;

#[cfg(test)]
mod tests;

pub use xpc_broker::{XpcBroker, XpcBrokerListener, BROKER_SERVICE_NAME};
pub use xpc_channel::XpcChannel;
pub use xpc_frame_transport::{release_frame_transport_handle, XpcFrameTransport};

use std::sync::Arc;

/// Argument flag that launchd uses to start the broker process.
const BROKER_ARG: &str = "--subprocess-broker";

/// Global constructor that runs before main().
///
/// If this process was started by launchd as the broker service,
/// runs the broker and exits. Otherwise, returns to normal execution.
#[ctor::ctor]
fn check_and_run_broker() {
    // Check if we were launched as the broker
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|arg| arg == BROKER_ARG) {
        // We are the broker process - run broker and never return
        run_broker_service();
    }
}

/// Default gRPC port for broker diagnostics.
const GRPC_PORT: u16 = 50051;

/// Run the XPC broker service.
///
/// This function never returns - it runs the broker indefinitely.
fn run_broker_service() -> ! {
    // Initialize minimal logging for broker (ignore if already initialized)
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    tracing::info!(
        "[Broker] Starting StreamLib XPC broker service (PID: {})",
        std::process::id()
    );

    // Create shared state for diagnostics
    let state = broker_state::BrokerState::new();

    // Start gRPC server in a separate thread
    let grpc_state = state.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime for gRPC");

        rt.block_on(async {
            if let Err(e) = start_grpc_server(grpc_state).await {
                tracing::error!("[Broker] gRPC server error: {}", e);
            }
        });
    });

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

/// Start the gRPC diagnostics server.
async fn start_grpc_server(
    state: broker_state::BrokerState,
) -> Result<(), Box<dyn std::error::Error>> {
    use proto::broker_service_server::BrokerServiceServer;
    use tonic::transport::Server;

    let addr = format!("127.0.0.1:{}", GRPC_PORT).parse()?;
    let service = broker_grpc_service::BrokerGrpcService::new(state);

    tracing::info!("[Broker] Starting gRPC server on {}", addr);

    Server::builder()
        .add_service(BrokerServiceServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
