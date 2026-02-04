// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Schema management commands.

use anyhow::Result;
use std::path::Path;
use streamlib::PROCESSOR_REGISTRY;
use streamlib_codegen_shared::parse_processor_yaml_file;

/// List all known schemas from the processor registry.
pub fn list() -> Result<()> {
    let schemas = PROCESSOR_REGISTRY.known_schemas();

    if schemas.is_empty() {
        println!("No schemas found in the processor registry.");
        return Ok(());
    }

    // Build a map: schema -> list of (processor_name, direction)
    let descriptors = PROCESSOR_REGISTRY.list_registered();
    let mut schema_usage: std::collections::BTreeMap<String, Vec<(String, &str)>> =
        std::collections::BTreeMap::new();

    for descriptor in &descriptors {
        for input in &descriptor.inputs {
            schema_usage
                .entry(input.schema.clone())
                .or_default()
                .push((descriptor.name.clone(), "input"));
        }
        for output in &descriptor.outputs {
            schema_usage
                .entry(output.schema.clone())
                .or_default()
                .push((descriptor.name.clone(), "output"));
        }
    }

    println!("Known schemas ({}):\n", schemas.len());

    for schema in &schemas {
        let has_definition =
            streamlib::core::embedded_schemas::get_embedded_schema_definition(schema).is_some();
        let def_marker = if has_definition { " [definition]" } else { "" };
        println!("  {}{}", schema, def_marker);

        if let Some(usages) = schema_usage.get(schema) {
            for (processor, direction) in usages {
                println!("    {} ({})", processor, direction);
            }
        }
        println!();
    }

    Ok(())
}

/// Show the YAML definition of a schema.
pub fn get(name: &str) -> Result<()> {
    match streamlib::core::embedded_schemas::get_embedded_schema_definition(name) {
        Some(definition) => {
            println!("{}", definition);
        }
        None => {
            // Check if it's at least known from the registry
            if PROCESSOR_REGISTRY.is_schema_known(name) {
                println!(
                    "Schema '{}' is referenced by registered processors, but no embedded definition found.",
                    name
                );
            } else {
                println!("No definition found for schema '{}'.", name);
            }

            // Show available schemas
            let available = streamlib::core::embedded_schemas::list_embedded_schema_names();
            println!("\nAvailable schema definitions:");
            for s in &available {
                println!("  {}", s);
            }
        }
    }
    Ok(())
}

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
