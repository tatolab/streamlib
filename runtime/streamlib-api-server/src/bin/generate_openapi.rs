// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // codegen binary: stdout is the output channel

//! Writes the control plane's OpenAPI spec to `dist/schemas/openapi.json`.
//!
//! Run with: `cargo run -p streamlib-api-server --bin generate_openapi`.
//!
//! The spec comes from [`streamlib_api_server::control_plane_openapi_spec`] —
//! the same route registrations the live router installs. This binary declares
//! no paths of its own: it once did, and the copy drifted from the server it
//! claimed to describe, which is invisible until a generated client calls a
//! route that does not exist.

use std::fs;
use std::path::Path;

fn main() {
    let schema_dir = Path::new("dist/schemas");
    if !schema_dir.exists() {
        fs::create_dir_all(schema_dir).expect("Failed to create schema directory");
        println!("Created directory: {}", schema_dir.display());
    }

    let openapi = streamlib_api_server::control_plane_openapi_spec();
    let openapi_json =
        serde_json::to_string_pretty(&openapi).expect("Failed to serialize OpenAPI spec");
    let openapi_path = schema_dir.join("openapi.json");
    fs::write(&openapi_path, &openapi_json).expect("Failed to write OpenAPI spec");
    println!("Generated: {}", openapi_path.display());
}
