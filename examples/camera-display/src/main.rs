use streamlib::core::{CameraConfig, DisplayConfig};
use streamlib::{input, output, CameraProcessor, DisplayProcessor, Result, StreamRuntime};

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    println!("=== Camera → Display Pipeline ===\n");

    let mut runtime = StreamRuntime::new();

    // =========================================================================
    // Add processors
    // =========================================================================

    println!("📷 Adding camera processor...");
    let camera = runtime.add_processor::<CameraProcessor::Processor>(CameraConfig {
        device_id: None, // Use default camera
    })?;
    println!("✓ Camera added: {}\n", camera.id);

    println!("🖥️  Adding display processor...");
    let display = runtime.add_processor::<DisplayProcessor::Processor>(DisplayConfig {
        width: 3840,
        height: 2160,
        title: Some("streamlib Camera Display".to_string()),
        scaling_mode: Default::default(),
    })?;
    println!("✓ Display added: {}\n", display.id);

    // =========================================================================
    // Connect ports
    // =========================================================================

    println!("🔗 Connecting camera → display...");

    // Type-safe connection using ProcessorNode methods
    // - Port names validated at runtime against node's port metadata
    // - Panics if port doesn't exist (use try_output/try_input for Result)
    runtime.connect(
        output::<CameraProcessor::OutputLink::video>(&camera),
        input::<DisplayProcessor::InputLink::video>(&display),
    )?;

    println!("✓ Pipeline connected\n");

    // =========================================================================
    // Run the pipeline
    // =========================================================================

    println!("▶️  Starting pipeline...");
    #[cfg(target_os = "macos")]
    println!("   Press Cmd+Q to stop\n");
    #[cfg(not(target_os = "macos"))]
    println!("   Press Ctrl+C to stop\n");

    runtime.start()?;
    runtime.wait_for_signal()?;

    println!("\n✓ Pipeline stopped gracefully");

    Ok(())
}
