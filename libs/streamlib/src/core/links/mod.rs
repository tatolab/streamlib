// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Link infrastructure for processor communication.
//!
//! This module provides the complete link infrastructure:
//!
//! - **graph/**: Blueprint (Link) and ECS components (LinkState, LinkInstanceComponent)
//! - **runtime/**: Actual data flow (LinkInstance, handles, ports)
//! - **traits/**: Shared traits (LinkPortMessage, LinkPortType)
//! - **link_factory**: Factory for creating link instances

pub mod graph;
pub mod link_factory;
pub mod runtime;
pub mod traits;

// Re-export graph types
pub use graph::{
    Link, LinkDirection, LinkEndpoint, LinkId, LinkIdError, LinkInstanceComponent, LinkState,
    LinkStateComponent, LinkTypeInfoComponent,
};

// Re-export runtime types
pub use runtime::{
    AnyLinkInstance, BoxedLinkInstance, LinkInput, LinkInputDataReader, LinkInstance, LinkOutput,
    LinkOutputDataWriter, LinkOutputToProcessorMessage,
};

// Re-export traits
pub use traits::{sealed, LinkBufferReadMode, LinkPortAddress, LinkPortMessage, LinkPortType};

// Re-export factory
pub use link_factory::{DefaultLinkFactory, LinkFactoryDelegate, LinkInstanceCreationResult};

// Re-export capacity constant
pub use graph::DEFAULT_LINK_CAPACITY;
