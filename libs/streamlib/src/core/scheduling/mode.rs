
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchedulingMode {
    Loop,

    Push,

    Pull,
}

impl Default for SchedulingMode {
    fn default() -> Self {
        SchedulingMode::Push
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scheduling_mode_equality() {
        assert_eq!(SchedulingMode::Loop, SchedulingMode::Loop);
        assert_ne!(SchedulingMode::Loop, SchedulingMode::Push);
        assert_ne!(SchedulingMode::Callback, SchedulingMode::Timer);
    }

    #[test]
    fn test_scheduling_mode_default() {
        assert_eq!(SchedulingMode::default(), SchedulingMode::Push);
    }

    #[test]
    fn test_scheduling_mode_debug() {
        let mode = SchedulingMode::Loop;
        let debug_str = format!("{:?}", mode);
        assert_eq!(debug_str, "Loop");
    }

    #[test]
    fn test_scheduling_mode_serde() {
        let mode = SchedulingMode::Callback;
        let json = serde_json::to_string(&mode).unwrap();
        let deserialized: SchedulingMode = serde_json::from_str(&json).unwrap();
        assert_eq!(mode, deserialized);
    }
}
