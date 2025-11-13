use streamlib::{Result, StreamRuntime};
use streamlib::{CameraProcessor, DisplayProcessor};
use streamlib::core::{CameraConfig, DisplayConfig, VideoFrame};

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== Camera → Display Pipeline (Handle-Based API) ===\n");

    let mut runtime = StreamRuntime::new();

    println!("📷 Adding camera processor...");
    let camera = runtime.add_processor_with_config::<CameraProcessor>(
        CameraConfig {
            device_id: None, // Use default camera
        }
    )?;
    println!("✓ Camera added\n");

    println!("🖥️  Adding display processor...");
    let display = runtime.add_processor_with_config::<DisplayProcessor>(
        DisplayConfig {
            width: 1280,
            height: 720,
            title: Some("streamlib Camera Display".to_string()),
        }
    )?;
    println!("✓ Display added\n");

    println!("🔗 Connecting camera → display (type-safe handles)...");
    runtime.connect(
        camera.output_port::<VideoFrame>("video"),
        display.input_port::<VideoFrame>("video"),
    )?;
    println!("✓ Pipeline connected\n");

    println!("▶️  Starting pipeline...");
    #[cfg(target_os = "macos")]
    println!("   Press Cmd+Q to stop\n");
    #[cfg(not(target_os = "macos"))]
    println!("   Press Ctrl+C to stop\n");
    runtime.start()?;
    runtime.run()?;

    println!("\n✓ Pipeline stopped gracefully");

    Ok(())
}
