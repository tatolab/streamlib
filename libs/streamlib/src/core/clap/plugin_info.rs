#[derive(Debug, Clone)]
pub struct ParameterInfo {
    pub id: u32,

    pub name: String,

    pub value: f64,

    pub min: f64,

    pub max: f64,

    pub default: f64,

    pub is_automatable: bool,

    pub is_stepped: bool,

    pub is_periodic: bool,

    pub is_hidden: bool,

    pub is_readonly: bool,

    pub is_bypass: bool,

    pub display: String,
}

#[derive(Debug, Clone)]
pub struct PluginInfo {
    pub name: String,

    pub vendor: String,

    pub version: String,

    pub format: String,

    pub id: String,

    pub num_inputs: u32,

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
