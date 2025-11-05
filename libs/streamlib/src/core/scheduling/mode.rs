//! Scheduling modes for processors
//!
//! Defines WHEN and HOW processors are executed by the runtime.

use serde::{Deserialize, Serialize};

/// Scheduling mode for processors
///
/// Determines when the runtime executes a processor's logic.
/// This is orthogonal to thread priority (scheduling mode = WHEN, priority = HOW IMPORTANT).
///
/// ## Scheduling Modes
///
/// - **Loop**: Continuous execution at fixed rate (e.g., test tone generator)
/// - **Reactive**: Execute when input data arrives (e.g., video effects)
/// - **Callback**: Hardware-driven execution (e.g., camera, audio I/O)
/// - **Pull**: Hardware callback pulls from input ports (e.g., audio output)
/// - **Timer**: Periodic execution at specified intervals (e.g., metrics collector)
///
/// ## Examples
///
/// ```rust,ignore
/// // Test tone generator - runs in loop at 23.44 Hz
/// SchedulingMode::Loop
///
/// // Video effect - reactive to camera frames
/// SchedulingMode::Reactive
///
/// // Camera capture - driven by AVFoundation callbacks
/// SchedulingMode::Callback
///
/// // Metrics collector - runs every 1 second
/// SchedulingMode::Timer
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchedulingMode {
    /// Continuous loop at specified rate
    ///
    /// Runtime spawns thread calling the processor in a loop.
    /// Used by: TestToneGenerator, pattern generators, software sources
    ///
    /// **Rate**: Calculated by source from buffer characteristics
    ///
    /// **Thread**: Spawned by runtime with configured priority
    ///
    /// **Example**: Test tone generator running at 23.44 Hz (48kHz / 2048 samples)
    Loop,

    /// Reactive to data arrival
    ///
    /// Processor executes when input data is available.
    /// Used by: Video effects, transformers, most processors
    ///
    /// **Trigger**: Input port receives data
    ///
    /// **Thread**: Shared worker pool or dedicated thread based on priority
    ///
    /// **Example**: Color grading effect processes when camera sends frame
    Push,

    /// Hardware callback pulls from input ports
    ///
    /// Processor's process() is called directly from hardware callback thread.
    /// The processor pulls data from input ports at hardware rate.
    /// Runtime does NOT spawn a thread - processor manages its own callback.
    ///
    /// Used by: Audio output (CoreAudio callback), video display (vsync callback)
    ///
    /// **Trigger**: Hardware callback (e.g., CoreAudio render callback)
    ///
    /// **Thread**: Hardware-managed real-time thread
    ///
    /// **Example**: AudioOutput pulls from ring buffer in CoreAudio callback
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
