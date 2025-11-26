//! Graph module - processor topology as a DAG
//!
//! The Graph is the "DOM" - a pure data representation of the desired processor topology.
//! It can be serialized, compared, cloned, and analyzed without any execution state.
//!
//! # Design
//!
//! The Graph follows a DOM/VDOM pattern:
//! - **Graph (DOM)**: Pure data structure describing topology
//! - **Executor**: Reads the graph and creates execution state
//!
//! The runtime modifies the Graph, and the executor reads it via shared `Arc<RwLock<Graph>>`.

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
