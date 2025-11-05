//! Camera to Display Pipeline Example
//!
//! Demonstrates a real-time camera → display pipeline using streamlib's
//! processor-based API with **type-safe connections via handles**.
//!
//! ## Key Features
//!
//! - **Type-safe connections**: Compiler enforces VideoFrame → VideoFrame matching at compile time
//! - **Handle-based API**: Processors are added first, then connected using handles
//! - **Event-driven**: Camera wakes display on each frame (push-based, not ticks)
//! - **Platform-agnostic**: Same code works on macOS, Linux, Windows
//! - **Zero-copy GPU**: Frames stay on GPU from camera to display
//!
//! ## How It Works
//!
//! 1. Add processors to runtime using config-based API (returns handles)
//! 2. Connect processors using type-safe port handles
//! 3. Camera captures frames and writes to output port
//! 4. Display wakes up immediately on data arrival (no polling)
//! 5. Display reads frame and renders to window
//!
//! Press Ctrl+C to stop.

use streamlib::{Result, StreamRuntime};

// Import platform-agnostic processor types
// On macOS: These resolve to AppleCameraProcessor and AppleDisplayProcessor
// On Linux: Would resolve to LinuxCameraProcessor and LinuxDisplayProcessor (future)
// On Windows: Would resolve to WindowsCameraProcessor and WindowsDisplayProcessor (future)
use streamlib::{CameraProcessor, DisplayProcessor};

// Import config types for processor configuration
use streamlib::core::config::{CameraConfig, DisplayConfig};
use streamlib::core::VideoFrame;

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Camera → Display Pipeline (Handle-Based API) ===\n");

    // Create runtime - automatically configured for the platform
    // Event-driven: processors wake on data arrival or timer events
    let mut runtime = StreamRuntime::new();

    // Create camera processor using config-based API
    // This returns a ProcessorHandle for making type-safe connections
    println!("📷 Adding camera processor...");
    let camera = runtime.add_processor_with_config::<CameraProcessor>(
        CameraConfig {
            device_id: Some("0x11424001bcf2284".to_string()), // Use specific camera to avoid Continuity Camera
        }
    )?;
    println!("✓ Camera added\n");

    // Create display processor using config-based API
    println!("🖥️  Adding display processor...");
    let display = runtime.add_processor_with_config::<DisplayProcessor>(
        DisplayConfig {
            width: 1280,
            height: 720,
            title: Some("streamlib Camera Display".to_string()),
        }
    )?;
    println!("✓ Display added\n");

    // Connect camera output → display input using handles (TYPE-SAFE!)
    // The compiler ensures both ports use VideoFrame - mismatched types won't compile
    println!("🔗 Connecting camera → display (type-safe handles)...");
    runtime.connect(
        camera.output_port::<VideoFrame>("video"),   // OutputPortRef<VideoFrame>
        display.input_port::<VideoFrame>("video"),   // InputPortRef<VideoFrame>
    )?;
    println!("✓ Pipeline connected\n");

    // Start pipeline
    println!("▶️  Starting pipeline...");
    println!("   Press Ctrl+C to stop\n");
    runtime.start().await?;

    // Run until Ctrl+C
    runtime.run().await?;

    println!("\n✓ Pipeline stopped gracefully");

    Ok(())
}
