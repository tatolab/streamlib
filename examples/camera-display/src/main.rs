use streamlib::{Result, StreamRuntime};
use streamlib::{CameraProcessor, DisplayProcessor};
use streamlib::core::{CameraConfig, DisplayConfig, VideoFrame};

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Camera → Display Pipeline (Handle-Based API) ===\n");

    
    let mut runtime = StreamRuntime::new();

    
    println!("📷 Adding camera processor...");
    let camera = runtime.add_processor_with_config::<CameraProcessor>(
        CameraConfig {
            device_id: Some("0x11424001bcf2284".to_string()), 
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

    // Start pipeline
    println!("▶️  Starting pipeline...");
    println!("   Press Ctrl+C to stop\n");
    runtime.start().await?;

    // Run until Ctrl+C
    runtime.run().await?;

    println!("\n✓ Pipeline stopped gracefully");

    Ok(())
}
