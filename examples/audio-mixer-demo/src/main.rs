//! Audio Mixer Demo
//!
//! Demonstrates mixing multiple audio streams using AudioMixerProcessor.
//! Creates three test tones at different frequencies and mixes them into a chord.

use streamlib::{
    StreamRuntime, AudioMixerProcessor, MixingStrategy, ChannelMode,
    TestToneGenerator, AudioOutputProcessor,
    ClapEffectProcessor, AudioFrame,
    Result,
};
use streamlib::core::sources::test_tone_source::TestToneConfig;
use streamlib::core::transformers::audio_mixer::AudioMixerConfig;
use streamlib::core::transformers::clap_effect::ClapEffectConfig;
use streamlib::core::sinks::audio_output::AudioOutputConfig;

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
    println!("   Buffer size: {} samples", audio_config.buffer_size);
    println!("   Channels: {}\n", audio_config.channels);

    // Calculate optimal tick rate to match audio buffer size
    // This ensures we generate exactly the right number of samples per tick
    // to match CoreAudio's consumption rate, eliminating buffer underruns
    let tick_rate = audio_config.sample_rate as f64 / audio_config.buffer_size as f64;
    println!("   Optimal tick rate: {:.2} Hz (matches buffer size)", tick_rate);

    // Step 2: Add test tone generators (A major chord: A4, C#5, E5)
    // All generators share the same timer group for synchronized wake-ups
    println!("🎹 Adding test tone generators...");
    println!("   Using timer group 'audio_master' for zero-drift synchronization");

    // A4 (440 Hz)
    let tone1 = runtime.add_element_with_config::<TestToneGenerator>(
        TestToneConfig {
            frequency: 440.0,             // A4
            amplitude: 0.15,              // 15% volume (quiet to avoid clipping)
        }
    ).await?;
    println!("   ✅ Tone 1: 440.00 Hz (A4)");

    // C#5 (554.37 Hz)
    let tone2 = runtime.add_element_with_config::<TestToneGenerator>(
        TestToneConfig {
            frequency: 554.37,            // C#5
            amplitude: 0.15,
        }
    ).await?;
    println!("   ✅ Tone 2: 554.37 Hz (C#5)");

    // E5 (659.25 Hz)
    let tone3 = runtime.add_element_with_config::<TestToneGenerator>(
        TestToneConfig {
            frequency: 659.25,            // E5
            amplitude: 0.15,
        }
    ).await?;
    println!("   ✅ Tone 3: 659.25 Hz (E5)\n");

    // Step 3: Add audio mixer
    println!("🔀 Adding audio mixer...");
    let mixer = runtime.add_element_with_config::<AudioMixerProcessor>(
        AudioMixerConfig {
            num_inputs: 3,                        // 3 inputs
            strategy: MixingStrategy::Sum, // Prevents clipping
            channel_mode: ChannelMode::MixUp,     // Mix up to stereo
        }
    ).await?;
    println!("   Strategy: Sum Normalized (no clipping)");
    println!("   Inputs: 3");
    println!("   Timer Group: 'audio_master' (synchronized with generators)");
    println!("   Output: Stereo at {} Hz\n", audio_config.sample_rate);

    // Step 4: Add CLAP reverb effect
    println!("🎚️  Adding CLAP reverb effect...");
    let reverb = runtime.add_element_with_config::<ClapEffectProcessor>(
        ClapEffectConfig {
            plugin_path: "/Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap".into(),
            plugin_name: None,  // Use first plugin in bundle
            sample_rate: audio_config.sample_rate,
            buffer_size: audio_config.buffer_size,
        }
    ).await?;
    println!("   Loaded: Surge XT Effect (first in bundle)");
    println!("   Plugin will activate on runtime start\n");

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

    // Connect tone generators to mixer inputs
    runtime.connect(
        tone1.output_port::<AudioFrame>("audio"),
        mixer.input_port::<AudioFrame>("input_0"),
    )?;
    println!("   ✅ Tone 1 → Mixer Input 0");

    runtime.connect(
        tone2.output_port::<AudioFrame>("audio"),
        mixer.input_port::<AudioFrame>("input_1"),
    )?;
    println!("   ✅ Tone 2 → Mixer Input 1");

    runtime.connect(
        tone3.output_port::<AudioFrame>("audio"),
        mixer.input_port::<AudioFrame>("input_2"),
    )?;
    println!("   ✅ Tone 3 → Mixer Input 2");

    // Connect mixer output to reverb input
    runtime.connect(
        mixer.output_port::<AudioFrame>("audio"),
        reverb.input_port::<AudioFrame>("audio"),
    )?;
    println!("   ✅ Mixer → Reverb");

    // Connect reverb output to speaker
    runtime.connect(
        reverb.output_port::<AudioFrame>("audio"),
        speaker.input_port::<AudioFrame>("audio"),
    )?;
    println!("   ✅ Reverb → Speaker\n");

    // Step 7: Start the runtime
    println!("▶️  Starting audio processing...");
    println!("   Press Ctrl+C to stop\n");
    println!("🎵 You should hear an A major chord (A4 + C#5 + E5) with reverb!\n");
    println!("💡 Audio pipeline:");
    println!("   • 440 Hz (A4)  → Mixer Input 0");
    println!("   • 554 Hz (C#5) → Mixer Input 1");
    println!("   • 659 Hz (E5)  → Mixer Input 2");
    println!("   • Mixed → CLAP Reverb → Speaker\n");
    println!("⏰ Timer Groups (Clock Domains):");
    println!("   • Group: 'audio_master' @ {:.2} Hz", tick_rate);
    println!("   • Members: Tone 1, Tone 2, Tone 3, Mixer");
    println!("   • All processors wake simultaneously (zero clock drift)");
    println!("   • Inspired by GStreamer pipeline clocks & PipeWire graph timing\n");
    println!("📡 Event-driven architecture:");
    println!("   • No FPS parameter in runtime");
    println!("   • Timer groups for synchronized sources");
    println!("   • Type-safe connections verified at compile time\n");

    runtime.start().await?;

    // Run until interrupted
    tokio::signal::ctrl_c().await?;

    println!("\n\n⏹️  Stopping...");
    runtime.stop().await?;
    println!("✅ Stopped\n");

    Ok(())
}
