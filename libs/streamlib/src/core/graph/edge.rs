use crate::core::bus::{ConnectionId, PortType};

/// Edge in the processor graph
#[derive(Debug, Clone)]
pub struct ConnectionEdge {
    pub id: ConnectionId,
    pub from_port: String,
    pub to_port: String,
    pub port_type: PortType,
}
