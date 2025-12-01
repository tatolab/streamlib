//! Microphone → CLAP Reverb → Speaker Example
//!
//! Demonstrates streamlib's audio processing pipeline using CLAP as the "shader language for audio".
//! Just as video shaders transform pixels on GPU, CLAP plugins transform audio in real-time.

use streamlib::core::{
    AudioCaptureConfig, AudioChannelConverterConfig, AudioOutputConfig, AudioResamplerConfig,
    BufferRechunkerConfig, ChannelConversionMode, ClapEffectConfig, ResamplingQuality,
};
use streamlib::{
    input, output, request_audio_permission, AudioCaptureProcessor, AudioChannelConverterProcessor,
    AudioOutputProcessor, AudioResamplerProcessor, BufferRechunkerProcessor, ClapEffectProcessor,
    ClapScanner, Result, StreamRuntime,
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
    if !request_audio_permission()? {
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
    let effects_plugin = plugins.iter().find(|p| {
        let name_lower = p.name.to_lowercase();
        name_lower.contains("reverb")
            || name_lower.contains("verb")
            || name_lower.contains("effects")
            || name_lower.contains("fx")
            || p.features.iter().any(|f| {
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

    // Step 3: Add microphone input processor
    println!("\n🎤 Adding microphone input...");
    let mic = runtime.add_processor::<AudioCaptureProcessor::Processor>(AudioCaptureConfig {
        device_id: None,
    })?;
    println!("✅ Microphone processor added (mono output at 24kHz)");

    // Step 4: Add resampler (24kHz → 48kHz)
    println!("\n🔄 Adding resampler (24kHz → 48kHz)...");
    let resampler =
        runtime.add_processor::<AudioResamplerProcessor::Processor>(AudioResamplerConfig {
            source_sample_rate: 24000,
            target_sample_rate: 48000,
            quality: ResamplingQuality::High,
        })?;
    println!("✅ Resampler added (upsamples to 48kHz)");

    // Step 5: Add channel converter (mono → stereo)
    println!("\n🎛️  Adding channel converter (mono → stereo)...");
    let channel_converter = runtime.add_processor::<AudioChannelConverterProcessor::Processor>(
        AudioChannelConverterConfig {
            mode: ChannelConversionMode::Duplicate,
        },
    )?;
    println!("✅ Channel converter added (duplicates mono to L+R)");

    // Step 6: Add buffer rechunker (variable → fixed size)
    println!("\n🔧 Adding buffer rechunker (normalizes buffer sizes)...");
    let rechunker =
        runtime.add_processor::<BufferRechunkerProcessor::Processor>(BufferRechunkerConfig {
            target_buffer_size: 512, // Fixed buffer size for CLAP plugin
        })?;
    println!("✅ Buffer rechunker added (ensures fixed 512 sample chunks)");

    // Step 7: Add CLAP reverb plugin
    println!("\n🎛️  Adding CLAP plugin...");
    let reverb = runtime.add_processor::<ClapEffectProcessor::Processor>(ClapEffectConfig {
        plugin_path,
        plugin_name: None, // Use first plugin in bundle
        plugin_index: None,
        sample_rate: 48000, // Explicit sample rate for CLAP activation
        buffer_size: 512,   // Explicit buffer size for CLAP activation
    })?;
    println!("✅ CLAP effect processor added");
    println!("   Note: Plugin activated with explicit 48kHz/512 samples config");
    println!("   Note: Use parameter automation API for runtime parameter changes");

    // Step 8: Add speaker output processor
    println!("\n🔊 Adding speaker output...");
    let speaker = runtime.add_processor::<AudioOutputProcessor::Processor>(AudioOutputConfig {
        device_id: None, // Use default speaker
    })?;
    println!("✅ Speaker processor added (will query hardware for native config)");

    // Step 9: Connect the pipeline using type-safe port markers
    println!("\n🔗 Building audio pipeline...");

    // Pipeline: mic → resampler → channel_converter → rechunker → reverb → speaker
    runtime.connect(
        output::<AudioCaptureProcessor::OutputLink::audio>(&mic),
        input::<AudioResamplerProcessor::InputLink::audio_in>(&resampler),
    )?;
    println!("   ✓ mic (mono 24kHz) → resampler");

    runtime.connect(
        output::<AudioResamplerProcessor::OutputLink::audio_out>(&resampler),
        input::<AudioChannelConverterProcessor::InputLink::audio_in>(&channel_converter),
    )?;
    println!("   ✓ resampler (mono 48kHz) → channel_converter");

    runtime.connect(
        output::<AudioChannelConverterProcessor::OutputLink::audio_out>(&channel_converter),
        input::<BufferRechunkerProcessor::InputLink::audio_in>(&rechunker),
    )?;
    println!("   ✓ channel_converter (stereo) → rechunker");

    runtime.connect(
        output::<BufferRechunkerProcessor::OutputLink::audio_out>(&rechunker),
        input::<ClapEffectProcessor::InputLink::audio_in>(&reverb),
    )?;
    println!("   ✓ rechunker (fixed-size stereo) → reverb");

    runtime.connect(
        output::<ClapEffectProcessor::OutputLink::audio_out>(&reverb),
        input::<AudioOutputProcessor::InputLink::audio>(&speaker),
    )?;
    println!("   ✓ reverb (stereo) → speaker");

    println!(
        "✅ Pipeline connected: mic → resampler → channel_converter → rechunker → reverb → speaker"
    );

    // Step 10: Start the runtime
    println!("\n▶️  Starting audio processing...");
    println!("   Press Ctrl+C to stop\n");
    println!("🎙️  Speak into your microphone - you should hear yourself with reverb!\n");

    // start() blocks on macOS standalone (runs NSApplication event loop)
    runtime.start()?;

    println!("\n✅ Stopped\n");

    Ok(())
}
