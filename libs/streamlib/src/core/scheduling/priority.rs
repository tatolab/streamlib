
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreadPriority {
    RealTime,

    High,

    Normal,
}

impl Default for ThreadPriority {
    fn default() -> Self {
        ThreadPriority::Normal
    }
}

impl ThreadPriority {
    pub fn description(&self) -> &'static str {
        match self {
            ThreadPriority::RealTime => "Real-time (< 10ms latency, time-constrained)",
            ThreadPriority::High => "High priority (< 33ms latency, elevated)",
            ThreadPriority::Normal => "Normal priority (no strict latency)",
        }
    }

    pub fn latency_budget_ms(&self) -> Option<f64> {
        match self {
            ThreadPriority::RealTime => Some(10.0),
            ThreadPriority::High => Some(33.0),
            ThreadPriority::Normal => None,  // No strict budget
        }
    }

    pub fn requires_realtime_safety(&self) -> bool {
        matches!(self, ThreadPriority::RealTime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thread_priority_equality() {
        assert_eq!(ThreadPriority::RealTime, ThreadPriority::RealTime);
        assert_ne!(ThreadPriority::RealTime, ThreadPriority::High);
        assert_ne!(ThreadPriority::High, ThreadPriority::Normal);
    }

    #[test]
    fn test_thread_priority_default() {
        assert_eq!(ThreadPriority::default(), ThreadPriority::Normal);
    }

    #[test]
    fn test_thread_priority_description() {
        assert_eq!(
            ThreadPriority::RealTime.description(),
            "Real-time (< 10ms latency, time-constrained)"
        );
        assert_eq!(
            ThreadPriority::High.description(),
            "High priority (< 33ms latency, elevated)"
        );
        assert_eq!(
            ThreadPriority::Normal.description(),
            "Normal priority (no strict latency)"
        );
    }

    #[test]
    fn test_latency_budget() {
        assert_eq!(ThreadPriority::RealTime.latency_budget_ms(), Some(10.0));
        assert_eq!(ThreadPriority::High.latency_budget_ms(), Some(33.0));
        assert_eq!(ThreadPriority::Normal.latency_budget_ms(), None);
    }

    #[test]
    fn test_requires_realtime_safety() {
        assert!(ThreadPriority::RealTime.requires_realtime_safety());
        assert!(!ThreadPriority::High.requires_realtime_safety());
        assert!(!ThreadPriority::Normal.requires_realtime_safety());
    }

    #[test]
    fn test_thread_priority_serde() {
        let priority = ThreadPriority::High;
        let json = serde_json::to_string(&priority).unwrap();
        let deserialized: ThreadPriority = serde_json::from_str(&json).unwrap();
        assert_eq!(priority, deserialized);
    }

    #[test]
    fn test_thread_priority_debug() {
        let priority = ThreadPriority::RealTime;
        let debug_str = format!("{:?}", priority);
        assert_eq!(debug_str, "RealTime");
    }
}
