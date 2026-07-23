// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

mod auth;
mod handlers;
mod mcp;
pub mod node_registry;
mod ops;
mod processor;
mod state;

pub use _generated_::ApiServerConfig;
pub use mcp::serve_stdio_jsonrpc;
pub use node_registry::{
    NODE_REGISTRY_SCHEMA_VERSION, NodeRegistryEntry, NodeRegistryError, read_entry, registry_dir,
    remove_entry, scan_entries, write_entry,
};
pub use processor::ApiServerProcessor;
