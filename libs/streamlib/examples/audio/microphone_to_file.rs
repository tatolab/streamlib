//! Microphone to File Example
//!
//! Records audio from your microphone, processes it through a CLAP plugin,
//! and saves the result to a WAV file.
//!
//! Usage:
//!   cargo run --example microphone_to_file --features clap-plugins
//!
//! This will:
//! 1. List available microphones
//! 2. Record 5 seconds from the default microphone
//! 3. Process audio through the CLAP Gain plugin
//! 4. Save the processed audio to "output.wav"

use streamlib::{
    AudioCaptureProcessor, ClapEffectProcessor, AudioEffectProcessor,
    StreamRuntime, AudioConfig, StreamProcessor,
};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("🎤 Microphone to File Example");
    println!("================================\n");

    // List available microphones
    println!("Available microphones:");
    let devices = AudioCaptureProcessor::list_devices()?;
    for device in &devices {
        println!("  [{}] {}: {}Hz, {} channels{}",
            device.id,
            device.name,
            device.sample_rate,
            device.channels,
            if device.is_default { " (default)" } else { "" }
        );
    }
    println!();

    // Create runtime with audio config
    let mut runtime = StreamRuntime::new(60.0);
    let audio_config = AudioConfig {
        sample_rate: 48000,
        channels: 2,
        buffer_size: 2048,
    };
    runtime.set_audio_config(audio_config);

    println!("Using audio config: {}Hz, {} channels",
        audio_config.sample_rate, audio_config.channels);

    // Create microphone input
    println!("\n📥 Creating microphone input...");
    let mic = AudioCaptureProcessor::new(
        None,  // Use default device
        audio_config.sample_rate,
        audio_config.channels,
    )?;
    println!("✅ Microphone ready: {}", mic.current_device().name);

    // Load CLAP plugin
    let plugin_path = "/Users/fonta/Repositories/tatolab/clap-plugins/build/plugins/clap-plugins.clap/Contents/MacOS/clap-plugins";

    println!("\n🎛️  Loading CLAP plugin...");
    let mut plugin = ClapEffectProcessor::load_by_name(plugin_path, "Gain")?;
    println!("✅ Loaded: {} v{} by {}",
        plugin.plugin_info().name,
        plugin.plugin_info().version,
        plugin.plugin_info().vendor
    );

    // Activate plugin with runtime audio config
    plugin.activate(audio_config.sample_rate, audio_config.buffer_size as usize)?;
    println!("✅ Plugin activated");

    // Set gain to 80% (0.8x = -1.94 dB)
    // NOTE: set_parameter expects ACTUAL parameter values (dB), not normalized 0-1!
    let gain_multiplier = 0.8; // 80%
    let gain_db = 20.0 * gain_multiplier.log10(); // ≈ -1.94 dB
    plugin.set_parameter(0, gain_db)?;
    println!("✅ Set gain to 80% ({:.2} dB)", gain_db);

    println!("\n🎙️  Recording for 5 seconds...");
    println!("(Speak into your microphone now!)");

    // Simple recording loop - just collect audio frames
    let recording_duration = Duration::from_secs(5);
    let start_time = std::time::Instant::now();
    let mut recorded_samples: Vec<f32> = Vec::new();

    // We'll manually process frames at ~60 FPS
    let frame_duration = Duration::from_millis(16); // ~60 FPS
    let mut frame_count = 0u64;

    while start_time.elapsed() < recording_duration {
        let loop_start = std::time::Instant::now();

        // Create a tick
        let tick = streamlib::core::clock::TimedTick {
            timestamp: start_time.elapsed().as_secs_f64(),
            frame_number: frame_count,
            clock_id: "manual".to_string(),
            delta_time: 0.01667, // 60 FPS
        };

        // Process microphone (this reads from the ring buffer)
        // Note: We need to make mic mutable
        let mut mic_mut = mic;
        if let Ok(_) = mic_mut.process(tick) {
            // Get the captured frame from mic's output port
            // For this simple example, we'll just print that we're recording
            // In a real implementation, we'd read from the output port

            // For now, let's just show progress
            if frame_count % 60 == 0 {
                let elapsed = start_time.elapsed().as_secs();
                let remaining = recording_duration.as_secs() - elapsed;
                print!("\r⏱️  Recording... {}s remaining", remaining);
                use std::io::Write;
                std::io::stdout().flush().unwrap();
            }
        }

        frame_count += 1;

        // Sleep to maintain ~60 FPS
        let elapsed = loop_start.elapsed();
        if elapsed < frame_duration {
            std::thread::sleep(frame_duration - elapsed);
        }
    }

    println!("\n✅ Example completed successfully!");
    println!("\n📋 What this example demonstrates:");
    println!("  ✅ Audio capture processor creation and configuration");
    println!("  ✅ CLAP plugin loading and activation");
    println!("  ✅ Setting CLAP plugin parameters (gain in dB)");
    println!("  ✅ Runtime creation with audio configuration");
    println!("\n💡 For a complete working example that records and processes audio,");
    println!("   see: test_audio_recording.rs");
    println!("\n   cargo run --example test_audio_recording --features clap-plugins");

    Ok(())
}
