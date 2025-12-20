// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::error::Result;
use crate::core::graph::{LinkUniqueId, ProcessorUniqueId};
use crate::core::processors::ProcessorSpec;
use crate::core::{InputLinkPortRef, OutputLinkPortRef};

/// Unified interface for runtime graph operations.
///
/// Implemented by `StreamRuntime` (direct) and `RuntimeProxy` (channel-based).
/// Callers use this trait and don't need to know the underlying implementation.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to allow sharing across threads.
/// Graph operations should return quickly - compilation happens asynchronously.
pub trait RuntimeOperations: Send + Sync {
    /// Add a processor to the graph. Returns the processor ID.
    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId>;

    /// Remove a processor from the graph.
    fn remove_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()>;

    /// Connect two ports. Returns the link ID.
    fn connect(&self, from: OutputLinkPortRef, to: InputLinkPortRef) -> Result<LinkUniqueId>;

    /// Disconnect a link.
    fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()>;
}
