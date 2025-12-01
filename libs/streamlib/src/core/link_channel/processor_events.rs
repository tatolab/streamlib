/// Events sent to control when a processor's process() function is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessFunctionEvent {
    /// Invoke the processor's process() function (e.g., new data available)
    InvokeFunction,
    /// Stop calling the process() function and shut down
    StopProcessing,
}
