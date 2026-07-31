// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

mod auth;
pub mod control_plane_host;
mod handlers;
mod mcp;
pub mod node_registry;
mod ops;
mod state;

// `processors/` is the one processor-discovery root for every language and
// every crate-type. This crate is a statically-linked host rlib (plus a
// `[[bin]]`), so it keeps a committed crate root instead of the generated one a
// distributable cdylib package uses — the `#[path]` is how that committed root
// reaches the shared discovery root.
#[path = "../processors/api_server.rs"]
pub mod api_server;

pub use _generated_::ApiServerConfig;
pub use api_server::ApiServerProcessor;
pub use mcp::serve_stdio_jsonrpc;
pub use node_registry::{
    NODE_REGISTRY_SCHEMA_VERSION, NodeRegistryEntry, NodeRegistryError, read_entry, registry_dir,
    remove_entry, scan_entries, write_entry,
};
