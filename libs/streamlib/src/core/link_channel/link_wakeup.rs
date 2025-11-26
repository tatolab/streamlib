//! Link wakeup events for push-based processor scheduling
//!
//! When a processor is in Push scheduling mode, it sleeps until woken by a LinkWakeupEvent.
//! These events signal that data is available on a link or the processor should check its state.

/// Event to wake up a processor in Push scheduling mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkWakeupEvent {
    /// New data is available on an input link
    DataAvailable,
    /// Processor should check for shutdown
    Shutdown,
}
