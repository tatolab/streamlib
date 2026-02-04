// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use anyhow::Result;
use streamlib::PROCESSOR_REGISTRY;

/// List all registered processor types.
pub fn processors() -> Result<()> {
    let descriptors = PROCESSOR_REGISTRY.list_registered();

    if descriptors.is_empty() {
        println!("No processors registered.");
        return Ok(());
    }

    println!("Available processors ({}):\n", descriptors.len());

    for descriptor in &descriptors {
        println!("  {}", descriptor.name);
        if !descriptor.description.is_empty() {
            println!("    {}", descriptor.description);
        }

        if !descriptor.inputs.is_empty() {
            println!("    Inputs:");
            for input in &descriptor.inputs {
                println!("      - {} ({})", input.name, input.schema);
            }
        }

        if !descriptor.outputs.is_empty() {
            println!("    Outputs:");
            for output in &descriptor.outputs {
                println!("      - {} ({})", output.name, output.schema);
            }
        }

        println!();
    }

    Ok(())
}
