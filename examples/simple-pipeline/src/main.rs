//! Simple Pipeline Example
//!
//! Demonstrates the simplest possible pipeline using streamlib:
//! A chord generator → audio output.
//!
//! This example shows:
//! - Event-driven processing (no explicit tick/FPS parameters)
//! - Config-based processor creation
//! - Handle-based type-safe connections
//! - Runtime management
//!
//! You should hear a C major chord (C4, E4, G4) for 2 seconds.

use streamlib::core::{AudioOutputConfig, ChordGeneratorConfig};
use streamlib::{AudioFrame, AudioOutputProcessor, ChordGeneratorProcessor, Result, StreamRuntime};

fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== Simple Pipeline Example ===\n");
    println!("This example demonstrates:");
    println!("  • Event-driven processing");
    println!("  • Config-based processor creation");
    println!("  • Handle-based type-safe connections\n");

    // Create runtime (no FPS parameter - event-driven!)
    let mut runtime = StreamRuntime::new();

    // Audio configuration
    let sample_rate = 48000;
    let buffer_size = 512;
    println!("Audio Config:");
    println!("  Sample Rate: {} Hz", sample_rate);
    println!("  Channels: 2 (stereo)");
    println!("  Buffer Size: {} samples\n", buffer_size);

    // Create a chord generator (C major: C4 + E4 + G4)
    println!("🎵 Adding chord generator (C major - C4, E4, G4)...");
    let chord =
        runtime.add_processor_with_config::<ChordGeneratorProcessor>(ChordGeneratorConfig {
            amplitude: 0.15, // 15% volume to avoid clipping
            sample_rate,
            buffer_size,
        })?;
    println!("✓ Chord generator added\n");

    // Create audio output processor
    println!("🔊 Adding audio output processor...");
    let output = runtime.add_processor_with_config::<AudioOutputProcessor>(AudioOutputConfig {
        device_id: None, // Use default audio device
    })?;
    println!("✓ Audio output added\n");

    // Connect processors using type-safe handles
    // The compiler verifies that AudioFrame<2> → AudioFrame<2> types match!
    println!("🔗 Connecting chord generator → audio output...");
    runtime.connect(
        chord.output_port::<AudioFrame<2>>("chord"), // OutputPortRef<AudioFrame<2>>
        output.input_port::<AudioFrame<2>>("audio"), // InputPortRef<AudioFrame<2>>
    )?;
    println!("✓ Pipeline connected\n");

    // Run pipeline
    println!("▶️  Starting pipeline (you should hear a C major chord)...");
    runtime.start()?;

    // Play for 2 seconds
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Stop the pipeline
    println!("\n⏹️  Stopping pipeline...");
    runtime.stop()?;

    println!("\n✓ Pipeline complete");
    println!("✓ Demonstrated:");
    println!("  • Event-driven architecture (no FPS/tick parameters)");
    println!("  • Config-based API (ChordGeneratorConfig, AudioOutputConfig)");
    println!("  • Type-safe connections (AudioFrame<2> → AudioFrame<2>)");
    println!("  • Same code works on macOS, Linux, Windows!");

    Ok(())
}
