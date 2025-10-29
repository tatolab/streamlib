//! CLAP Gain Plugin Test
//!
//! Tests CLAP plugin parameter processing by:
//! 1. Generating a 440Hz sine wave
//! 2. Processing through CLAP Gain plugin at 250%
//! 3. Verifying the gain is actually applied
//!
//! Usage:
//!   cargo run --example test_clap_gain --features clap-plugins

use streamlib::core::{
    AudioEffectProcessor, AudioFrame,
};
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

/// Calculate RMS (root mean square) level
fn calculate_rms(samples: &[f32]) -> f32 {
    let sum_squares: f32 = samples.iter().map(|s| s * s).sum();
    (sum_squares / samples.len() as f32).sqrt()
}

/// Calculate peak level
fn calculate_peak(samples: &[f32]) -> f32 {
    samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Enable tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    println!("🎵 CLAP Gain Plugin Test");
    println!("========================\n");

    let sample_rate = 48000u32;
    let duration = 1.0; // 1 second
    let input_amplitude = 0.3; // 30% amplitude

    // Generate 440Hz test tone
    println!("📊 Generating 440Hz sine wave...");
    println!("   Sample rate: {}Hz", sample_rate);
    println!("   Duration: {}s", duration);
    println!("   Input amplitude: {:.1}%", input_amplitude * 100.0);

    let mono_samples = generate_sine_wave(sample_rate, duration, input_amplitude);
    let stereo_samples = mono_to_stereo(&mono_samples);

    println!("✅ Generated {} mono samples ({} stereo)", mono_samples.len(), stereo_samples.len());

    // Load CLAP plugin
    let plugin_path = "/Users/fonta/Repositories/tatolab/clap-plugins/build/plugins/clap-plugins.clap/Contents/MacOS/clap-plugins";

    println!("\n🎛️  Loading CLAP Gain plugin...");
    #[cfg(feature = "clap-plugins")]
    let mut plugin = {
        use streamlib::core::processors::ClapEffectProcessor;
        let mut p = ClapEffectProcessor::load_by_name(plugin_path, "Gain")?;

        // Activate first (this enumerates parameters)
        p.activate(sample_rate, 2048)?;

        // List available parameters
        let params = p.list_parameters();
        println!("   Found {} parameter(s):", params.len());
        for param in &params {
            println!("   - [{}] {}: range {:.1} to {:.1} dB (current: {:.2})",
                param.id, param.name, param.min, param.max, param.value);
        }

        // CONFIRMED: ParamValueEvent expects ACTUAL parameter values (dB), not normalized 0-1!
        let gain_value = if let Some(gain_param) = params.first() {
            // Test with +8 dB (should be 2.5x gain = 250%)
            // Formula: gain_multiplier = 10^(dB/20)
            // +8 dB = 10^(8/20) = 10^0.4 ≈ 2.51x
            let actual_db_value = 8.0;  // +8 dB

            println!("\n✅ Testing with actual dB value: +{} dB", actual_db_value);
            println!("   Expected gain: ~2.5x (250%)");
            println!("   Parameter range: {:.1} to {:.1} dB", gain_param.min, gain_param.max);
            actual_db_value
        } else {
            println!("\n⚠️  No parameters found, using default");
            0.0
        };

        // Set the gain parameter using the ID from enumeration
        if !params.is_empty() {
            println!("   Setting parameter ID {} (from enumeration)...", params[0].id);
            p.set_parameter(params[0].id, gain_value)?;
        }

        p
    };

    #[cfg(not(feature = "clap-plugins"))]
    {
        eprintln!("Error: This example requires the 'clap-plugins' feature");
        return Ok(());
    }

    // Process audio through plugin
    println!("\n🔄 Processing audio through gain plugin...");

    let input_frame = AudioFrame::new(
        stereo_samples.clone(),
        0,
        0,
        sample_rate,
        2, // stereo
    );

    #[cfg(feature = "clap-plugins")]
    let output_frame = plugin.process_audio(&input_frame)?;

    #[cfg(not(feature = "clap-plugins"))]
    let output_frame = input_frame.clone();

    println!("✅ Processed {} samples", output_frame.samples.len());

    // Analyze results
    println!("\n📈 Analysis:");

    let input_rms = calculate_rms(&stereo_samples);
    let input_peak = calculate_peak(&stereo_samples);
    let output_rms = calculate_rms(&output_frame.samples);
    let output_peak = calculate_peak(&output_frame.samples);

    let rms_ratio = output_rms / input_rms;
    let peak_ratio = output_peak / input_peak;

    println!("   Input:");
    println!("     RMS:  {:.4} ({:.1}%)", input_rms, input_rms * 100.0);
    println!("     Peak: {:.4} ({:.1}%)", input_peak, input_peak * 100.0);
    println!("\n   Output:");
    println!("     RMS:  {:.4} ({:.1}%)", output_rms, output_rms * 100.0);
    println!("     Peak: {:.4} ({:.1}%)", output_peak, output_peak * 100.0);
    println!("\n   Gain Applied:");
    println!("     RMS ratio:  {:.4}x (expected: 2.5x)", rms_ratio);
    println!("     Peak ratio: {:.4}x (expected: 2.5x)", peak_ratio);

    // Verify gain is within tolerance
    // +8 dB = 10^(8/20) = 2.51x multiplier
    let expected_gain = 2.51;
    let tolerance = 0.1; // 10% tolerance

    let rms_ok = (rms_ratio - expected_gain).abs() < tolerance;
    let peak_ok = (peak_ratio - expected_gain).abs() < tolerance;

    println!("\n✨ Results:");
    if rms_ok && peak_ok {
        println!("   ✅ PASS: Gain correctly applied!");
        println!("   The CLAP parameter events are working properly.");
        println!("   ");
        println!("   🎯 KEY FINDING: ParamValueEvent expects ACTUAL parameter values,");
        println!("      not normalized 0-1 values! For a dB parameter:");
        println!("      - Send the actual dB value (e.g., 8.0 for +8dB)");
        println!("      - NOT the normalized value (e.g., NOT 0.6 for +8dB)");
    } else {
        println!("   ❌ FAIL: Gain not applied correctly");
        println!("   Expected gain: {:.2}x, got RMS: {:.2}x, Peak: {:.2}x",
            expected_gain, rms_ratio, peak_ratio);
    }

    Ok(())
}
