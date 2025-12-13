use crate::core::graph::ProcessorUniqueId;

use crate::core::ProcessorState;

/// Runtime status information.
#[derive(Debug, Clone, Default)]
pub struct RuntimeStatus {
    pub running: bool,
    pub processor_count: usize,
    pub link_count: usize,
    pub processor_states: Vec<(ProcessorUniqueId, ProcessorState)>,
}
