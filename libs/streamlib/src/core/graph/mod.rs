mod components;
#[allow(clippy::module_inception)]
mod graph;
mod link;
mod link_port_markers;
mod link_port_ref;
mod node;
mod property_graph;
mod validation;

// Re-export all public types
pub use components::{
    EcsComponentJson, LightweightMarker, LinkOutputToProcessorWriterAndReader, LinkStateComponent,
    MainThreadMarker, ProcessorInstance, ProcessorMetrics, ProcessorPauseGate, RayonPoolMarker,
    ShutdownChannel, StateComponent, ThreadHandle,
};
pub use graph::{compute_config_checksum, Graph, GraphChecksum};
pub use link::{Link, LinkDirection, LinkEndpoint, LinkState};
pub use link_port_markers::{input, output, InputPortMarker, OutputPortMarker, PortMarker};
pub use link_port_ref::{IntoLinkPortRef, LinkPortRef};
pub use node::{NodePorts, PortInfo, PortKind, ProcessorId, ProcessorNode};
pub use property_graph::{GraphState, PropertyGraph};
