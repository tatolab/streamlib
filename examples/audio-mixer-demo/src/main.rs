//! Audio Mixer Demo
//!
//! Demonstrates mixing multiple audio streams using AudioMixerProcessor.
//! Creates three test tones at different frequencies and mixes them into a chord.

use streamlib::core::{AudioOutputConfig, ChordGeneratorConfig};
use streamlib::{AudioFrame, AudioOutputProcessor, ChordGeneratorProcessor, Result, StreamRuntime};

fn main() -> Result<()> {
    // Initialize logging - use DEBUG level for diagnostics
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    println!("\n🎵 Audio Mixer Demo - Mixing Multiple Tones\n");

    // Step 1: Create runtime (event-driven, no FPS parameter!)
    println!("🎛️  Creating audio runtime...");
    let mut runtime = StreamRuntime::new();

    // Step 2: Add chord generator (now outputs pre-mixed stereo)
    println!("🎹 Adding chord generator (C major chord)...");
    println!("   Generates stereo output with C4 + E4 + G4 pre-mixed");

    let chord_gen = runtime
        .add_processor_with_config::<ChordGeneratorProcessor>(ChordGeneratorConfig::default())?;
    println!("   ✅ C4 (261.63 Hz) + E4 (329.63 Hz) + G4 (392.00 Hz)");
    println!("   ✅ Pre-mixed stereo output on port 'chord'");
    println!("   All 3 tones generated from single synchronized source\n");

    // Step 3: Add audio mixer (3 mono inputs → 1 stereo output)
    // NOTE: Commented out - now using ChordGenerator's pre-mixed stereo output
    // println!("🔀 Adding audio mixer...");
    // let mixer = runtime.add_element_with_config::<AudioMixerProcessor<3>>(
    //     AudioMixerConfig {
    //         strategy: MixingStrategy::Sum,
    //         timestamp_tolerance_ms: Some(1u32), // 1 ms tolerance for sync
    //     }
    // ).await?;
    // println!("   Strategy: Sum (prevents distortion)");
    // println!("   Inputs: 3 mono signals");
    // println!("   Output: Stereo signal at {} Hz\n", audio_config.sample_rate);

    // Step 4: Add CLAP effect chain (4x stacked for extreme processing!)
    // NOTE: Commented out - connecting ChordGenerator directly to speaker
    // println!("🎚️  Adding CLAP effect chain (4x Surge XT Effects stacked)...");
    //
    // // Effect 1 - Plugin index 0
    // let effect1 = runtime.add_element_with_config::<ClapEffectProcessor>(
    //     ClapEffectConfig {
    //         plugin_path: "/Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap".into(),
    //         plugin_name: None,
    //         plugin_index: Some(0),
    //     }
    // ).await?;
    // println!("   ✅ Effect 1: Surge XT Effects (default settings)");
    //
    // // Effect 2 - Same plugin (index 0)
    // let effect2 = runtime.add_element_with_config::<ClapEffectProcessor>(
    //     ClapEffectConfig {
    //         plugin_path: "/Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap".into(),
    //         plugin_name: None,
    //         plugin_index: Some(0),
    //     }
    // ).await?;
    // println!("   ✅ Effect 2: Surge XT Effects");
    //
    // // Effect 3 - Same plugin (index 0)
    // let effect3 = runtime.add_element_with_config::<ClapEffectProcessor>(
    //     ClapEffectConfig {
    //         plugin_path: "/Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap".into(),
    //         plugin_name: None,
    //         plugin_index: Some(0),
    //     }
    // ).await?;
    // println!("   ✅ Effect 3: Surge XT Effects");
    //
    // // Effect 4 - Same plugin (index 0)
    // let effect4 = runtime.add_element_with_config::<ClapEffectProcessor>(
    //     ClapEffectConfig {
    //         plugin_path: "/Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap".into(),
    //         plugin_name: None,
    //         plugin_index: Some(0),
    //     }
    // ).await?;
    // println!("   ✅ Effect 4: Surge XT Effects");
    // println!("   🔗 Chain: Mixer → Effect1 → Effect2 → Effect3 → Effect4 → Speaker");
    // println!("   💥 4x STACKED EFFECTS = Maximum Obnoxiousness!\n");

    // Step 5: Add speaker output
    println!("🔊 Adding speaker output...");
    let speaker = runtime.add_processor_with_config::<AudioOutputProcessor>(AudioOutputConfig {
        device_id: None, // Use default speaker
    })?;
    println!("   Using default audio device\n");

    // Step 6: Connect the audio pipeline using type-safe handles
    println!("🔗 Building audio pipeline...");

    // Connect chord generator's pre-mixed stereo output directly to effects
    // NOTE: Old mixer connections commented out
    // runtime.connect(
    //     chord_gen.output_port::<AudioFrame<1>>("tone_c4"),
    //     mixer.input_port::<AudioFrame<1>>("input_0"),
    // )?;
    // println!("   ✅ Chord Generator (C4 mono) → Mixer Input 0");
    //
    // runtime.connect(
    //     chord_gen.output_port::<AudioFrame<1>>("tone_e4"),
    //     mixer.input_port::<AudioFrame<1>>("input_1"),
    // )?;
    // println!("   ✅ Chord Generator (E4 mono) → Mixer Input 1");
    //
    // runtime.connect(
    //     chord_gen.output_port::<AudioFrame<1>>("tone_g4"),
    //     mixer.input_port::<AudioFrame<1>>("input_2"),
    // )?;
    // println!("   ✅ Chord Generator (G4 mono) → Mixer Input 2");

    // Connect ChordGenerator directly to Speaker (bypassing effects)
    runtime.connect(
        chord_gen.output_port::<AudioFrame<2>>("chord"),
        speaker.input_port::<AudioFrame<2>>("audio"),
    )?;
    println!("   ✅ Chord Generator (stereo) → Speaker\n");

    // Old effect chain connections (commented out)
    // runtime.connect(
    //     chord_gen.output_port::<AudioFrame<2>>("chord"),
    //     effect1.input_port::<AudioFrame<2>>("audio"),
    // )?;
    // println!("   ✅ Chord Generator (stereo) → Effect1");
    //
    // runtime.connect(
    //     effect1.output_port::<AudioFrame<2>>("audio"),
    //     effect2.input_port::<AudioFrame<2>>("audio"),
    // )?;
    // println!("   ✅ Effect1 → Effect2");
    //
    // runtime.connect(
    //     effect2.output_port::<AudioFrame<2>>("audio"),
    //     effect3.input_port::<AudioFrame<2>>("audio"),
    // )?;
    // println!("   ✅ Effect2 → Effect3");
    //
    // runtime.connect(
    //     effect3.output_port::<AudioFrame<2>>("audio"),
    //     effect4.input_port::<AudioFrame<2>>("audio"),
    // )?;
    // println!("   ✅ Effect3 → Effect4");
    //
    // runtime.connect(
    //     effect4.output_port::<AudioFrame<2>>("audio"),
    //     speaker.input_port::<AudioFrame<2>>("audio"),
    // )?;
    // println!("   ✅ Effect4 → Speaker\n");

    // Step 7: Start the runtime
    println!("▶️  Starting audio processing...");
    println!("   Press Ctrl+C to stop\n");
    println!("🎵 You should hear a clean C major chord!\n");
    println!("💡 Audio pipeline:");
    println!("   • Chord Generator (3 tones pre-mixed: C4 + E4 + G4)");
    println!("     └─ Output 'chord' (stereo with all 3 tones mixed)");
    println!("   • ChordGen → Speaker (direct connection, no effects)\n");
    println!("⏰ Clock Synchronization:");

    println!("   • All 3 tones generated and mixed in single callback");
    println!("   • Zero mixing overhead - pre-mixed stereo output");
    println!("   • Demonstrates single-output source pattern\n");
    println!("📡 Event-driven architecture:");
    println!("   • No FPS parameter in runtime");
    println!("   • Hardware sources drive the clock");
    println!("   • Type-safe connections verified at compile time\n");

    runtime.start()?;
    runtime.run()?;

    println!("\n\n⏹️  Stopping...");

    println!("✅ Stopped\n");

    Ok(())
}
