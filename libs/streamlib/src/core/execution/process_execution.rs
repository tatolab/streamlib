use serde::{Deserialize, Serialize};

/// Determines how and when the runtime invokes your `process()` function.
///
/// This is the most important decision when creating a processor - it controls
/// the entire execution model of your processor.
///
/// ## Quick Reference
///
/// | Mode | When `process()` is called | Use for |
/// |------|---------------------------|---------|
/// | [`Continuous`] | Repeatedly in a loop | Generators, sources, polling |
/// | [`Reactive`] | When input data arrives | Transforms, filters, effects |
/// | [`Manual`] | Once, then you control | Hardware callbacks, external schedulers |
///
/// [`Continuous`]: ProcessExecution::Continuous
/// [`Reactive`]: ProcessExecution::Reactive
/// [`Manual`]: ProcessExecution::Manual
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProcessExecution {
    /// **Runtime calls `process()` continuously in a loop.**
    ///
    /// The runtime manages a dedicated loop that repeatedly calls your `process()`
    /// function. You can optionally specify an interval between calls.
    ///
    /// ## When to use
    /// - Generating content (video frames, audio samples, test data)
    /// - Polling external sources (files, network endpoints)
    /// - Batch processing where you read from inputs on each iteration
    ///
    /// ## Behavior
    /// ```text
    /// setup() → process() → process() → process() → ... → teardown()
    ///              ↓           ↓           ↓
    ///          [interval]  [interval]  [interval]
    /// ```
    ///
    /// ## Configuration
    /// - `interval_ms: 0` - Run as fast as possible (yields to scheduler between calls)
    /// - `interval_ms: N` - Wait at least N milliseconds between `process()` calls
    ///
    /// ## Examples
    /// Camera capture, audio generator, file reader, test data source
    Continuous {
        /// Minimum interval between `process()` calls in milliseconds.
        ///
        /// - `0`: Run as fast as possible (default, yields between calls)
        /// - `N`: Wait at least N milliseconds between calls
        ///
        /// This is a *minimum* interval - actual timing depends on how long
        /// `process()` takes to execute.
        #[serde(default)]
        interval_ms: u32,
    },

    /// **Runtime calls `process()` when upstream writes to ANY input port.**
    ///
    /// Your processor sleeps until data arrives, then wakes to process it.
    /// This is event-driven and efficient - no polling, no wasted cycles.
    ///
    /// ## When to use
    /// - Transforming data (filters, effects, converters)
    /// - Reacting to input events
    /// - Any processor that processes input and produces output
    ///
    /// ## Behavior
    /// ```text
    /// setup() → [wait] → process() → [wait] → process() → ... → teardown()
    ///              ↑                     ↑
    ///         input data            input data
    /// ```
    ///
    /// ## Important
    /// `process()` is called once per wake event, not once per input message.
    /// If multiple inputs have data, batch-read them in a single `process()` call.
    ///
    /// ## Examples
    /// Video filter, audio mixer, format converter, encoder, decoder
    Reactive,

    /// **Runtime calls `process()` once, then YOU control all subsequent calls.**
    ///
    /// After `setup()`, the runtime calls `process()` exactly once to let you
    /// initialize. From then on, YOU are responsible for calling `process()`
    /// from your own callbacks, threads, or external systems.
    ///
    /// ## When to use
    /// - Hardware-driven timing (audio devices with callbacks, vsync)
    /// - External scheduler integration (game engines, UI frameworks)
    /// - Custom timing requirements that don't fit Continuous or Reactive
    ///
    /// ## Behavior
    /// ```text
    /// setup() → process() → [you control timing] → teardown()
    ///                              ↓
    ///                    your callbacks/threads
    ///                    call process() directly
    /// ```
    ///
    /// ## Warning
    /// This is advanced. You must handle your own timing and ensure `process()`
    /// is called appropriately. The runtime only manages setup/teardown lifecycle.
    ///
    /// ## Examples
    /// Audio output (hardware callback), display (vsync), game engine integration
    Manual,
}

impl ProcessExecution {
    /// Create a Continuous execution with default interval (as fast as possible).
    pub const fn continuous() -> Self {
        ProcessExecution::Continuous { interval_ms: 0 }
    }

    /// Create a Continuous execution with a specific interval.
    pub const fn continuous_with_interval(interval_ms: u32) -> Self {
        ProcessExecution::Continuous { interval_ms }
    }

    /// Create a Reactive execution (wake on input).
    pub const fn reactive() -> Self {
        ProcessExecution::Reactive
    }

    /// Create a Manual execution (you control timing).
    pub const fn manual() -> Self {
        ProcessExecution::Manual
    }

    /// Returns a human-readable description of this execution mode.
    ///
    /// Useful for logging and debugging.
    pub fn description(&self) -> String {
        match self {
            ProcessExecution::Continuous { interval_ms: 0 } => {
                "Continuous (runtime loops as fast as possible)".to_string()
            }
            ProcessExecution::Continuous { interval_ms } => {
                format!("Continuous (runtime loops every {}ms minimum)", interval_ms)
            }
            ProcessExecution::Reactive => {
                "Reactive (runtime calls process() when input data arrives)".to_string()
            }
            ProcessExecution::Manual => {
                "Manual (runtime calls process() once, then you control timing)".to_string()
            }
        }
    }

    /// Returns true if this is Continuous execution mode.
    pub fn is_continuous(&self) -> bool {
        matches!(self, ProcessExecution::Continuous { .. })
    }

    /// Returns true if this is Reactive execution mode.
    pub fn is_reactive(&self) -> bool {
        matches!(self, ProcessExecution::Reactive)
    }

    /// Returns true if this is Manual execution mode.
    pub fn is_manual(&self) -> bool {
        matches!(self, ProcessExecution::Manual)
    }

    /// Returns the interval in milliseconds for Continuous mode, or None for other modes.
    pub fn interval_ms(&self) -> Option<u32> {
        match self {
            ProcessExecution::Continuous { interval_ms } => Some(*interval_ms),
            _ => None,
        }
    }
}

impl Default for ProcessExecution {
    /// Default is Reactive - the most common case for processors that transform input to output.
    fn default() -> Self {
        ProcessExecution::Reactive
    }
}

impl std::fmt::Display for ProcessExecution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessExecution::Continuous { interval_ms: 0 } => write!(f, "Continuous"),
            ProcessExecution::Continuous { interval_ms } => {
                write!(f, "Continuous({}ms)", interval_ms)
            }
            ProcessExecution::Reactive => write!(f, "Reactive"),
            ProcessExecution::Manual => write!(f, "Manual"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_execution_equality() {
        assert_eq!(
            ProcessExecution::Continuous { interval_ms: 0 },
            ProcessExecution::Continuous { interval_ms: 0 }
        );
        assert_eq!(ProcessExecution::Reactive, ProcessExecution::Reactive);
        assert_eq!(ProcessExecution::Manual, ProcessExecution::Manual);
        assert_ne!(
            ProcessExecution::Continuous { interval_ms: 0 },
            ProcessExecution::Reactive
        );
        assert_ne!(ProcessExecution::Reactive, ProcessExecution::Manual);
        assert_ne!(
            ProcessExecution::Continuous { interval_ms: 10 },
            ProcessExecution::Continuous { interval_ms: 20 }
        );
    }

    #[test]
    fn test_process_execution_default() {
        assert_eq!(ProcessExecution::default(), ProcessExecution::Reactive);
    }

    #[test]
    fn test_process_execution_constructors() {
        assert_eq!(
            ProcessExecution::continuous(),
            ProcessExecution::Continuous { interval_ms: 0 }
        );
        assert_eq!(
            ProcessExecution::continuous_with_interval(100),
            ProcessExecution::Continuous { interval_ms: 100 }
        );
        assert_eq!(ProcessExecution::reactive(), ProcessExecution::Reactive);
        assert_eq!(ProcessExecution::manual(), ProcessExecution::Manual);
    }

    #[test]
    fn test_process_execution_is_methods() {
        let continuous = ProcessExecution::Continuous { interval_ms: 50 };
        let reactive = ProcessExecution::Reactive;
        let manual = ProcessExecution::Manual;

        assert!(continuous.is_continuous());
        assert!(!continuous.is_reactive());
        assert!(!continuous.is_manual());

        assert!(!reactive.is_continuous());
        assert!(reactive.is_reactive());
        assert!(!reactive.is_manual());

        assert!(!manual.is_continuous());
        assert!(!manual.is_reactive());
        assert!(manual.is_manual());
    }

    #[test]
    fn test_process_execution_interval_ms() {
        assert_eq!(
            ProcessExecution::Continuous { interval_ms: 50 }.interval_ms(),
            Some(50)
        );
        assert_eq!(
            ProcessExecution::Continuous { interval_ms: 0 }.interval_ms(),
            Some(0)
        );
        assert_eq!(ProcessExecution::Reactive.interval_ms(), None);
        assert_eq!(ProcessExecution::Manual.interval_ms(), None);
    }

    #[test]
    fn test_process_execution_display() {
        assert_eq!(
            ProcessExecution::Continuous { interval_ms: 0 }.to_string(),
            "Continuous"
        );
        assert_eq!(
            ProcessExecution::Continuous { interval_ms: 100 }.to_string(),
            "Continuous(100ms)"
        );
        assert_eq!(ProcessExecution::Reactive.to_string(), "Reactive");
        assert_eq!(ProcessExecution::Manual.to_string(), "Manual");
    }

    #[test]
    fn test_process_execution_description() {
        let desc = ProcessExecution::Continuous { interval_ms: 0 }.description();
        assert!(desc.contains("as fast as possible"));

        let desc = ProcessExecution::Continuous { interval_ms: 50 }.description();
        assert!(desc.contains("50ms"));

        let desc = ProcessExecution::Reactive.description();
        assert!(desc.contains("input data arrives"));

        let desc = ProcessExecution::Manual.description();
        assert!(desc.contains("you control"));
    }

    #[test]
    fn test_process_execution_serde() {
        // Test all variants
        let variants = [
            ProcessExecution::Continuous { interval_ms: 0 },
            ProcessExecution::Continuous { interval_ms: 100 },
            ProcessExecution::Reactive,
            ProcessExecution::Manual,
        ];

        for mode in variants {
            let json = serde_json::to_string(&mode).unwrap();
            let deserialized: ProcessExecution = serde_json::from_str(&json).unwrap();
            assert_eq!(mode, deserialized);
        }
    }

    #[test]
    fn test_process_execution_serde_format() {
        // Verify the JSON format is human-readable
        let continuous = ProcessExecution::Continuous { interval_ms: 50 };
        let json = serde_json::to_string(&continuous).unwrap();
        assert!(json.contains("Continuous"));
        assert!(json.contains("50"));

        let reactive = ProcessExecution::Reactive;
        let json = serde_json::to_string(&reactive).unwrap();
        assert!(json.contains("Reactive"));
    }
}
