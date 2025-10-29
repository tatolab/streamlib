//! Microphone → CLAP Reverb → Speaker Example
//!
//! Demonstrates streamlib's audio processing pipeline using CLAP as the "shader language for audio".
//! Just as video shaders transform pixels on GPU, CLAP plugins transform audio in real-time.

use streamlib::{
    StreamRuntime, ClapEffectProcessor, ClapScanner,
    AudioCaptureProcessor, AudioOutputProcessor,
    Result,
};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("\n🎙️  Microphone → CLAP Reverb → Speaker Example\n");

    // Step 1: Scan for installed CLAP plugins
    println!("🔍 Scanning for installed CLAP plugins...");
    let plugins = ClapScanner::scan_system_plugins()?;

    if plugins.is_empty() {
        eprintln!("❌ No CLAP plugins found!");
        eprintln!("\nPlease install a CLAP plugin:");
        eprintln!("  • Surge XT Effects: https://surge-synthesizer.github.io/");
        eprintln!("  • Airwindows Consolidated: https://github.com/baconpaul/airwin2rack");
        eprintln!("\nInstallation paths:");
        eprintln!("  macOS: ~/Library/Audio/Plug-Ins/CLAP/");
        eprintln!("  Linux: ~/.clap/ or /usr/lib/clap/");
        eprintln!("  Windows: %COMMONPROGRAMFILES%\\CLAP\\");
        return Ok(());
    }

    println!("✅ Found {} CLAP plugins:", plugins.len());
    for (i, plugin) in plugins.iter().enumerate().take(10) {
        println!("   [{}] {} by {}", i, plugin.name, plugin.vendor);
    }

    // Step 2: Find a reverb plugin
    println!("\n🔍 Looking for reverb plugin...");
    let reverb_plugin = plugins.iter()
        .find(|p| {
            let name_lower = p.name.to_lowercase();
            name_lower.contains("reverb") ||
            name_lower.contains("verb") ||
            p.features.iter().any(|f| f.to_lowercase().contains("reverb"))
        });

    let plugin_path = match reverb_plugin {
        Some(plugin) => {
            println!("✅ Using: {} by {}", plugin.name, plugin.vendor);
            println!("   Path: {}", plugin.path.display());
            plugin.path.clone()
        }
        None => {
            println!("⚠️  No reverb plugin found, using first available plugin...");
            let first = &plugins[0];
            println!("   Using: {} by {}", first.name, first.vendor);
            first.path.clone()
        }
    };

    // Step 3: Create runtime with audio configuration
    println!("\n🎛️  Creating audio runtime...");
    let mut runtime = StreamRuntime::new(60.0); // 60 FPS tick rate
    let audio_config = runtime.audio_config();
    println!("   Sample rate: {} Hz", audio_config.sample_rate);
    println!("   Buffer size: {} samples", audio_config.buffer_size);
    println!("   Channels: {}", audio_config.channels);

    // Step 4: Create microphone input
    println!("\n🎤 Setting up microphone input...");
    let mic = AudioCaptureProcessor::new(
        None, // Use default mic
        audio_config.sample_rate,
        audio_config.channels,
    )?;
    println!("✅ Using microphone: {}", mic.current_device().name);

    // Step 5: Load CLAP reverb plugin
    println!("\n🎛️  Loading CLAP plugin...");
    let mut reverb = ClapEffectProcessor::load(&plugin_path)?;
    println!("✅ Plugin loaded: {}", reverb.plugin_info().name);

    // Activate plugin with runtime's audio config
    println!("   Activating plugin...");
    reverb.activate(audio_config.sample_rate, audio_config.buffer_size)?;
    println!("✅ Plugin activated");

    // List and set parameters
    let params = reverb.list_parameters();
    println!("   Plugin has {} parameters", params.len());

    // Try to set room size or mix if available
    for param in params.iter().take(20) {
        let name_lower = param.name.to_lowercase();
        if name_lower.contains("mix") || name_lower.contains("wet") {
            // Set mix to 30% for subtle reverb
            reverb.set_parameter(param.id, 0.3)?;
            println!("   Set {}: 30%", param.name);
        } else if name_lower.contains("size") || name_lower.contains("room") {
            // Set room size to 60%
            reverb.set_parameter(param.id, 0.6)?;
            println!("   Set {}: 60%", param.name);
        }
    }

    // Step 6: Create speaker output
    println!("\n🔊 Setting up speaker output...");
    let speaker = AudioOutputProcessor::new(None)?; // Use default speaker
    println!("✅ Using speaker: {}", speaker.current_device().name);

    // Step 7: Add processors to runtime
    println!("\n🔗 Building audio pipeline...");
    let mic_id = runtime.add_processor(Box::new(mic));
    let reverb_id = runtime.add_processor(Box::new(reverb));
    let speaker_id = runtime.add_processor(Box::new(speaker));

    // Step 8: Connect the pipeline
    runtime.connect(&format!("{}.audio", mic_id), &format!("{}.input", reverb_id))?;
    runtime.connect(&format!("{}.output", reverb_id), &format!("{}.audio", speaker_id))?;

    println!("✅ Pipeline connected:");
    println!("   {} (mic) → {} (reverb) → {} (speaker)", mic_id, reverb_id, speaker_id);

    // Step 9: Start the runtime
    println!("\n▶️  Starting audio processing...");
    println!("   Press Ctrl+C to stop\n");
    println!("🎙️  Speak into your microphone - you should hear yourself with reverb!\n");

    runtime.start().await?;

    // Run until interrupted
    tokio::signal::ctrl_c().await?;

    println!("\n\n⏹️  Stopping...");
    runtime.stop().await?;
    println!("✅ Stopped\n");

    Ok(())
}
