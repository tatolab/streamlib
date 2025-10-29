//! Simple Audio Recording Test
//!
//! Tests the microphone capture and CLAP plugin processing by:
//! 1. Recording audio from microphone
//! 2. Processing through CLAP Gain plugin
//! 3. Saving to output.raw (raw f32 samples)
//!
//! Usage:
//!   cargo run --example test_audio_recording --features clap-plugins
//!   cargo run --example test_audio_recording --features clap-plugins -- "AirPods"
//!   cargo run --example test_audio_recording --features clap-plugins -- 3
//!
//! The output file can be played with:
//!   ffplay -f f32le -ar 48000 -ac 2 output.raw

use streamlib::core::{
    AudioCaptureProcessor, AudioEffectProcessor, AudioFrame,
    clock::TimedTick, StreamProcessor,
};
use std::fs::File;
use std::io::Write;
use std::time::Duration;

/// Convert mono audio to stereo by duplicating the channel
fn mono_to_stereo(mono_frame: &AudioFrame) -> AudioFrame {
    if mono_frame.channels == 2 {
        return mono_frame.clone();
    }

    assert_eq!(mono_frame.channels, 1, "Expected mono (1 channel) audio");

    let mut stereo_samples = Vec::with_capacity(mono_frame.sample_count * 2);
    for &sample in mono_frame.samples.iter() {
        stereo_samples.push(sample); // Left
        stereo_samples.push(sample); // Right (same as left for mono)
    }

    AudioFrame::new(
        stereo_samples,
        mono_frame.timestamp_ns,
        mono_frame.frame_number,
        mono_frame.sample_rate,
        2, // stereo
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🎤 Audio Recording Test");
    println!("=====================\n");

    // Get device selection from command line
    let args: Vec<String> = std::env::args().collect();
    let device_filter = args.get(1).map(|s| s.as_str());

    // List available microphones and select device
    println!("Available microphones:");

    #[cfg(target_os = "macos")]
    let selected_device_id = {
        use streamlib::apple::processors::AppleAudioCaptureProcessor;
        let devices = AppleAudioCaptureProcessor::list_devices()?;

        for device in &devices {
            println!("  [{}] {}: {}Hz, {} channels{}",
                device.id,
                device.name,
                device.sample_rate,
                device.channels,
                if device.is_default { " (default)" } else { "" }
            );
        }

        // Select device based on filter
        if let Some(filter) = device_filter {
            // Try parsing as device ID number first
            if let Ok(id) = filter.parse::<usize>() {
                if devices.iter().any(|d| d.id == id) {
                    println!("\n✅ Selected device {} by ID", id);
                    Some(id)
                } else {
                    println!("\n⚠️  Device ID {} not found, using default", id);
                    None
                }
            } else {
                // Search by name (case-insensitive partial match)
                let filter_lower = filter.to_lowercase();
                if let Some(device) = devices.iter().find(|d| d.name.to_lowercase().contains(&filter_lower)) {
                    println!("\n✅ Selected device [{}] {}", device.id, device.name);
                    Some(device.id)
                } else {
                    println!("\n⚠️  No device matching '{}' found, using default", filter);
                    None
                }
            }
        } else {
            println!("\n✅ Using default device");
            None
        }
    };

    #[cfg(not(target_os = "macos"))]
    let selected_device_id: Option<usize> = None;

    println!();

    // Get the device's native sample rate and channel count
    #[cfg(target_os = "macos")]
    let (sample_rate, channels, device_name) = {
        use streamlib::apple::processors::AppleAudioCaptureProcessor;
        let devices = AppleAudioCaptureProcessor::list_devices()?;

        let device = if let Some(id) = selected_device_id {
            devices.iter().find(|d| d.id == id)
        } else {
            devices.iter().find(|d| d.is_default)
        }.ok_or("No suitable device found")?;

        (device.sample_rate, device.channels, device.name.clone())
    };

    #[cfg(not(target_os = "macos"))]
    let (sample_rate, channels, device_name) = (48000u32, 2u32, String::from("Unknown"));

    // Create microphone input using device's native configuration
    println!("📥 Creating microphone input for '{}'", device_name);
    println!("   Using device's native config: {}Hz, {} channel(s)", sample_rate, channels);

    #[cfg(target_os = "macos")]
    let mut mic = {
        use streamlib::apple::processors::AppleAudioCaptureProcessor;
        AppleAudioCaptureProcessor::new(selected_device_id, sample_rate, channels)?
    };

    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("Error: This example currently only works on macOS");
        return Ok(());
    }

    println!("✅ Microphone ready");

    // Load CLAP plugin
    let plugin_path = "/Users/fonta/Repositories/tatolab/clap-plugins/build/plugins/clap-plugins.clap/Contents/MacOS/clap-plugins";

    println!("\n🎛️  Loading CLAP Gain plugin...");
    #[cfg(feature = "clap-plugins")]
    let mut plugin = {
        use streamlib::core::processors::ClapEffectProcessor;
        use streamlib::core::AudioEffectProcessor;
        let mut p = ClapEffectProcessor::load_by_name(plugin_path, "Gain")?;

        // Activate first (this enumerates parameters)
        p.activate(sample_rate, 2048)?;

        // List available parameters (after activation)
        println!("Available parameters:");
        let params = p.list_parameters();
        for param in &params {
            println!("  [{}] {}: current={:.2}, range {:.2} to {:.2}",
                param.id, param.name, param.value, param.min, param.max);
        }

        // Calculate 250% gain value in actual dB units
        // NOTE: set_parameter expects ACTUAL parameter values (dB), not normalized 0-1!
        let gain_value = if let Some(gain_param) = params.first() {
            // For 250% gain (2.5x), we need approximately +8 dB
            // Formula: dB = 20 * log10(gain_multiplier)
            //         +8 dB = 20 * log10(2.5) ≈ 7.96 dB
            let gain_multiplier = 2.5f64; // 250% = 2.5x
            let target_db = 20.0 * gain_multiplier.log10();

            println!("✅ Setting gain to 250% (+{:.2} dB)", target_db);
            println!("   Parameter range: {:.1} to {:.1} dB", gain_param.min, gain_param.max);
            target_db
        } else {
            println!("⚠️  No parameters found, using default value 0.0");
            0.0
        };

        // Set the calculated gain value
        if !params.is_empty() {
            p.set_parameter(params[0].id, gain_value)?;
            println!("✅ Plugin loaded and activated (gain = 250%)");
        } else {
            println!("✅ Plugin loaded and activated (no parameters to set)");
        }
        p
    };

    #[cfg(not(feature = "clap-plugins"))]
    {
        eprintln!("Error: This example requires the 'clap-plugins' feature");
        return Ok(());
    }

    println!("\n🎙️  Recording for 10 seconds...");
    println!("(Speak into your microphone!)");

    // Countdown
    for i in (1..=3).rev() {
        println!("Starting in {}...", i);
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("🔴 RECORDING NOW!");

    // Collect both unprocessed and processed samples
    let mut all_unprocessed_samples: Vec<f32> = Vec::new();
    let mut all_processed_samples: Vec<f32> = Vec::new();

    // Record for 10 seconds
    let recording_duration = Duration::from_secs(10);
    let start_time = std::time::Instant::now();

    // Process at 60 FPS
    let frame_duration = Duration::from_millis(16);
    let mut frame_number = 0u64;

    // Give microphone time to start capturing
    std::thread::sleep(Duration::from_millis(100));

    while start_time.elapsed() < recording_duration {
        let loop_start = std::time::Instant::now();

        // Create tick
        let tick = TimedTick {
            timestamp: start_time.elapsed().as_secs_f64(),
            frame_number,
            clock_id: "test".to_string(),
            delta_time: 0.01667,
        };

        // Process microphone to get audio frame
        mic.process(tick)?;

        // Try to read from microphone output port buffer
        if let Some(audio_frame) = mic.output_port_mut().buffer().read_latest() {
            // Save unprocessed samples (original from mic)
            all_unprocessed_samples.extend_from_slice(&audio_frame.samples);

            // Convert mono to stereo if needed (CLAP plugins require stereo)
            let stereo_frame = mono_to_stereo(&audio_frame);

            // Process through CLAP plugin
            let processed = plugin.process_audio(&stereo_frame)?;

            // Save processed samples (after CLAP plugin)
            all_processed_samples.extend_from_slice(&processed.samples);

            // Show progress
            if frame_number % 30 == 0 {
                let elapsed = start_time.elapsed().as_secs();
                let remaining = recording_duration.as_secs() - elapsed;
                let level = mic.current_level();
                print!("\r⏱️  Recording... {}s remaining | Level: {:.2}", remaining, level);
                std::io::stdout().flush()?;
            }
        }

        frame_number += 1;

        // Maintain frame rate
        let elapsed = loop_start.elapsed();
        if elapsed < frame_duration {
            std::thread::sleep(frame_duration - elapsed);
        }
    }

    println!("\n\n💾 Writing output files...");

    // Write unprocessed audio (original channel count)
    let mut unprocessed_file = File::create("output_unprocessed.raw")?;
    for &sample in &all_unprocessed_samples {
        unprocessed_file.write_all(&sample.to_le_bytes())?;
    }
    println!("✅ Saved {} samples to output_unprocessed.raw (original, no CLAP)", all_unprocessed_samples.len());

    // Write processed audio (stereo after mono-to-stereo conversion)
    let mut processed_file = File::create("output_processed.raw")?;
    for &sample in &all_processed_samples {
        processed_file.write_all(&sample.to_le_bytes())?;
    }
    println!("✅ Saved {} samples to output_processed.raw (with CLAP gain)", all_processed_samples.len());

    println!("\n📊 Comparison:");
    println!("  Unprocessed: {} channels at {}Hz", channels, sample_rate);
    println!("  Processed:   2 channels at {}Hz (stereo)", sample_rate);

    println!("\n🔊 To play the recordings:");
    println!("  Unprocessed: ffplay -f f32le -ar {} -ac {} output_unprocessed.raw", sample_rate, channels);
    println!("  Processed:   ffplay -f f32le -ar {} -ac 2 output_processed.raw", sample_rate);

    println!("\n💿 Or convert to WAV:");
    println!("  ffmpeg -f f32le -ar {} -ac {} -i output_unprocessed.raw output_unprocessed.wav", sample_rate, channels);
    println!("  ffmpeg -f f32le -ar {} -ac 2 -i output_processed.raw output_processed.wav", sample_rate);

    Ok(())
}
