// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::error::Result;
use crate::core::graph::{LinkUniqueId, ProcessorUniqueId};
use crate::core::processors::ProcessorSpec;
use crate::core::runtime::TapSubscription;
use crate::core::{InputLinkPortRef, OutputLinkPortRef};
use std::future::Future;
use std::pin::Pin;

/// Boxed future type for async trait methods (required for dyn compatibility).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Unified interface for runtime graph operations.
///
/// Implemented by `Runner` (direct) and `RuntimeProxy` (channel-based).
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
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the ID in fire-and-forget scenarios.
    fn add_processor_async(&self, spec: ProcessorSpec) -> BoxFuture<'_, Result<ProcessorUniqueId>>;

    /// Remove a processor from the graph asynchronously.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the result in fire-and-forget scenarios.
    fn remove_processor_async(&self, processor_id: ProcessorUniqueId) -> BoxFuture<'_, Result<()>>;

    /// Connect two ports asynchronously. Returns the link ID.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the ID in fire-and-forget scenarios.
    fn connect_async(
        &self,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
    ) -> BoxFuture<'_, Result<LinkUniqueId>>;

    /// Disconnect a link asynchronously.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the result in fire-and-forget scenarios.
    fn disconnect_async(&self, link_id: LinkUniqueId) -> BoxFuture<'_, Result<()>>;

    /// Export graph state as JSON asynchronously.
    fn to_json_async(&self) -> BoxFuture<'_, Result<serde_json::Value>>;

    /// Attach a read-only tap to a named channel, streaming its raw bags.
    ///
    /// `channel` is a channel data-service name
    /// (`{source_processor}/{source_output_port}`,
    /// [`crate::iceoryx2::source_channel_name`]); `count` bounds the tap to that
    /// many bags then ends, `None` streams live until the returned
    /// [`TapSubscription`] is dropped. The tap consumes the channel's single
    /// reserved subscriber slot with no publisher re-open, so exactly one
    /// concurrent tap per channel is allowed — a second attach fails with
    /// [`Error::TapSlotOccupied`] until the first detaches (drops). An unwired /
    /// unknown channel fails with [`Error::TapChannelNotFound`].
    ///
    /// There is no sync variant: a tap yields a live streaming handle, not a
    /// one-shot result, so blocking on it is never the intent. Host-side only —
    /// a plugin cdylib cannot own the host's `!Send` subscriber, so
    /// implementations reachable only across the plugin ABI reject this with
    /// [`Error::NotSupported`].
    ///
    /// [`Error::TapSlotOccupied`]: crate::core::error::Error::TapSlotOccupied
    /// [`Error::TapChannelNotFound`]: crate::core::error::Error::TapChannelNotFound
    /// [`Error::NotSupported`]: crate::core::error::Error::NotSupported
    fn tap_async(
        &self,
        channel: String,
        count: Option<usize>,
    ) -> BoxFuture<'_, Result<TapSubscription>>;

    // =========================================================================
    // Sync Methods (convenience wrappers - NOT safe from tokio tasks)
    // =========================================================================

    /// Add a processor to the graph. Returns the processor ID.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the ID in fire-and-forget scenarios.
    ///
    /// This is a blocking wrapper around [`add_processor_async`]. Do not call
    /// from within a tokio task - use the async variant instead.
    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId>;

    /// Remove a processor from the graph.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the result in fire-and-forget scenarios.
    ///
    /// This is a blocking wrapper around [`remove_processor_async`]. Do not call
    /// from within a tokio task - use the async variant instead.
    fn remove_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()>;

    /// Connect two ports. Returns the link ID.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the ID in fire-and-forget scenarios.
    ///
    /// This is a blocking wrapper around [`connect_async`]. Do not call
    /// from within a tokio task - use the async variant instead.
    fn connect(&self, from: OutputLinkPortRef, to: InputLinkPortRef) -> Result<LinkUniqueId>;

    /// Disconnect a link.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the result in fire-and-forget scenarios.
    ///
    /// This is a blocking wrapper around [`disconnect_async`]. Do not call
    /// from within a tokio task - use the async variant instead.
    fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()>;

    // =========================================================================
    // Lifecycle
    // =========================================================================

    /// Ask whoever owns the run loop to shut the runtime down, with a
    /// human-readable `reason` logged for attribution.
    ///
    /// A *request*, not a teardown: the loop owner
    /// ([`Runner::wait_for_signal_with`](crate::core::runtime::Runner::wait_for_signal_with))
    /// observes it and runs the normal stop sequence. Idempotent — requesting
    /// twice is not an error.
    ///
    /// The effect is process-global (matching the `RuntimeShutdown` event on
    /// `topics::RUNTIME_GLOBAL`): the receiver is not a scoping parameter, and
    /// the latch is first-observer-wins — the loop owner that observes the
    /// request takes it. A request issued while no run loop is running is
    /// observed by the next one to start, so a start-script that aborts from
    /// `setup(rt)` still stops the run.
    ///
    /// Fire-and-forget with no completion payload, so unlike every other sync
    /// method on this trait it never `block_on`s and therefore cannot deadlock
    /// when called from inside a tokio task. It can still block briefly: the
    /// host arm publishes over iceoryx2.
    fn request_runtime_shutdown(&self, reason: &str) -> Result<()>;

    // =========================================================================
    // Introspection
    // =========================================================================

    /// Export graph state as JSON including topology, processor states, metrics, and buffer levels.
    fn to_json(&self) -> Result<serde_json::Value>;
}
