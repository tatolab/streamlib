/// Event to wake up a processor in Push scheduling mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkWakeupEvent {
    /// New data is available on an input link
    DataAvailable,
    /// Processor should check for shutdown
    Shutdown,
}
