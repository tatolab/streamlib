// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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

use streamlib::core::json_schema::{GraphResponse, RegistryResponse};

fn main() {
    let schema_dir = Path::new("dist/schemas");

    // Create the schema directory if it doesn't exist
    if !schema_dir.exists() {
        fs::create_dir_all(schema_dir).expect("Failed to create schema directory");
        println!("Created directory: {}", schema_dir.display());
    }

    // Generate GraphResponse schema
    let graph_schema = schema_for!(GraphResponse);
    let graph_json =
        serde_json::to_string_pretty(&graph_schema).expect("Failed to serialize schema");
    let graph_path = schema_dir.join("graph-response.schema.json");
    fs::write(&graph_path, &graph_json).expect("Failed to write graph schema");
    println!("Generated: {}", graph_path.display());

    // Generate RegistryResponse schema
    let registry_schema = schema_for!(RegistryResponse);
    let registry_json =
        serde_json::to_string_pretty(&registry_schema).expect("Failed to serialize schema");
    let registry_path = schema_dir.join("registry-response.schema.json");
    fs::write(&registry_path, &registry_json).expect("Failed to write registry schema");
    println!("Generated: {}", registry_path.display());

    println!("\nSchema generation complete!");
    println!("Files written to: {}", schema_dir.display());
}
