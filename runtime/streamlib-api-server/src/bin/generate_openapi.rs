// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// A build-time codegen binary run by hand or by `schemas.yml`, never inside a
// runtime: it has no `tracing` subscriber to emit through, and its one line of
// output tells the operator where the artifact landed. The engine's
// `lint-logging` gate honours this attribute as its sanctioned allowance.
#![allow(clippy::disallowed_macros)]

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
    fs::create_dir_all(schema_dir).expect("Failed to create schema directory");

    let openapi = streamlib_api_server::control_plane_openapi_spec();
    let openapi_json =
        serde_json::to_string_pretty(&openapi).expect("Failed to serialize OpenAPI spec");
    let openapi_path = schema_dir.join("openapi.json");
    fs::write(&openapi_path, &openapi_json).expect("Failed to write OpenAPI spec");
    println!("Generated: {}", openapi_path.display());
}
