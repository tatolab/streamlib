//! Example: Inspect Processor Descriptors
//!
//! Demonstrates what processor descriptors look like when serialized to JSON.
//! This is what AI agents and MCP tools would see.

use streamlib_core::{
    ProcessorDescriptor, PortDescriptor, SCHEMA_VIDEO_FRAME,
};
use std::sync::Arc;

fn main() {
    println!("=== Processor Descriptor Inspector ===\n");

    // Create example descriptors (same as actual processors)
    let camera_descriptor = ProcessorDescriptor::new(
        "CameraProcessor",
        "Captures video frames from a camera device. Outputs WebGPU textures at the configured frame rate."
    )
    .with_usage_context(
        "Use when you need live video input from a camera. This is typically the source \
         processor in a pipeline. Supports multiple camera devices - use set_device_id() \
         to select a specific camera, or use 'default' for the system default camera."
    )
    .with_output(PortDescriptor::new(
        "video",
        Arc::clone(&SCHEMA_VIDEO_FRAME),
        true,
        "Live video frames from the camera. Each frame is a WebGPU texture with timestamp \
         and metadata. Frames are produced at the camera's native frame rate (typically 30 or 60 FPS)."
    ))
    .with_tags(vec!["source", "camera", "video", "input", "capture"]);

    let display_descriptor = ProcessorDescriptor::new(
        "DisplayProcessor",
        "Displays video frames in a window. Renders WebGPU textures to the screen at the configured frame rate."
    )
    .with_usage_context(
        "Use when you need to visualize video output in a window. This is typically a sink \
         processor at the end of a pipeline. Each DisplayProcessor manages one window. The window \
         is created automatically on first frame and can be configured with set_window_title()."
    )
    .with_input(PortDescriptor::new(
        "video",
        Arc::clone(&SCHEMA_VIDEO_FRAME),
        true,
        "Video frames to display. Accepts WebGPU textures and renders them to the window. \
         Automatically handles format conversion and scaling to fit the window."
    ))
    .with_tags(vec!["sink", "display", "window", "output", "render"]);

    let processors = vec![
        ("CameraProcessor", camera_descriptor),
        ("DisplayProcessor", display_descriptor),
    ];

    for (name, descriptor) in processors {
        println!("📹 {}", name);
        println!("{}", "=".repeat(60));

        // Serialize to JSON (pretty-printed)
        match descriptor.to_json() {
            Ok(json) => {
                println!("{}", json);
            }
            Err(e) => {
                eprintln!("Error serializing {}: {}", name, e);
            }
        }

        println!("\n");
    }

    // Show what querying by tags looks like
    println!("\n📑 Query Examples");
    println!("{}", "=".repeat(60));

    // In a real scenario with registry:
    println!("\n// Find all source processors:");
    println!("let sources = list_processors_by_tag(\"source\");");
    println!("// Returns: [CameraProcessor]");

    println!("\n// Find all sink processors:");
    println!("let sinks = list_processors_by_tag(\"sink\");");
    println!("// Returns: [DisplayProcessor]");

    println!("\n// Find all video processors:");
    println!("let video = list_processors_by_tag(\"video\");");
    println!("// Returns: [CameraProcessor, DisplayProcessor]");
}
