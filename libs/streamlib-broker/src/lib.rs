// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! StreamLib Broker - Centralized runtime coordination service.
//!
//! This crate provides the broker service that enables runtime tracking
//! and diagnostics via gRPC interface.

#[cfg(target_os = "macos")]
mod grpc_service;
#[cfg(target_os = "macos")]
mod state;
#[cfg(target_os = "macos")]
mod xpc_ffi;
#[cfg(target_os = "macos")]
mod xpc_service;

/// Protocol buffer types for gRPC service.
pub mod proto;

// Re-export for CLI and other consumers
#[cfg(target_os = "macos")]
pub use grpc_service::{BrokerGrpcService, PROTOCOL_VERSION};
#[cfg(target_os = "macos")]
pub use state::BrokerState;
#[cfg(target_os = "macos")]
pub use xpc_service::XpcSurfaceService;

/// Default gRPC port for broker diagnostics.
pub const GRPC_PORT: u16 = 50051;

/// Broker version from Cargo.toml.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
