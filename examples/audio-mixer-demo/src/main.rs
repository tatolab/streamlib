//! Audio Mixer Demo
//!
//! Demonstrates mixing multiple audio streams using AudioMixerProcessor.
//! Creates three test tones at different frequencies and mixes them into a chord.

use streamlib::{
    StreamRuntime, AudioMixerProcessor, MixingStrategy,
    TestToneGenerator, AudioOutputProcessor,
    ClapEffectProcessor,  // Add CLAP effect support
    Result,
};

// Import traits to access their methods
use streamlib::core::{
    AudioOutputProcessor as AudioOutputProcessorTrait,
    AudioEffectProcessor as AudioEffectProcessorTrait,  // For CLAP activation
};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("\n🎵 Audio Mixer Demo - Mixing Multiple Tones\n");

    // Step 1: Create runtime with audio configuration
    println!("🎛️  Creating audio runtime...");
    let mut runtime = StreamRuntime::new(60.0); // 60 FPS tick rate
    let audio_config = runtime.audio_config();
    println!("   Sample rate: {} Hz", audio_config.sample_rate);
    println!("   Buffer size: {} samples", audio_config.buffer_size);
    println!("   Channels: {}\n", audio_config.channels);

    // Step 2: Create test tone generators (A major chord: A4, C#5, E5)
    println!("🎹 Creating test tone generators...");

    // A4 (440 Hz) - Left channel louder
    let mut tone1 = TestToneGenerator::new(
        440.0,                        // Frequency: A4
        audio_config.sample_rate,
        60.0,                         // Tick rate: 60 FPS
        0.15,                         // Volume: 15% (quiet to avoid clipping)
    );
    println!("   ✅ Tone 1: 440.00 Hz (A4)  - Left channel");

    // C#5 (554.37 Hz) - Right channel louder
    let mut tone2 = TestToneGenerator::new(
        554.37,                       // Frequency: C#5
        audio_config.sample_rate,
        60.0,                         // Tick rate: 60 FPS
        0.15,                         // Volume: 15%
    );
    println!("   ✅ Tone 2: 554.37 Hz (C#5) - Right channel");

    // E5 (659.25 Hz) - Centered
    let mut tone3 = TestToneGenerator::new(
        659.25,                       // Frequency: E5
        audio_config.sample_rate,
        60.0,                         // Tick rate: 60 FPS
        0.15,                         // Volume: 15%
    );
    println!("   ✅ Tone 3: 659.25 Hz (E5)  - Center\n");

    // Step 3: Create audio mixer
    println!("🔀 Creating audio mixer...");
    let mut mixer = AudioMixerProcessor::new(
        3,                                    // 3 inputs
        MixingStrategy::SumNormalized,        // Prevents clipping
        audio_config.sample_rate,
    )?;
    println!("   Strategy: Sum Normalized (no clipping)");
    println!("   Inputs: 3");
    println!("   Output: Stereo at {} Hz\n", audio_config.sample_rate);

    // Step 4: Create CLAP reverb effect
    println!("🎚️  Loading CLAP reverb effect...");
    let mut reverb = ClapEffectProcessor::load_by_name(
        "/Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap",
        "Reverb 1"
    )?;

    // Activate with runtime's audio config for consistency
    // NOTE: We're passing config.buffer_size (2048) but will receive 800-sample frames
    // This tests if CLAP plugins handle variable buffer sizes correctly!
    reverb.activate(audio_config.sample_rate, audio_config.buffer_size)?;
    println!("   Loaded: Surge XT Reverb");
    println!("   Activated at {}Hz, max {} frames", audio_config.sample_rate, audio_config.buffer_size);
    println!("   ⚠️  Testing: Will receive 800-sample frames (60 FPS) despite max 2048!\n");

    // Step 5: Create speaker output
    println!("🔊 Setting up speaker output...");
    let mut speaker = AudioOutputProcessor::new(None)?;
    println!("   Using: {}\n", speaker.current_device().name);

    // Step 5: Connect the audio pipeline
    println!("🔗 Building audio pipeline...");

    // Connect tone generators to mixer inputs
    runtime.connect(
        &mut tone1.output_ports().audio,
        &mut mixer.input_ports().inputs.get_mut("input_0").unwrap().lock()
    )?;
    println!("   ✅ Tone 1 → Mixer Input 0");

    runtime.connect(
        &mut tone2.output_ports().audio,
        &mut mixer.input_ports().inputs.get_mut("input_1").unwrap().lock()
    )?;
    println!("   ✅ Tone 2 → Mixer Input 1");

    runtime.connect(
        &mut tone3.output_ports().audio,
        &mut mixer.input_ports().inputs.get_mut("input_2").unwrap().lock()
    )?;
    println!("   ✅ Tone 3 → Mixer Input 2");

    // Connect mixer output to reverb input
    runtime.connect(
        &mut mixer.output_ports().audio,
        &mut reverb.input_ports().audio
    )?;
    println!("   ✅ Mixer → Reverb");

    // Connect reverb output to speaker
    runtime.connect(
        &mut reverb.output_ports().audio,
        &mut speaker.input_ports().audio
    )?;
    println!("   ✅ Reverb → Speaker\n");

    // Step 6: Add all processors to runtime
    runtime.add_processor(Box::new(tone1));
    runtime.add_processor(Box::new(tone2));
    runtime.add_processor(Box::new(tone3));
    runtime.add_processor(Box::new(mixer));
    runtime.add_processor(Box::new(reverb));  // Add CLAP reverb
    runtime.add_processor(Box::new(speaker));

    // Step 7: Start the runtime
    println!("▶️  Starting audio processing...");
    println!("   Press Ctrl+C to stop\n");
    println!("🎵 You should hear an A major chord (A4 + C#5 + E5) with reverb!\n");
    println!("💡 Audio pipeline:");
    println!("   • 440 Hz (A4)  → Mixer (Left)");
    println!("   • 554 Hz (C#5) → Mixer (Right)");
    println!("   • 659 Hz (E5)  → Mixer (Center)");
    println!("   • Mixed → CLAP Reverb → Speaker\n");
    println!("🧪 Testing buffer size compatibility:");
    println!("   • TestToneGenerator: 800 samples/frame (60 FPS)");
    println!("   • CLAP plugin activated: max 2048 samples");
    println!("   • Actual frames received: 800 samples\n");

    runtime.start().await?;

    // Run until interrupted
    tokio::signal::ctrl_c().await?;

    println!("\n\n⏹️  Stopping...");
    runtime.stop().await?;
    println!("✅ Stopped\n");

    Ok(())
}
