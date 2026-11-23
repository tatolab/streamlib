// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// A build-time codegen binary run by hand or by `control-plane-client.yml`, never inside a
// runtime: it has no `tracing` subscriber to emit through, and its one line of
// output tells the operator where the artifact landed. The `no-print-macro`
// ast-grep rule honours this attribute as its sanctioned allowance.
#![allow(
    clippy::disallowed_macros,
    reason = "a codegen binary: stdout and stderr tell the operator where the spec landed"
)]

//! Writes the REST description of the control operations to `dist/schemas/openapi.json`.
//!
//! Run with: `cargo run -p streamlib-control-contract --bin generate_openapi`.
//!
//! The description is [`streamlib_control_contract::control_rest_openapi_document`], the one the
//! runtime serves at `/api/openapi.json`, generated from the control contract.

use std::fs;
use std::path::Path;

fn main() {
    let schema_dir = Path::new("dist/schemas");
    fs::create_dir_all(schema_dir).expect("Failed to create schema directory");

    let openapi = streamlib_control_contract::control_rest_openapi_document();
    let openapi_json =
        serde_json::to_string_pretty(&openapi).expect("Failed to serialize OpenAPI spec");
    let openapi_path = schema_dir.join("openapi.json");
    fs::write(&openapi_path, &openapi_json).expect("Failed to write OpenAPI spec");
    println!("Generated: {}", openapi_path.display());
}
