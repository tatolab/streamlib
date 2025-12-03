//! Link infrastructure for processor communication.
//!
//! This module provides the complete link infrastructure:
//!
//! - **graph/**: Blueprint (Link) and ECS components (LinkState)
//! - **runtime/**: Actual data flow (LinkInstance, handles, ports)
//! - **traits/**: Shared traits (LinkPortMessage, LinkPortType)
//!
//! The `LinkInstanceManager` orchestrates between graph and runtime.

pub mod graph;
pub mod link_instance_manager;
pub mod runtime;
pub mod traits;

// Re-export graph types
pub use graph::{
    Link, LinkDirection, LinkEndpoint, LinkId, LinkIdError, LinkState, LinkStateComponent,
};

// Re-export runtime types
pub use runtime::{
    AnyLinkInstance, BoxedLinkInstance, LinkInput, LinkInputDataReader, LinkInstance, LinkOutput,
    LinkOutputDataWriter, LinkOutputToProcessorMessage,
};

// Re-export traits
pub use traits::{sealed, LinkBufferReadMode, LinkPortAddress, LinkPortMessage, LinkPortType};

// Re-export manager
pub use link_instance_manager::LinkInstanceManager;

// Re-export capacity constant
pub use graph::DEFAULT_LINK_CAPACITY;
