//! Audio Mixer Demo
//!
//! Demonstrates mixing multiple audio streams using AudioMixerProcessor.
//! Creates three test tones at different frequencies and mixes them into a chord.

use streamlib::{
    StreamRuntime, AudioMixerProcessor, MixingStrategy,
    ChordGeneratorProcessor, AudioOutputProcessor,
    ClapEffectProcessor,
    Result, AudioFrame,
};
use streamlib::core::sources::chord_generator::ChordGeneratorConfig;
use streamlib::core::transformers::audio_mixer::AudioMixerConfig;
use streamlib::core::transformers::clap_effect::ClapEffectConfig;
use streamlib::core::sinks::audio_output::AudioOutputConfig;
use streamlib::core::traits::StreamProcessor; // Needed for from_config()

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging - use DEBUG level for diagnostics
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    println!("\n🎵 Audio Mixer Demo - Mixing Multiple Tones\n");

    // Step 1: Create runtime (event-driven, no FPS parameter!)
    println!("🎛️  Creating audio runtime...");
    let mut runtime = StreamRuntime::new();
    let audio_config = runtime.audio_config();
    println!("   Sample rate: {} Hz", audio_config.sample_rate);
    println!("   Buffer size: {} samples\n", audio_config.buffer_size);

    // Calculate optimal tick rate to match audio buffer size
    // This ensures we generate exactly the right number of samples per tick
    // to match CoreAudio's consumption rate, eliminating buffer underruns
    let tick_rate = audio_config.sample_rate as f64 / audio_config.buffer_size as f64;
    println!("   Optimal tick rate: {:.2} Hz (matches buffer size)", tick_rate);

    // Step 2: Add chord generator (emulates a 3-channel microphone array)
    // Single processor generates all 3 tones simultaneously, like a mic array
    println!("🎹 Adding chord generator (C major chord)...");
    println!("   Emulates 3-channel microphone array pattern");

    let chord_gen = runtime.add_element_with_config::<ChordGeneratorProcessor>(
        ChordGeneratorConfig {
            amplitude: 0.5,              // 50% volume (louder for more noticeable reverb!)
        }
    ).await?;
    println!("   ✅ C4 (261.63 Hz) on output 'tone_c4'");
    println!("   ✅ E4 (329.63 Hz) on output 'tone_e4'");
    println!("   ✅ G4 (392.00 Hz) on output 'tone_g4'");
    println!("   All 3 tones generated from single synchronized source\n");

    // Step 3: Add audio mixer (3 mono inputs → 1 stereo output)
    println!("🔀 Adding audio mixer...");
    let mixer = runtime.add_element_with_config::<AudioMixerProcessor<3>>(
        AudioMixerConfig {
            strategy: MixingStrategy::SumNormalized, // Prevents clipping
        }
    ).await?;
    println!("   Strategy: Sum Clipped (prevents distortion)");
    println!("   Inputs: 3 mono signals");
    println!("   Output: Stereo signal at {} Hz\n", audio_config.sample_rate);

    // Step 4: Add CLAP effect chain (4x stacked for extreme processing!)
    // TODO: Set different parameters on each instance to select different effect types
    // The plugin logs will show all 13 available parameters with their IDs
    println!("🎚️  Adding CLAP effect chain (4x Surge XT Effects stacked)...");

    // Effect 1 - Plugin index 0
    let effect1 = runtime.add_element_with_config::<ClapEffectProcessor>(
        ClapEffectConfig {
            plugin_path: "/Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap".into(),
            plugin_name: None,
            plugin_index: Some(0),
        }
    ).await?;
    println!("   ✅ Effect 1: Surge XT Effects (default settings)");

    // Effect 2 - Same plugin (index 0)
    let effect2 = runtime.add_element_with_config::<ClapEffectProcessor>(
        ClapEffectConfig {
            plugin_path: "/Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap".into(),
            plugin_name: None,
            plugin_index: Some(0),
        }
    ).await?;
    println!("   ✅ Effect 2: Surge XT Effects");

    // Effect 3 - Same plugin (index 0)
    let effect3 = runtime.add_element_with_config::<ClapEffectProcessor>(
        ClapEffectConfig {
            plugin_path: "/Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap".into(),
            plugin_name: None,
            plugin_index: Some(0),
        }
    ).await?;
    println!("   ✅ Effect 3: Surge XT Effects");

    // Effect 4 - Same plugin (index 0)
    let effect4 = runtime.add_element_with_config::<ClapEffectProcessor>(
        ClapEffectConfig {
            plugin_path: "/Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap".into(),
            plugin_name: None,
            plugin_index: Some(0),
        }
    ).await?;
    println!("   ✅ Effect 4: Surge XT Effects");
    println!("   🔗 Chain: Mixer → Effect1 → Effect2 → Effect3 → Effect4 → Speaker");
    println!("   💥 4x STACKED EFFECTS = Maximum Obnoxiousness!\n");

    // Step 5: Add speaker output
    println!("🔊 Adding speaker output...");
    let speaker = runtime.add_element_with_config::<AudioOutputProcessor>(
        AudioOutputConfig {
            device_id: None, // Use default speaker
        }
    ).await?;
    println!("   Using default audio device\n");

    // Step 6: Connect the audio pipeline using type-safe handles
    println!("🔗 Building audio pipeline...");

    // Connect chord generator's 3 mono outputs to mixer inputs (like a mic array)
    runtime.connect(
        chord_gen.output_port::<AudioFrame<1>>("tone_c4"),
        mixer.input_port::<AudioFrame<1>>("input_0"),
    )?;
    println!("   ✅ Chord Generator (C4 mono) → Mixer Input 0");

    runtime.connect(
        chord_gen.output_port::<AudioFrame<1>>("tone_e4"),
        mixer.input_port::<AudioFrame<1>>("input_1"),
    )?;
    println!("   ✅ Chord Generator (E4 mono) → Mixer Input 1");

    runtime.connect(
        chord_gen.output_port::<AudioFrame<1>>("tone_g4"),
        mixer.input_port::<AudioFrame<1>>("input_2"),
    )?;
    println!("   ✅ Chord Generator (G4 mono) → Mixer Input 2");

    // Connect effect chain: Mixer → Effect1 → Effect2 → Effect3 → Effect4 → Speaker
    runtime.connect(
        mixer.output_port::<AudioFrame<2>>("audio"),
        effect1.input_port::<AudioFrame<2>>("audio"),
    )?;
    println!("   ✅ Mixer (stereo) → Effect1");

    runtime.connect(
        effect1.output_port::<AudioFrame<2>>("audio"),
        effect2.input_port::<AudioFrame<2>>("audio"),
    )?;
    println!("   ✅ Effect1 → Effect2");

    runtime.connect(
        effect2.output_port::<AudioFrame<2>>("audio"),
        effect3.input_port::<AudioFrame<2>>("audio"),
    )?;
    println!("   ✅ Effect2 → Effect3");

    runtime.connect(
        effect3.output_port::<AudioFrame<2>>("audio"),
        effect4.input_port::<AudioFrame<2>>("audio"),
    )?;
    println!("   ✅ Effect3 → Effect4");

    runtime.connect(
        effect4.output_port::<AudioFrame<2>>("audio"),
        speaker.input_port::<AudioFrame<2>>("audio"),
    )?;
    println!("   ✅ Effect4 → Speaker\n");

    // Step 7: Start the runtime
    println!("▶️  Starting audio processing...");
    println!("   Press Ctrl+C to stop\n");
    println!("🎵 You should hear a C major chord with INSANE effects!\n");
    println!("💡 Audio pipeline:");
    println!("   • Chord Generator (3 tones: C4 + E4 + G4)");
    println!("     ├─ Output 'tone_c4' (262 Hz) → Mixer Input 0");
    println!("     ├─ Output 'tone_e4' (330 Hz) → Mixer Input 1");
    println!("     └─ Output 'tone_g4' (392 Hz) → Mixer Input 2");
    println!("   • Mixer → Effect[0] → Effect[1] → Effect[2] → Effect[3] → Speaker");
    println!("   🌊 4 DIFFERENT SURGE XT EFFECTS IN SERIES = Maximum Obnoxiousness!\n");
    println!("⏰ Clock Synchronization:");
    println!("   • Single hardware-like source @ {:.2} Hz", tick_rate);
    println!("   • All 3 tones generated simultaneously (one callback)");
    println!("   • Zero clock drift between channels (like real mic array)");
    println!("   • Demonstrates multi-output source pattern\n");
    println!("📡 Event-driven architecture:");
    println!("   • No FPS parameter in runtime");
    println!("   • Hardware sources drive the clock");
    println!("   • Type-safe connections verified at compile time\n");

    runtime.start().await?;

    // Run until interrupted
    tokio::signal::ctrl_c().await?;

    println!("\n\n⏹️  Stopping...");
    runtime.stop().await?;
    println!("✅ Stopped\n");

    Ok(())
}
