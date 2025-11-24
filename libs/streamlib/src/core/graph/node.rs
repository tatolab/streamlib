use crate::core::runtime::ProcessorId;

/// Node in the processor graph
#[derive(Debug, Clone)]
pub struct ProcessorNode {
    pub id: ProcessorId,
    pub processor_type: String,
    pub config_checksum: u64,
}
