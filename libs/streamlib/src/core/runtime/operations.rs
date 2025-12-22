// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::error::Result;
use crate::core::graph::{LinkUniqueId, ProcessorUniqueId};
use crate::core::processors::ProcessorSpec;
use crate::core::{InputLinkPortRef, OutputLinkPortRef};
use std::future::Future;
use std::pin::Pin;

/// Boxed future type for async trait methods (required for dyn compatibility).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Unified interface for runtime graph operations.
///
/// Implemented by `StreamRuntime` (direct) and `RuntimeProxy` (channel-based).
/// Callers use this trait and don't need to know the underlying implementation.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to allow sharing across threads.
/// Graph operations should return quickly - compilation happens asynchronously.
///
/// # Sync vs Async Methods
///
/// Both sync and async variants are provided:
/// - **Async methods** (`*_async`): Safe to call from any context including tokio tasks.
///   Use these from async code: `ctx.runtime().add_processor_async(spec).await`
/// - **Sync methods**: Convenience wrappers that block on the async variants.
///   Use these from sync code: `runtime.add_processor(spec)`
///
/// The sync methods internally use `block_on`, so they must NOT be called from
/// within a tokio task (will panic). Use the async variants in async contexts.
pub trait RuntimeOperations: Send + Sync {
    // =========================================================================
    // Async Methods (primary implementation - safe from any context)
    // =========================================================================

    /// Add a processor to the graph asynchronously. Returns the processor ID.
    #[must_use]
    fn add_processor_async(&self, spec: ProcessorSpec) -> BoxFuture<'_, Result<ProcessorUniqueId>>;

    /// Remove a processor from the graph asynchronously.
    #[must_use]
    fn remove_processor_async(&self, processor_id: ProcessorUniqueId) -> BoxFuture<'_, Result<()>>;

    /// Connect two ports asynchronously. Returns the link ID.
    #[must_use]
    fn connect_async(
        &self,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
    ) -> BoxFuture<'_, Result<LinkUniqueId>>;

    /// Disconnect a link asynchronously.
    #[must_use]
    fn disconnect_async(&self, link_id: LinkUniqueId) -> BoxFuture<'_, Result<()>>;

    // =========================================================================
    // Sync Methods (convenience wrappers - NOT safe from tokio tasks)
    // =========================================================================

    /// Add a processor to the graph. Returns the processor ID.
    ///
    /// This is a blocking wrapper around [`add_processor_async`]. Do not call
    /// from within a tokio task - use the async variant instead.
    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId>;

    /// Remove a processor from the graph.
    ///
    /// This is a blocking wrapper around [`remove_processor_async`]. Do not call
    /// from within a tokio task - use the async variant instead.
    fn remove_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()>;

    /// Connect two ports. Returns the link ID.
    ///
    /// This is a blocking wrapper around [`connect_async`]. Do not call
    /// from within a tokio task - use the async variant instead.
    fn connect(&self, from: OutputLinkPortRef, to: InputLinkPortRef) -> Result<LinkUniqueId>;

    /// Disconnect a link.
    ///
    /// This is a blocking wrapper around [`disconnect_async`]. Do not call
    /// from within a tokio task - use the async variant instead.
    fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()>;
}
