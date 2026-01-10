// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use anyhow::Result;
use streamlib::PROCESSOR_REGISTRY;

// Force linkage of streamlib-python to ensure Python processors are registered via inventory
extern crate streamlib_python;

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

/// List all available schemas.
pub fn schemas() -> Result<()> {
    // For now, list the built-in frame type schemas
    println!("Built-in schemas:\n");
    println!("  VideoFrame");
    println!("    GPU texture with metadata for video processing\n");
    println!("  AudioFrame");
    println!("    Fixed-size audio buffer with streaming metadata\n");
    println!("  DataFrame");
    println!("    Generic binary data with custom schema\n");

    Ok(())
}
