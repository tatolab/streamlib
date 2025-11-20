//! Simple Pipeline Example
//!
//! Demonstrates the simplest possible pipeline using streamlib:
//! A test tone generator → audio output.
//!
//! This example shows:
//! - Event-driven processing (no explicit tick/FPS parameters)
//! - Config-based processor creation
//! - Handle-based type-safe connections
//! - Runtime management
//!
//! You should hear a 440 Hz tone (musical note A4) for 2 seconds.

use streamlib::core::config::{AudioOutputConfig, TestToneConfig};
use streamlib::{AudioFrame, AudioOutputProcessor, Result, StreamRuntime, TestToneGenerator};

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Simple Pipeline Example ===\n");
    println!("This example demonstrates:");
    println!("  • Event-driven processing");
    println!("  • Config-based processor creation");
    println!("  • Handle-based type-safe connections\n");

    // Create runtime (no FPS parameter - event-driven!)
    let mut runtime = StreamRuntime::new();

    // Get global audio config from runtime
    let audio_config = runtime.audio_config();
    println!("Audio Config:");
    println!("  Sample Rate: {} Hz", audio_config.sample_rate);
    println!("  Channels: {}", audio_config.channels);
    println!("  Buffer Size: {} samples\n", audio_config.buffer_size);

    // Create a test tone generator (440 Hz = musical note A4)
    println!("🎵 Adding test tone generator (440 Hz)...");
    let tone = runtime.add_processor_with_config::<TestToneGenerator>(TestToneConfig {
        frequency: 440.0,
        amplitude: 0.3, // 30% volume to avoid clipping
        sample_rate: audio_config.sample_rate,
        timer_group_id: None,
    })?;
    println!("✓ Test tone added\n");

    // Create audio output processor
    println!("🔊 Adding audio output processor...");
    let output = runtime.add_processor_with_config::<AudioOutputProcessor>(AudioOutputConfig {
        device_id: None, // Use default audio device
    })?;
    println!("✓ Audio output added\n");

    // Connect processors using type-safe handles
    // The compiler verifies that AudioFrame → AudioFrame types match!
    println!("🔗 Connecting test tone → audio output...");
    runtime.connect(
        tone.output_port::<AudioFrame>("audio"), // OutputPortRef<AudioFrame>
        output.input_port::<AudioFrame>("audio"), // InputPortRef<AudioFrame>
    )?;
    println!("✓ Pipeline connected\n");

    // Run pipeline
    println!("▶️  Starting pipeline (you should hear a 440 Hz tone)...");
    runtime.start().await?;

    // Play for 2 seconds
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // Stop the pipeline
    println!("\n⏹️  Stopping pipeline...");
    runtime.stop().await?;

    println!("\n✓ Pipeline complete");
    println!("✓ Demonstrated:");
    println!("  • Event-driven architecture (no FPS/tick parameters)");
    println!("  • Config-based API (TestToneConfig, AudioOutputConfig)");
    println!("  • Type-safe connections (AudioFrame → AudioFrame)");
    println!("  • Same code works on macOS, Linux, Windows!");

    Ok(())
}
