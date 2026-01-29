// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Microphone → CLAP Reverb → Speaker Example
//!
//! Demonstrates streamlib's audio processing pipeline using CLAP as the "shader language for audio".
//! Just as video shaders transform pixels on GPU, CLAP plugins transform audio in real-time.

use streamlib::{
    input, output, request_audio_permission, AudioCaptureProcessor, AudioOutputProcessor,
    ClapEffectProcessor, ClapScanner, Result, StreamRuntime,
};

fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("\n🎙️  Microphone → CLAP Reverb → Speaker Example\n");

    // Create runtime first
    let runtime = StreamRuntime::new()?;

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
    let mic =
        runtime.add_processor(AudioCaptureProcessor::node(AudioCaptureProcessor::Config {
            device_id: None,
        }))?;
    println!("✅ Microphone processor added");

    // Step 4: Add CLAP reverb plugin (activates at source sample rate on first frame)
    println!("\n🎛️  Adding CLAP plugin...");
    let reverb = runtime.add_processor(ClapEffectProcessor::node(ClapEffectProcessor::Config {
        plugin_path: plugin_path.to_string_lossy().to_string(),
        plugin_name: None, // Use first plugin in bundle
        plugin_index: None,
        buffer_size: 512,
    }))?;
    println!("✅ CLAP effect processor added (activates at source sample rate)");

    // Step 5: Add speaker output processor
    println!("\n🔊 Adding speaker output...");
    let speaker =
        runtime.add_processor(AudioOutputProcessor::node(AudioOutputProcessor::Config {
            device_id: None, // Use default speaker
        }))?;
    println!("✅ Speaker processor added");

    // Step 6: Connect the pipeline using type-safe port markers
    println!("\n🔗 Building audio pipeline...");

    // Pipeline: mic → reverb → speaker
    runtime.connect(
        output::<AudioCaptureProcessor::OutputLink::audio>(&mic),
        input::<ClapEffectProcessor::InputLink::audio_in>(&reverb),
    )?;
    println!("   ✓ mic → reverb");

    runtime.connect(
        output::<ClapEffectProcessor::OutputLink::audio_out>(&reverb),
        input::<AudioOutputProcessor::InputLink::audio>(&speaker),
    )?;
    println!("   ✓ reverb → speaker");

    println!("✅ Pipeline connected: mic → reverb → speaker");

    // Step 7: Start the runtime
    println!("\n▶️  Starting audio processing...");
    println!("   Press Ctrl+C to stop\n");
    println!("🎙️  Speak into your microphone - you should hear yourself with reverb!\n");

    // start() blocks on macOS standalone (runs NSApplication event loop)
    runtime.start()?;

    runtime.wait_for_signal()?;

    println!("\n✅ Stopped\n");

    Ok(())
}
