//! Wakeup events for push-based processor scheduling
//!
//! When a processor is in Push scheduling mode, it sleeps until woken by a WakeupEvent.
//! These events signal that data is available or the processor should check its state.

/// Event to wake up a processor in Push scheduling mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeupEvent {
    /// New data is available on an input port
    DataAvailable,
    /// Processor should check for shutdown
    Shutdown,
}
