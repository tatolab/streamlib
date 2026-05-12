// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/api-server` — HTTP control-plane processor for streamlib
//! runtimes.
//!
//! Exposes graph mutation, registry browsing, schema catalog, WebSocket
//! event streaming, and an auto-generated OpenAPI spec.

pub mod _generated_;

mod handlers;
mod processor;
mod state;

pub use _generated_::ApiServerConfig;
pub use processor::ApiServerProcessor;
