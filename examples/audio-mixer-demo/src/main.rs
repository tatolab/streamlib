// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Audio Mixer Demo — a canonical no-load-call streamlib app.
//!
//! Wires `@tatolab/audio`'s ChordGenerator → AudioOutput via
//! `processor_type_ref!` + JSON config + string-named ports. There is no
//! module-loading call: `@tatolab/audio` (and any package it depends on)
//! lives in this app's `streamlib_modules/` folder (populated by
//! `./setup.sh`), and the runtime lazily discovers + loads it on the first
//! `processor_type_ref!` reference. The reference sites carry no version.

use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processor_type_ref;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::RunnerAutoBuild;

fn main() -> Result<()> {
    println!("\n🎵 Audio Mixer Demo - Mixing Multiple Tones\n");

    println!("🎛️  Creating audio runtime...");
    let runtime = Runner::with_auto_build()?;

    // Chord generator — addressed by a version-free processor_type_ref,
    // configured via JSON payload (matches chord_generator_config.yaml). The
    // first reference lazily loads `@tatolab/audio` from streamlib_modules/.
    println!("🎹 Adding chord generator (C major chord)...");
    let chord_gen_ident = processor_type_ref!("tatolab", "audio", "ChordGenerator");
    let chord_gen_config = serde_json::json!({
        // sample_rate / buffer_size are taken from the runtime AudioClock
        // at runtime; the values supplied here are placeholders the
        // processor ignores.
        "sample_rate": 0,
        "buffer_size": 0,
        "amplitude": 0.3,
    });
    let chord_gen = runtime.add_processor(ProcessorSpec::new(chord_gen_ident, chord_gen_config))?;
    println!("   ✅ C4 (261.63 Hz) + E4 (329.63 Hz) + G4 (392.00 Hz)");
    println!("   ✅ Pre-mixed stereo output on port 'chord'\n");

    // 3) Speaker output — default audio device.
    println!("🔊 Adding speaker output...");
    let speaker_ident = processor_type_ref!("tatolab", "audio", "AudioOutput");
    let speaker_config = serde_json::json!({});
    let speaker = runtime.add_processor(ProcessorSpec::new(speaker_ident, speaker_config))?;
    println!("   Using default audio device\n");

    // 4) Connect chord_gen.chord → speaker.audio using runtime-typed
    //    port refs. Schema compatibility is validated at connect time
    //    against the registered processor descriptors.
    println!("🔗 Building audio pipeline...");
    runtime.connect(
        OutputLinkPortRef::new(&chord_gen, "chord"),
        InputLinkPortRef::new(&speaker, "audio"),
    )?;
    println!("   ✅ Chord Generator (stereo) → Speaker\n");

    println!("▶️  Starting audio processing...");
    println!("   Press Ctrl+C to stop\n");
    println!("🎵 You should hear a clean C major chord!\n");

    runtime.start()?;
    runtime.wait_for_signal()?;

    println!("\n\n⏹️  Stopping...");
    println!("✅ Stopped\n");

    Ok(())
}
