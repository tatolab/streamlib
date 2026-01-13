// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! StreamLib Broker - Cross-process coordination service for macOS.
//!
//! This crate provides the broker service that enables runtime and subprocess
//! endpoint exchange via XPC, plus a gRPC interface for diagnostics.

#[cfg(target_os = "macos")]
mod block_helpers;
#[cfg(target_os = "macos")]
mod grpc_service;
#[cfg(target_os = "macos")]
mod state;
#[cfg(target_os = "macos")]
mod xpc_listener;

/// Protocol buffer types for gRPC service.
pub mod proto;

// Re-export for CLI and other consumers
#[cfg(target_os = "macos")]
pub use grpc_service::{BrokerGrpcService, PROTOCOL_VERSION};
#[cfg(target_os = "macos")]
pub use state::BrokerState;
#[cfg(target_os = "macos")]
pub use xpc_listener::{XpcBrokerListener, BROKER_SERVICE_NAME};

/// Default gRPC port for broker diagnostics.
pub const GRPC_PORT: u16 = 50051;

/// Broker version from Cargo.toml.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
