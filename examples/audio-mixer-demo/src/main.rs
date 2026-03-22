// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Audio Mixer Demo
//!
//! Demonstrates mixing multiple audio streams using AudioMixerProcessor.
//! Creates three test tones at different frequencies and mixes them into a chord.

use streamlib::{
    input, output, AudioOutputProcessor, ChordGeneratorProcessor, Result, StreamRuntime,
};

fn main() -> Result<()> {

    println!("\n🎵 Audio Mixer Demo - Mixing Multiple Tones\n");

    // Step 1: Create runtime (event-driven, no FPS parameter!)
    println!("🎛️  Creating audio runtime...");
    let runtime = StreamRuntime::new()?;

    // Step 2: Add chord generator (now outputs pre-mixed stereo)
    println!("🎹 Adding chord generator (C major chord)...");
    println!("   Generates stereo output with C4 + E4 + G4 pre-mixed");

    let chord_gen = runtime.add_processor(ChordGeneratorProcessor::node(
        ChordGeneratorProcessor::Config {
            // sample_rate and buffer_size now come from AudioClock (48kHz, 512 samples)
            sample_rate: 0, // Ignored - uses AudioClock
            buffer_size: 0, // Ignored - uses AudioClock
            amplitude: 0.3, // Moderate volume (3 tones will sum)
        },
    ))?;
    println!("   ✅ C4 (261.63 Hz) + E4 (329.63 Hz) + G4 (392.00 Hz)");
    println!("   ✅ Pre-mixed stereo output on port 'chord'");
    println!("   All 3 tones generated from single synchronized source\n");

    // Step 3: Add speaker output
    println!("🔊 Adding speaker output...");
    let speaker =
        runtime.add_processor(AudioOutputProcessor::node(AudioOutputProcessor::Config {
            device_id: None, // Use default speaker
        }))?;
    println!("   Using default audio device\n");

    // Step 4: Connect the audio pipeline using type-safe port markers
    println!("🔗 Building audio pipeline...");

    // Connect ChordGenerator directly to Speaker
    runtime.connect(
        output::<ChordGeneratorProcessor::OutputLink::chord>(&chord_gen),
        input::<AudioOutputProcessor::InputLink::audio>(&speaker),
    )?;
    println!("   ✅ Chord Generator (stereo) → Speaker\n");

    // Step 5: Start the runtime
    println!("▶️  Starting audio processing...");
    println!("   Press Ctrl+C to stop\n");
    println!("🎵 You should hear a clean C major chord!\n");
    println!("💡 Audio pipeline:");
    println!("   • Chord Generator (3 tones pre-mixed: C4 + E4 + G4)");
    println!("     └─ Output 'chord' (stereo with all 3 tones mixed)");
    println!("   • ChordGen → Speaker (direct connection)\n");
    println!("⏰ AudioClock Synchronization:");
    println!("   • ChordGenerator syncs to runtime's AudioClock (48kHz, 512 samples/tick)");
    println!("   • All 3 tones generated in AudioClock tick callbacks");
    println!("   • AudioOutput resamples to device's native rate if needed\n");
    println!("📡 Event-driven architecture:");
    println!("   • No FPS parameter in runtime");
    println!("   • Hardware sources drive the clock");
    println!("   • Type-safe connections verified at compile time\n");

    runtime.start()?;
    runtime.wait_for_signal()?;

    println!("\n\n⏹️  Stopping...");
    println!("✅ Stopped\n");

    Ok(())
}
