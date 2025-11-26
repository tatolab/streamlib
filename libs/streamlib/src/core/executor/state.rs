#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorState {
    /// Not yet compiled or stopped
    Idle,
    /// Graph compiled, ready to start
    Compiled,
    /// Actively executing processors
    Running,
    /// Execution paused, can resume
    Paused,
}
