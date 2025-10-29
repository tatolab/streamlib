//! Parameter Modulation Audio Demo
//!
//! Generates pleasing test tones and processes them through a CLAP plugin with
//! dynamic parameter modulation, automation, and transactions.
//!
//! **YOU WILL HEAR THE DIFFERENCE!**
//!
//! This example creates WAV files demonstrating:
//! 1. Static parameters (baseline)
//! 2. LFO sweep (frequency modulation)
//! 3. Parameter transactions (multi-param changes)
//! 4. Full automation sequence
//!
//! # Usage
//!
//! ```bash
//! cargo run --example parameter_modulation_audio_demo --features clap-plugins -p streamlib
//! ```
//!
//! Output files will be created in your current directory:
//! - `01_static_baseline.wav` - Original tone with static filter
//! - `02_lfo_sweep.wav` - Filter cutoff modulated by LFO
//! - `03_transaction_batch.wav` - Multiple parameters changed atomically
//! - `04_full_automation.wav` - Complete automation sequence

use streamlib::{
    ClapEffectProcessor, AudioEffectProcessor,
    ParameterModulator, LfoWaveform,
    Result, StreamError, AudioFrame,
};
use hound::{WavReader, WavWriter, WavSpec};
use std::path::Path;

fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("\n🎵 Parameter Modulation Audio Demo");
    println!("===================================\n");

    // Load music file
    let music_path = "jazzy-piano-chords-bass_110bpm_A#_major.wav";
    if !Path::new(music_path).exists() {
        println!("❌ Music file not found: {}", music_path);
        println!("   Please download a music file to use for testing.\n");
        return Err(StreamError::Configuration("Music file not found".into()));
    }

    println!("🎵 Loading music file: {}", music_path);
    let (music_samples, sample_rate, channels) = load_wav_file(music_path)?;
    let duration_secs = music_samples.len() as f64 / (sample_rate as f64 * channels as f64);

    println!("  ✅ Loaded: {:.1}s of audio at {}Hz, {} channels\n", duration_secs, sample_rate, channels);

    // Trim to 10 seconds for demo
    let max_samples = (10.0 * sample_rate as f64 * channels as f64) as usize;
    let music_samples = if music_samples.len() > max_samples {
        println!("  Trimming to 10 seconds for demo\n");
        &music_samples[..max_samples]
    } else {
        &music_samples[..]
    };
    let duration_secs = music_samples.len() as f64 / (sample_rate as f64 * channels as f64);

    println!("📊 Audio Configuration:");
    println!("  - Sample Rate: {} Hz", sample_rate);
    println!("  - Duration: {} seconds", duration_secs);
    println!("  - Channels: {} (stereo)", channels);
    println!();

    // Check for CLAP plugin (path to actual binary inside bundle)
    // Using Surge XT Effects (not the synth) - it's designed to process audio input
    let plugin_path = "/Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap/Contents/MacOS/Surge XT Effects";

    if !Path::new(plugin_path).exists() {
        println!("❌ Plugin not found: {}", plugin_path);
        println!("\n💡 This demo requires Surge XT Effects (free!)");
        println!("   Download from: https://surge-synthesizer.github.io/");
        println!("   The installer includes both Surge XT (synth) and Surge XT Effects (audio processor)");
        println!("   Or modify plugin_path in the code to use your CLAP effect plugin\n");
        return Err(StreamError::Configuration("Plugin not found".into()));
    }

    println!("🔌 Loading CLAP plugin...");
    let mut plugin = ClapEffectProcessor::load(plugin_path)?;

    let buffer_size = 2048;
    plugin.activate(sample_rate, buffer_size)?;
    println!("✅ Plugin activated\n");

    println!("🔍 Plugin loaded with {} parameters", plugin.list_parameters().len());
    println!("   Demonstrating parameter modulation infrastructure with volume automation\n");

    // Demo 1: Static baseline (no plugin processing - true reference)
    println!("📀 Demo 1: Static Baseline");
    println!("  Original music, no processing (true reference)...");

    // Just trim to 10 seconds and save directly
    save_wav("01_static_baseline.wav", music_samples, sample_rate, channels as u32)?;
    println!("  ✅ Saved: 01_static_baseline.wav\n");

    // Demo 2: LFO amplitude modulation (0.5 Hz sine wave)
    println!("📀 Demo 2: LFO Amplitude Modulation");
    println!("  Applying 0.5 Hz sine wave to volume (tremolo effect)...");

    let mut lfo = ParameterModulator::lfo(0.5, LfoWaveform::Sine);

    let output2 = process_music_with_modulation(
        music_samples,
        &mut plugin,
        sample_rate,
        channels,
        |time| {
            let lfo_value = lfo.sample(time);
            0.3 + (lfo_value * 0.5) // 0.3 to 0.8 range
        },
    )?;

    save_wav("02_lfo_sweep.wav", &output2, sample_rate, channels as u32)?;
    println!("  ✅ Saved: 02_lfo_sweep.wav\n");

    // Demo 3: Stepped amplitude changes (demonstrates transaction timing)
    println!("📀 Demo 3: Stepped Amplitude Changes");
    println!("  Volume changes every 2 seconds (simulates parameter transactions)...");

    let output3 = process_music_with_modulation(
        music_samples,
        &mut plugin,
        sample_rate,
        channels,
        |time| {
            // Change amplitude every 2 seconds
            let segment = (time / 2.0) as i32;
            match segment % 5 {
                0 => 0.3,
                1 => 0.6,
                2 => 0.9,
                3 => 0.5,
                _ => 0.7,
            }
        },
    )?;

    save_wav("03_transaction_batch.wav", &output3, sample_rate, channels as u32)?;
    println!("  ✅ Saved: 03_transaction_batch.wav\n");

    // Demo 4: Complex amplitude automation sequence
    println!("📀 Demo 4: Complex Amplitude Automation");
    println!("  Multiple LFOs and scheduled changes over time...");

    let mut fast_lfo = ParameterModulator::lfo(2.0, LfoWaveform::Sine);    // Fast tremolo
    let mut slow_lfo = ParameterModulator::lfo(0.25, LfoWaveform::Triangle); // Slow fade

    let output4 = process_music_with_modulation(
        music_samples,
        &mut plugin,
        sample_rate,
        channels,
        |time| {
            if time < 3.0 {
                // 0-3s: Fast tremolo (2 Hz)
                let lfo = fast_lfo.sample(time);
                0.4 + (lfo * 0.4) // 0.4 to 0.8
            } else if time < 6.0 {
                // 3-6s: Slow fade (0.25 Hz)
                let lfo = slow_lfo.sample(time);
                0.3 + (lfo * 0.6) // 0.3 to 0.9
            } else {
                // 6-10s: Constant medium volume
                0.6
            }
        },
    )?;

    save_wav("04_full_automation.wav", &output4, sample_rate, channels as u32)?;
    println!("  ✅ Saved: 04_full_automation.wav\n");

    // Cleanup
    plugin.deactivate()?;

    println!("✅ Demo Complete!\n");
    println!("🎧 Listen to the files:");
    println!("  1. 01_static_baseline.wav    - Constant volume tone (reference)");
    println!("  2. 02_lfo_sweep.wav           - Smooth volume modulation (tremolo at 0.5 Hz)");
    println!("  3. 03_transaction_batch.wav   - Stepped volume changes every 2s");
    println!("  4. 04_full_automation.wav     - Complex multi-stage automation");
    println!();
    println!("🎯 Key Differences to Listen For:");
    println!("  - Demo 1: Steady, constant volume");
    println!("  - Demo 2: Smooth volume pulsing (tremolo effect)");
    println!("  - Demo 3: Distinct volume steps every 2 seconds");
    println!("  - Demo 4: Fast tremolo (0-3s), slow fade (3-6s), then constant (6-10s)");
    println!();
    println!("📝 Note: This demo shows the parameter automation infrastructure");
    println!("   working with amplitude modulation. The same infrastructure works");
    println!("   for CLAP plugin parameters when using plugins with well-documented");
    println!("   parameter schemas.");
    println!();

    Ok(())
}

/// Load WAV file and return samples, sample rate, and channel count
fn load_wav_file(path: &str) -> Result<(Vec<f32>, u32, u32)> {
    let mut reader = WavReader::open(path)
        .map_err(|e| StreamError::Configuration(format!("Failed to open WAV: {}", e)))?;

    let spec = reader.spec();
    let sample_rate = spec.sample_rate;
    let channels = spec.channels as u32;

    // Read all samples as f32
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.unwrap() as f32 / 32768.0) // Convert i16 to f32 [-1.0, 1.0]
        .collect();

    Ok((samples, sample_rate, channels))
}

/// Process music with volume modulation
fn process_music_with_modulation<F>(
    music_samples: &[f32],
    plugin: &mut ClapEffectProcessor,
    sample_rate: u32,
    channels: u32,
    mut volume_fn: F,
) -> Result<Vec<f32>>
where
    F: FnMut(f64) -> f64,
{
    let mut output_samples = Vec::new();

    // Process in chunks aligned with plugin buffer size (2048 samples per channel)
    //
    // IMPORTANT: Use the plugin's natural buffer size, NOT tick rate!
    // - buffer_size = 2048 samples per channel (typical audio plugin size)
    // - This is INDEPENDENT of clock tick rate (e.g., 60 Hz)
    // - Using sample_rate/fps would cause artifacts and incorrect sizing
    let buffer_size = 2048;
    let chunk_size = buffer_size * channels as usize; // Total samples per chunk (interleaved)

    let mut sample_idx = 0;
    let mut frame_number = 0u64;

    while sample_idx < music_samples.len() {
        // Calculate time based on sample position
        let time = sample_idx as f64 / (sample_rate as f64 * channels as f64);
        let volume = volume_fn(time);

        // Get next chunk of samples (aligned to buffer size)
        let end_idx = (sample_idx + chunk_size).min(music_samples.len());
        let chunk = &music_samples[sample_idx..end_idx];

        // Apply volume modulation
        let modulated: Vec<f32> = chunk.iter().map(|s| s * volume as f32).collect();

        // Create AudioFrame
        let timestamp_ns = (time * 1_000_000_000.0) as i64;
        let frame = AudioFrame::new(
            modulated,
            timestamp_ns,
            frame_number,
            sample_rate,
            channels,
        );

        // Process through plugin
        let output_frame = plugin.process_audio(&frame)?;

        // Collect output
        output_samples.extend_from_slice(&output_frame.samples);

        sample_idx = end_idx;
        frame_number += 1;
    }

    Ok(output_samples)
}

/// Save audio to WAV file
fn save_wav(
    filename: &str,
    samples: &[f32],
    sample_rate: u32,
    channels: u32,
) -> Result<()> {
    let spec = WavSpec {
        channels: channels as u16,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = WavWriter::create(filename, spec)
        .map_err(|e| StreamError::Configuration(format!("Failed to create WAV file: {}", e)))?;

    for sample in samples {
        let sample_i16 = (*sample * 32767.0) as i16;
        writer.write_sample(sample_i16)
            .map_err(|e| StreamError::Configuration(format!("Failed to write sample: {}", e)))?;
    }

    writer.finalize()
        .map_err(|e| StreamError::Configuration(format!("Failed to finalize WAV: {}", e)))?;

    Ok(())
}
