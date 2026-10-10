// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // codegen binary: stdout is the output channel

//! Generates JSON Schema files for streamlib API responses.
//!
//! Run with: `cargo run --bin generate_schemas`
//!
//! This generates schema files in `dist/schemas/` that can be used for:
//! - API documentation
//! - Client code generation
//! - Runtime validation
//! - Web UI development

use schemars::schema_for;
use std::fs;
use std::path::Path;

use streamlib_engine::core::json_schema::{
    GraphResponse, MachineWideGraphResponse, MachineWideRegistryResponse, RegistryResponse,
};

fn main() {
    let schema_dir = Path::new("dist/schemas");

    // Create the schema directory if it doesn't exist
    if !schema_dir.exists() {
        fs::create_dir_all(schema_dir).expect("Failed to create schema directory");
        println!("Created directory: {}", schema_dir.display());
    }

    for (schema_file_name, schema) in [
        ("graph-response.schema.json", schema_for!(GraphResponse)),
        (
            "machine-wide-graph-response.schema.json",
            schema_for!(MachineWideGraphResponse),
        ),
        (
            "registry-response.schema.json",
            schema_for!(RegistryResponse),
        ),
        (
            "machine-wide-registry-response.schema.json",
            schema_for!(MachineWideRegistryResponse),
        ),
    ] {
        let schema_json =
            serde_json::to_string_pretty(&schema).expect("Failed to serialize schema");
        let schema_path = schema_dir.join(schema_file_name);
        fs::write(&schema_path, &schema_json).expect("Failed to write schema");
        println!("Generated: {}", schema_path.display());
    }

    println!("\nSchema generation complete!");
    println!("Files written to: {}", schema_dir.display());
}
