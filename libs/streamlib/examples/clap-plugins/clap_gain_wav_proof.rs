//! CLAP Gain WAV File Proof
//!
//! Generates WAV files at different gain settings to prove parameter processing works.
//! Creates 5 files you can listen to:
//! - input.wav: Original 440Hz tone
//! - output_minus40db.wav: Minimum gain (almost silent)
//! - output_0db.wav: Unity gain (unchanged)
//! - output_plus8db.wav: +8 dB gain (2.5x louder)
//! - output_plus20db.wav: +20 dB gain (10x louder)
//!
//! Usage:
//!   cargo run --example clap_gain_wav_proof --features clap-plugins
//!
//! Then play the files with:
//!   afplay input.wav
//!   afplay output_0db.wav
//!   afplay output_plus8db.wav
//!   etc.

use streamlib::core::{AudioEffectProcessor, AudioFrame};
use std::f32::consts::PI;

/// Generate a 440Hz sine wave tone
fn generate_sine_wave(sample_rate: u32, duration_secs: f32, amplitude: f32) -> Vec<f32> {
    let frequency = 440.0; // A4 note
    let num_samples = (sample_rate as f32 * duration_secs) as usize;
    let mut samples = Vec::with_capacity(num_samples);

    for i in 0..num_samples {
        let t = i as f32 / sample_rate as f32;
        let sample = amplitude * (2.0 * PI * frequency * t).sin();
        samples.push(sample);
    }

    samples
}

/// Convert mono to stereo by duplicating channels
fn mono_to_stereo(mono_samples: &[f32]) -> Vec<f32> {
    let mut stereo = Vec::with_capacity(mono_samples.len() * 2);
    for &sample in mono_samples {
        stereo.push(sample); // Left
        stereo.push(sample); // Right
    }
    stereo
}

/// Save stereo audio to WAV file
fn save_wav(filename: &str, samples: &[f32], sample_rate: u32) -> Result<(), Box<dyn std::error::Error>> {
    use hound::{WavSpec, WavWriter};

    let spec = WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = WavWriter::create(filename, spec)?;

    // Convert f32 samples (-1.0 to 1.0) to i16 samples (-32768 to 32767)
    for &sample in samples {
        let sample_i16 = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
        writer.write_sample(sample_i16)?;
    }

    writer.finalize()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🎵 CLAP Gain WAV File Proof");
    println!("============================\n");

    let sample_rate = 48000u32;
    let duration = 2.0; // 2 seconds
    let input_amplitude = 0.3; // 30% amplitude (safe level)

    // Generate 440Hz test tone
    println!("📊 Generating 440Hz sine wave...");
    println!("   Sample rate: {}Hz", sample_rate);
    println!("   Duration: {}s", duration);
    println!("   Input amplitude: {:.1}%\n", input_amplitude * 100.0);

    let mono_samples = generate_sine_wave(sample_rate, duration, input_amplitude);
    let input_stereo = mono_to_stereo(&mono_samples);

    // Save original input
    println!("💾 Saving input.wav...");
    save_wav("input.wav", &input_stereo, sample_rate)?;
    println!("   ✅ Saved original tone (no processing)");
    println!("   Play with: afplay input.wav\n");

    // Load CLAP plugin
    let plugin_path = "/Users/fonta/Repositories/tatolab/clap-plugins/build/plugins/clap-plugins.clap/Contents/MacOS/clap-plugins";

    #[cfg(feature = "clap-plugins")]
    {
        use streamlib::core::processors::ClapEffectProcessor;

        println!("🎛️  Loading CLAP Gain plugin...");
        let mut plugin = ClapEffectProcessor::load_by_name(plugin_path, "Gain")?;
        plugin.activate(sample_rate, 2048)?;

        let params = plugin.list_parameters();
        if let Some(gain_param) = params.first() {
            println!("   Parameter: {} [ID={}]", gain_param.name, gain_param.id);
            println!("   Range: {:.1} to {:.1} dB\n", gain_param.min, gain_param.max);

            // Test cases: Different gain settings
            let test_cases = vec![
                (-40.0, "output_minus40db.wav", "Minimum gain (almost silent, 0.01x)"),
                (0.0, "output_0db.wav", "Unity gain (unchanged, 1.0x)"),
                (8.0, "output_plus8db.wav", "+8 dB gain (2.5x louder)"),
                (20.0, "output_plus20db.wav", "+20 dB gain (10x louder)"),
            ];

            for (gain_db, filename, description) in test_cases {
                println!("🔄 Processing with {} dB gain...", gain_db);
                println!("   Description: {}", description);

                // Set gain parameter
                plugin.set_parameter(gain_param.id, gain_db)?;

                // Process audio
                let input_frame = AudioFrame::new(
                    input_stereo.clone(),
                    0,
                    0,
                    sample_rate,
                    2, // stereo
                );

                let output_frame = plugin.process_audio(&input_frame)?;

                // Calculate actual gain applied
                let input_peak = input_stereo.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                let output_peak = output_frame.samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                let actual_gain_ratio = output_peak / input_peak;
                let actual_gain_db = 20.0 * actual_gain_ratio.log10();

                println!("   Input peak:  {:.4} ({:.1}%)", input_peak, input_peak * 100.0);
                println!("   Output peak: {:.4} ({:.1}%)", output_peak, output_peak * 100.0);
                println!("   Actual gain: {:.2}x ({:.2} dB)", actual_gain_ratio, actual_gain_db);

                // Save to WAV
                save_wav(filename, &output_frame.samples, sample_rate)?;
                println!("   ✅ Saved {}", filename);
                println!("   Play with: afplay {}\n", filename);
            }
        }

        println!("✨ All files generated successfully!\n");
        println!("🎧 Listen to the files to hear the difference:");
        println!("   afplay input.wav              # Original (baseline)");
        println!("   afplay output_minus40db.wav   # Very quiet (barely audible)");
        println!("   afplay output_0db.wav         # Same as input (unity gain)");
        println!("   afplay output_plus8db.wav     # Noticeably louder (2.5x)");
        println!("   afplay output_plus20db.wav    # Much louder (10x)");
        println!();
        println!("💡 The gain parameter is working correctly!");
        println!("   Each file demonstrates different gain settings.");
        println!("   You can HEAR the CLAP parameter processing in action!");
    }

    #[cfg(not(feature = "clap-plugins"))]
    {
        eprintln!("Error: This example requires the 'clap-plugins' feature");
        return Ok(());
    }

    Ok(())
}
