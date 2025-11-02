//! CLAP Plugin Information Types
//!
//! Data structures for CLAP plugin metadata and parameter information.
//! These are generic across CLAP plugins and used by ClapEffectProcessor.

/// Information about a plugin parameter
///
/// Represents a single parameter exposed by a CLAP plugin, including
/// its current value, range, and metadata.
#[derive(Debug, Clone)]
pub struct ParameterInfo {
    /// Parameter ID (stable across sessions)
    pub id: u32,

    /// Human-readable name
    pub name: String,

    /// Current value in parameter's native units (e.g., dB, Hz, %, etc.)
    /// NOT normalized! Use min/max to understand the range.
    pub value: f64,

    /// Minimum value in parameter's native units
    pub min: f64,

    /// Maximum value in parameter's native units
    pub max: f64,

    /// Default value in parameter's native units
    pub default: f64,

    /// Is this parameter automatable?
    pub is_automatable: bool,

    /// Is this parameter stepped (discrete values like enums)?
    pub is_stepped: bool,

    /// Is this parameter periodic (wraps around, like phase)?
    pub is_periodic: bool,

    /// Is this parameter hidden from UI?
    pub is_hidden: bool,

    /// Is this parameter read-only?
    pub is_readonly: bool,

    /// Is this parameter a bypass parameter?
    pub is_bypass: bool,

    /// Display string for current value (e.g., "12.5 dB", "440 Hz")
    pub display: String,
}

/// Information about a CLAP audio plugin
///
/// Provides metadata about a loaded CLAP plugin instance.
#[derive(Debug, Clone)]
pub struct PluginInfo {
    /// Plugin name
    pub name: String,

    /// Vendor/manufacturer
    pub vendor: String,

    /// Version string
    pub version: String,

    /// Plugin format (always "CLAP" for this module)
    pub format: String,

    /// Unique identifier
    pub id: String,

    /// Number of audio inputs
    pub num_inputs: u32,

    /// Number of audio outputs
    pub num_outputs: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parameter_info() {
        let param = ParameterInfo {
            id: 0,
            name: "Volume".to_string(),
            value: 0.5,
            min: 0.0,
            max: 1.0,
            default: 0.5,
            is_automatable: true,
            is_stepped: false,
            is_periodic: false,
            is_hidden: false,
            is_readonly: false,
            is_bypass: false,
            display: "-6.0 dB".to_string(),
        };

        assert_eq!(param.name, "Volume");
        assert_eq!(param.value, 0.5);
        assert!(param.is_automatable);
        assert!(!param.is_stepped);
        assert!(!param.is_readonly);
    }

    #[test]
    fn test_plugin_info() {
        let info = PluginInfo {
            name: "Test Reverb".to_string(),
            vendor: "Test Vendor".to_string(),
            version: "1.0.0".to_string(),
            format: "CLAP".to_string(),
            id: "com.example.reverb".to_string(),
            num_inputs: 2,
            num_outputs: 2,
        };

        assert_eq!(info.name, "Test Reverb");
        assert_eq!(info.format, "CLAP");
        assert_eq!(info.num_inputs, 2);
    }
}
