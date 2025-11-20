use super::{SchedulingMode, ThreadPriority};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulingConfig {
    pub mode: SchedulingMode,

    pub priority: ThreadPriority,
}

impl Default for SchedulingConfig {
    fn default() -> Self {
        Self {
            mode: SchedulingMode::Push,
            priority: ThreadPriority::Normal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SchedulingConfig::default();
        assert_eq!(config.mode, SchedulingMode::Push);
        assert_eq!(config.priority, ThreadPriority::Normal);
    }

    #[test]
    fn test_scheduling_config_serde() {
        let config = SchedulingConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: SchedulingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.mode, deserialized.mode);
        assert_eq!(config.priority, deserialized.priority);
    }
}
