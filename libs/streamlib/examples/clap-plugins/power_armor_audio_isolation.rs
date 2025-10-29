//! Power Armor Audio Isolation Example
//!
//! Demonstrates advanced CLAP parameter features for agent-driven audio processing.
//!
//! # Scenario
//!
//! A power armor helmet has microphones that detect an unknown sound. An AI agent
//! dynamically adds an EQ/filter plugin to isolate the frequency band, then forwards
//! the clean audio to an ML node for classification.
//!
//! # Features Demonstrated
//!
//! 1. **Parameter Discovery** - Examining parameter flags (is_stepped, is_automatable)
//! 2. **Parameter Transactions** - Using begin_edit/end_edit for batched updates
//! 3. **Parameter Modulation** - LFO sweep to scan for the target frequency
//! 4. **Parameter Automation** - Orchestrating the entire isolation sequence
//!
//! # Prerequisites
//!
//! This example requires:
//! - A CLAP plugin with EQ/filter (e.g., Surge XT FX)
//! - The plugin should have parameters for: filter type, cutoff, Q, gain
//!
//! # Usage
//!
//! ```bash
//! cargo run --example power_armor_audio_isolation --features clap-plugins
//! ```

use streamlib::{
    ClapEffectProcessor, AudioEffectProcessor, ParameterInfo,
    ParameterModulator, ParameterAutomation, LfoWaveform, Result,
};

fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("\n🎯 Power Armor Audio Isolation System");
    println!("=====================================\n");

    // Step 1: Detect available CLAP plugins
    println!("📦 Step 1: Plugin Discovery");
    println!("Looking for audio filter plugins...\n");

    // For this example, you'll need to provide a path to a CLAP plugin
    // Common locations:
    // - macOS: /Library/Audio/Plug-Ins/CLAP/
    // - Linux: /usr/lib/clap/
    // - Windows: C:\\Program Files\\Common Files\\CLAP\\

    // Using Surge XT Effects (audio processor), not Surge XT (synthesizer)
    let plugin_path = "/Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap/Contents/MacOS/Surge XT Effects";

    println!("🔌 Loading plugin from: {}", plugin_path);

    // Load plugin (this example assumes you have Surge XT Effects installed)
    // In a real scenario, the agent would search for appropriate plugins
    let mut filter = match ClapEffectProcessor::load(plugin_path) {
        Ok(p) => {
            println!("✅ Plugin loaded successfully\n");
            p
        },
        Err(e) => {
            println!("❌ Failed to load plugin: {}", e);
            println!("\n💡 Hint: Install Surge XT from https://surge-synthesizer.github.io/");
            println!("   The installer includes both Surge XT (synth) and Surge XT Effects (audio processor)");
            println!("   Or modify plugin_path to point to your CLAP effect plugin\n");
            return Err(e);
        }
    };

    // Activate plugin for audio processing
    let sample_rate = 48000;
    let buffer_size = 2048;
    filter.activate(sample_rate, buffer_size)?;

    println!("🎛️  Plugin activated ({}Hz, {} samples buffer)\n", sample_rate, buffer_size);

    // Step 2: Discover and examine parameters
    println!("📊 Step 2: Parameter Discovery");
    println!("Examining plugin parameters...\n");

    let parameters = filter.list_parameters();
    println!("Found {} parameters:\n", parameters.len());

    // Display parameter capabilities
    let mut automatable_params = Vec::new();
    let mut stepped_params = Vec::new();

    for (i, param) in parameters.iter().take(10).enumerate() {
        println!("  [{}] {}", i, param.name);
        println!("      ID: {}, Value: {:.2} (range: {:.2} - {:.2})",
            param.id, param.value, param.min, param.max);

        // Show parameter flags
        let mut flags = Vec::new();
        if param.is_automatable { flags.push("automatable"); automatable_params.push(param.clone()); }
        if param.is_stepped { flags.push("stepped"); stepped_params.push(param.clone()); }
        if param.is_periodic { flags.push("periodic"); }
        if param.is_readonly { flags.push("readonly"); }
        if param.is_bypass { flags.push("bypass"); }

        if !flags.is_empty() {
            println!("      Flags: {}", flags.join(", "));
        }
        println!();
    }

    if parameters.len() > 10 {
        println!("  ... and {} more parameters\n", parameters.len() - 10);
    }

    println!("📈 Analysis:");
    println!("  - {} automatable parameters (suitable for modulation)", automatable_params.len());
    println!("  - {} stepped parameters (discrete values)\n", stepped_params.len());

    // Step 3: Parameter Transactions
    println!("🔄 Step 3: Parameter Transactions");
    println!("Configuring filter for frequency isolation...\n");

    // Find key parameters (this is plugin-specific)
    // In real code, the agent would intelligently search for these
    let filter_params = find_filter_parameters(&parameters);

    if let Some(params) = filter_params {
        println!("✅ Found filter parameters:");
        println!("   - Cutoff: ID {}", params.cutoff_id);
        println!("   - Q Factor: ID {}", params.q_id);
        if let Some(gain_id) = params.gain_id {
            println!("   - Gain: ID {}", gain_id);
        }
        println!();

        // Use parameter transactions to batch all changes
        println!("🎛️  Applying parameter transaction...");
        println!("   (Batching multiple parameter changes for glitch-free updates)\n");

        // Begin transaction for all parameters
        filter.begin_edit(params.cutoff_id)?;
        filter.begin_edit(params.q_id)?;
        if let Some(gain_id) = params.gain_id {
            filter.begin_edit(gain_id)?;
        }

        // Configure band-pass filter centered at 3kHz
        println!("   Setting up band-pass filter:");
        println!("   - Center frequency: 3000 Hz");
        filter.set_parameter(params.cutoff_id, 3000.0)?;

        println!("   - Q factor: 2.0 (narrow band)");
        filter.set_parameter(params.q_id, 2.0)?;

        if let Some(gain_id) = params.gain_id {
            println!("   - Gain: +12 dB (boost target frequency)");
            filter.set_parameter(gain_id, 12.0)?;
        }

        // Commit transaction
        filter.end_edit(params.cutoff_id)?;
        filter.end_edit(params.q_id)?;
        if let Some(gain_id) = params.gain_id {
            filter.end_edit(gain_id)?;
        }

        println!("\n   ✅ Transaction complete - all parameters updated atomically\n");

    } else {
        println!("⚠️  Could not find filter parameters in this plugin");
        println!("   (This example is designed for filter/EQ plugins)\n");
    }

    // Step 4: Parameter Modulation
    println!("🌊 Step 4: Parameter Modulation");
    println!("Demonstrating LFO frequency sweep...\n");

    // Create LFO for frequency scanning (0.5 Hz sine wave)
    let mut frequency_lfo = ParameterModulator::lfo(0.5, LfoWaveform::Sine);

    println!("Created LFO modulator:");
    println!("  - Waveform: Sine");
    println!("  - Frequency: 0.5 Hz (2 second cycle)");
    println!("  - Range: 200 Hz - 4000 Hz\n");

    // Sample the LFO at different times
    println!("LFO samples over time:");
    for i in 0..5 {
        let time = i as f64 * 0.5;
        let lfo_value = frequency_lfo.sample(time);

        // Map to frequency range
        let freq = 200.0 + (lfo_value * 3800.0);

        println!("  t={:.1}s: LFO={:.3} → Cutoff={:.0} Hz", time, lfo_value, freq);
    }
    println!();

    // Step 5: Parameter Automation
    println!("⏰ Step 5: Parameter Automation");
    println!("Orchestrating the complete isolation sequence...\n");

    let mut automation = ParameterAutomation::new();

    if let Some(params) = find_filter_parameters(&parameters) {
        // Schedule the isolation sequence:

        // 1. At t=0.0, bypass off (enable filter)
        if let Some(bypass_param) = parameters.iter().find(|p| p.is_bypass) {
            automation.schedule(0.0, bypass_param.id, 0.0);
            println!("   [0.0s] Enable filter (bypass off)");
        }

        // 2. At t=0.5, start frequency sweep
        let sweep_lfo = ParameterModulator::lfo(0.25, LfoWaveform::Sine);
        automation.add_modulator(
            params.cutoff_id,
            sweep_lfo,
            0.5,           // start time
            Some(5.5),     // end time (5 second sweep)
            200.0,         // min frequency
            4000.0,        // max frequency
        );
        println!("   [0.5s] Begin frequency sweep (200 Hz - 4 kHz over 5 seconds)");

        // 3. At t=6.0, lock onto detected frequency
        automation.schedule(6.0, params.cutoff_id, 2850.0);
        println!("   [6.0s] Lock onto detected frequency (2850 Hz)");

        // 4. At t=6.5, increase Q for tight isolation
        automation.schedule(6.5, params.q_id, 5.0);
        println!("   [6.5s] Narrow band-pass (Q=5.0) for precise isolation");

        println!("\n   Automation ready: {} scheduled events, {} active modulators\n",
            automation.pending_changes(),
            automation.active_modulators()
        );

        // Simulate running the automation over time
        println!("🎬 Simulating automation execution:\n");

        let time_steps = vec![0.0, 0.5, 1.0, 2.0, 4.0, 6.0, 6.5, 7.0];

        for time in time_steps {
            let updates = automation.update(time, &mut filter)?;
            if updates > 0 {
                println!("   [t={:.1}s] Applied {} parameter updates", time, updates);
            }
        }

        println!();
    }

    // Cleanup
    filter.deactivate()?;

    println!("✅ Demonstration Complete!");
    println!("\n📋 Summary:");
    println!("   - Discovered plugin parameters and examined their capabilities");
    println!("   - Used parameter transactions for glitch-free multi-parameter updates");
    println!("   - Created LFO modulators for frequency sweeps");
    println!("   - Orchestrated complex automation sequences\n");

    println!("🚀 This demonstrates the infrastructure for:");
    println!("   - Agent-driven audio processing");
    println!("   - Real-time frequency isolation");
    println!("   - Automated parameter control");
    println!("   - Embedded system optimization (transaction batching)\n");

    Ok(())
}

/// Helper struct to hold filter parameter IDs
struct FilterParameters {
    cutoff_id: u32,
    q_id: u32,
    gain_id: Option<u32>,
}

/// Find filter-related parameters in the plugin
///
/// This is a heuristic search - real agents would use more sophisticated methods
fn find_filter_parameters(parameters: &[ParameterInfo]) -> Option<FilterParameters> {
    // Look for common filter parameter names
    let cutoff = parameters.iter().find(|p| {
        let name_lower = p.name.to_lowercase();
        name_lower.contains("cutoff") || name_lower.contains("frequency") || name_lower.contains("freq")
    })?;

    let q_factor = parameters.iter().find(|p| {
        let name_lower = p.name.to_lowercase();
        name_lower.contains("q") || name_lower.contains("resonance") || name_lower.contains("bandwidth")
    })?;

    let gain = parameters.iter().find(|p| {
        let name_lower = p.name.to_lowercase();
        name_lower.contains("gain") || name_lower.contains("level")
    });

    Some(FilterParameters {
        cutoff_id: cutoff.id,
        q_id: q_factor.id,
        gain_id: gain.map(|p| p.id),
    })
}
