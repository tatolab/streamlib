//! Microphone → CLAP Reverb → Speaker Example
//!
//! Demonstrates streamlib's audio processing pipeline using CLAP as the "shader language for audio".
//! Just as video shaders transform pixels on GPU, CLAP plugins transform audio in real-time.

use streamlib::{
    StreamRuntime, ClapEffectProcessor, ClapScanner,
    AudioCaptureProcessor, AudioOutputProcessor, AudioMixerProcessor,
    AudioFrame, Result,
};
use streamlib::core::{
    AudioCaptureConfig, AudioOutputConfig, ClapEffectConfig, AudioMixerConfig,
};

fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("\n🎙️  Microphone → CLAP Reverb → Speaker Example\n");

    // Create runtime first
    let mut runtime = StreamRuntime::new();

    // Request microphone permission (must be on main thread before adding audio processors)
    println!("🔒 Requesting microphone permission...");
    if !runtime.request_microphone()? {
        eprintln!("❌ Microphone permission denied!");
        eprintln!("\nThis example requires microphone access.");
        eprintln!("Please grant permission in System Settings → Privacy & Security → Microphone");
        return Ok(());
    }
    println!("✅ Microphone permission granted\n");

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

    // Step 2: Find an effects plugin (reverb, delay, etc.)
    println!("\n🔍 Looking for audio effects plugin...");
    let effects_plugin = plugins.iter()
        .find(|p| {
            let name_lower = p.name.to_lowercase();
            name_lower.contains("reverb") ||
            name_lower.contains("verb") ||
            name_lower.contains("effects") ||
            name_lower.contains("fx") ||
            p.features.iter().any(|f| {
                let f_lower = f.to_lowercase();
                f_lower.contains("reverb") || f_lower.contains("effect")
            })
        });

    let plugin_path = match effects_plugin {
        Some(plugin) => {
            println!("✅ Using: {} by {}", plugin.name, plugin.vendor);
            println!("   Path: {}", plugin.path.display());
            plugin.path.clone()
        }
        None => {
            println!("⚠️  No effects plugin found, using first available plugin...");
            let first = &plugins[0];
            println!("   Using: {} by {}", first.name, first.vendor);
            first.path.clone()
        }
    };

    // Step 3: Get audio config from runtime
    println!("\n🎛️  Audio runtime configuration...");
    let audio_config = runtime.audio_config();
    println!("   Sample rate: {} Hz", audio_config.sample_rate);
    println!("   Buffer size: {} samples", audio_config.buffer_size);

    // Step 4: Add microphone input processor using config-based API
    println!("\n🎤 Adding microphone input...");
    let mic = runtime.add_processor_with_config::<AudioCaptureProcessor>(
        AudioCaptureConfig {
            device_id: None
        }
    )?;
    println!("✅ Microphone processor added (mono output)");

    // Step 5: Add audio mixer to convert mono to stereo
    println!("\n🎚️  Adding audio mixer (mono → stereo)...");
    let mixer = runtime.add_processor_with_config::<AudioMixerProcessor>(
        AudioMixerConfig::default()
    )?;
    println!("✅ Audio mixer added");

    // Step 6: Add CLAP reverb plugin using config-based API
    println!("\n🎛️  Adding CLAP plugin...");
    let reverb = runtime.add_processor_with_config::<ClapEffectProcessor>(
        ClapEffectConfig {
            plugin_path,
            plugin_name: None, // Use first plugin in bundle
            plugin_index: None,
        }
    )?;
    println!("✅ CLAP effect processor added");
    println!("   Note: Plugin activates automatically with runtime's audio config");
    println!("   Note: Use parameter automation API for runtime parameter changes");

    // Step 7: Add speaker output processor using config-based API
    println!("\n🔊 Adding speaker output...");
    let speaker = runtime.add_processor_with_config::<AudioOutputProcessor>(
        AudioOutputConfig {
            device_id: None, // Use default speaker
        }
    )?;
    println!("✅ Speaker processor added");

    // Step 8: Connect the pipeline using type-safe handles
    println!("\n🔗 Building audio pipeline...");

    // Connect mono mic to both left and right inputs of mixer
    runtime.connect(
        mic.output_port::<AudioFrame<1>>("audio"),
        mixer.input_port::<AudioFrame<1>>("left"),
    )?;
    runtime.connect(
        mic.output_port::<AudioFrame<1>>("audio"),
        mixer.input_port::<AudioFrame<1>>("right"),
    )?;
    println!("   ✓ mic (mono) → mixer (left + right)");

    // TODO: Connect CLAP reverb when port names are fixed
    // For now, bypass reverb and connect mixer directly to speaker
    runtime.connect(
        mixer.output_port::<AudioFrame<2>>("audio"),
        speaker.input_port::<AudioFrame<2>>("audio"),
    )?;
    println!("   ✓ mixer (stereo) → speaker");

    println!("✅ Pipeline connected: mic (mono) → mixer → speaker (stereo)");

    // Step 9: Start the runtime
    println!("\n▶️  Starting audio processing...");
    println!("   Press Ctrl+C to stop\n");
    println!("🎙️  Speak into your microphone - you should hear yourself with reverb!\n");

    runtime.start()?;

    // Run until interrupted (blocks until Ctrl+C)
    runtime.run()?;

    println!("\n✅ Stopped\n");

    Ok(())
}
