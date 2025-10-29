//! CLAP Parameter Discovery Example
//!
//! Demonstrates how to discover and set parameters for ANY CLAP plugin,
//! regardless of parameter type (dB, Hz, %, time, etc.)
//!
//! Usage:
//!   cargo run --example clap_parameter_discovery --features clap-plugins

use streamlib::core::AudioEffectProcessor;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🎛️  CLAP Parameter Discovery");
    println!("============================\n");

    let plugin_path = "/Users/fonta/Repositories/tatolab/clap-plugins/build/plugins/clap-plugins.clap/Contents/MacOS/clap-plugins";

    // Load and activate plugin
    #[cfg(feature = "clap-plugins")]
    let mut plugin = {
        use streamlib::core::processors::ClapEffectProcessor;
        let mut p = ClapEffectProcessor::load_by_name(plugin_path, "Gain")?;
        p.activate(48000, 2048)?;
        p
    };

    #[cfg(not(feature = "clap-plugins"))]
    {
        eprintln!("Error: This example requires the 'clap-plugins' feature");
        return Ok(());
    }

    // List all parameters (works for ANY plugin!)
    let params = plugin.list_parameters();

    println!("📋 Plugin has {} parameter(s):\n", params.len());

    for param in &params {
        println!("Parameter: {}", param.name);
        println!("  ID:          {}", param.id);
        println!("  Range:       {:.2} to {:.2}", param.min, param.max);
        println!("  Default:     {:.2}", param.default);
        println!("  Current:     {:.2} (display: \"{}\")", param.value, param.display);
        println!("  Automatable: {}", param.is_automatable);
        println!();

        // Determine parameter type from range and display string
        let param_type = if param.display.contains("dB") {
            "Decibel (dB)"
        } else if param.display.contains("Hz") {
            "Frequency (Hz)"
        } else if param.display.contains("%") {
            "Percentage (%)"
        } else if param.display.contains("ms") || param.display.contains("sec") {
            "Time"
        } else {
            "Unknown/Custom"
        };

        println!("  Detected type: {}", param_type);
        println!();
    }

    // Demonstrate setting parameters with ACTUAL values
    println!("🎯 Setting Parameter Examples:\n");

    if let Some(gain_param) = params.first() {
        // For a dB parameter (like our Gain plugin):
        println!("Example 1: Setting to unity gain (0 dB)");
        println!("  plugin.set_parameter({}, 0.0);", gain_param.id);
        plugin.set_parameter(gain_param.id, 0.0)?;
        println!("  ✅ Set to 0.0 dB (unity gain, 1.0x)");
        println!();

        println!("Example 2: Setting to +8 dB (2.5x gain)");
        println!("  plugin.set_parameter({}, 8.0);", gain_param.id);
        plugin.set_parameter(gain_param.id, 8.0)?;
        println!("  ✅ Set to 8.0 dB (~2.5x gain)");
        println!();

        println!("Example 3: Setting to minimum (-40 dB)");
        println!("  plugin.set_parameter({}, {:.1});", gain_param.id, gain_param.min);
        plugin.set_parameter(gain_param.id, gain_param.min)?;
        println!("  ✅ Set to {:.1} dB (minimum)", gain_param.min);
        println!();
    }

    println!("💡 Key Principles:\n");
    println!("1. ✅ Always use ACTUAL parameter values (dB, Hz, %, etc.)");
    println!("2. ✅ Use list_parameters() to discover min/max ranges");
    println!("3. ✅ Check the 'display' field to understand units");
    println!("4. ❌ NEVER use normalized 0.0-1.0 values!");
    println!();

    println!("📚 Parameter Type Examples:\n");
    println!("  Decibel (dB):      -40.0 to +40.0 dB");
    println!("  Frequency (Hz):    20.0 to 20000.0 Hz");
    println!("  Percentage (%):    0.0 to 100.0 %");
    println!("  Time (seconds):    0.001 to 10.0 sec");
    println!("  Enum (index):      0, 1, 2, 3, etc.");
    println!();

    println!("✨ This works for ANY CLAP plugin - the parameter system is universal!");

    Ok(())
}
