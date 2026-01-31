// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Schema management commands.

use anyhow::Result;
use std::path::Path;
use streamlib_codegen_shared::parse_processor_yaml_file;

/// Validate a processor YAML schema file.
pub fn validate_processor(path: &Path) -> Result<()> {
    println!("Validating processor schema: {}", path.display());

    match parse_processor_yaml_file(path) {
        Ok(schema) => {
            println!();
            println!("  Name:        {}", schema.name);
            println!("  Version:     {}", schema.version);
            if let Some(desc) = &schema.description {
                println!("  Description: {}", desc);
            }
            println!("  Runtime:     {:?}", schema.runtime.language);
            if schema.runtime.options.unsafe_send {
                println!("  Options:     unsafe_send=true");
            }
            println!("  Execution:   {:?}", schema.execution);
            if let Some(config) = &schema.config {
                println!("  Config:      {} (schema: {})", config.name, config.schema);
            }
            if !schema.inputs.is_empty() {
                println!("  Inputs:");
                for input in &schema.inputs {
                    println!("    - {} ({})", input.name, input.schema);
                }
            }
            if !schema.outputs.is_empty() {
                println!("  Outputs:");
                for output in &schema.outputs {
                    println!("    - {} ({})", output.name, output.schema);
                }
            }
            println!();
            println!("Processor schema is valid.");
            Ok(())
        }
        Err(e) => {
            println!();
            anyhow::bail!("Validation failed: {}", e);
        }
    }
}
