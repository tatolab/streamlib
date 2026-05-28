// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Audio Mixer Demo — canonical reference for the All-Dynamic Package
//! Loading milestone.
//!
//! Demonstrates loading `@tatolab/audio` at runtime via
//! `Runner::add_module` (no Cargo dep on `streamlib-audio`) and wiring
//! its processors via structured `schema_ident!` + JSON config +
//! string-named ports.
//!
//! Packages build automatically on `cargo run` via the build orchestrator.
//! find the staged cdylib at `target/streamlib-plugins/tatolab__audio/`.

use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::schema_ident;

fn main() -> Result<()> {
    println!("\n🎵 Audio Mixer Demo - Mixing Multiple Tones\n");

    println!("🎛️  Creating audio runtime...");
    let runtime = Runner::with_auto_build()?;

    // 1) Load @tatolab/audio (and any deps it walks via patch:) from
    //    the package source. `the build orchestrator`
    //   .
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "audio"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/audio"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    println!("+ @tatolab/audio loaded from target/streamlib-plugins/\n");

    // 2) Chord generator — addressed by structured schema_ident,
    //    configured via JSON payload (matches chord_generator_config.yaml).
    println!("🎹 Adding chord generator (C major chord)...");
    let chord_gen_ident = schema_ident!("tatolab", "audio", "ChordGenerator", "1.0.0");
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
    let speaker_ident = schema_ident!("tatolab", "audio", "AudioOutput", "1.0.0");
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
