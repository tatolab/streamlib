mod graph;
mod link;
mod link_port_markers;
mod link_port_ref;
mod node;
mod validation;

// Re-export all public types
pub use graph::{compute_config_checksum, Graph, GraphChecksum};
pub use link::{Link, LinkDirection, LinkEndpoint};
pub use link_port_markers::{input, output, InputPortMarker, OutputPortMarker, PortMarker};
pub use link_port_ref::{IntoLinkPortRef, LinkPortRef};
pub use node::{NodePorts, PortInfo, PortKind, ProcessorId, ProcessorNode};
