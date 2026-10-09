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

/// A node in the graph: its per-run id and its name — for an added node, the
/// name it received: the one asked for, cast, or the class's short name with
/// any `-2` suffix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInTheGraph {
    /// The node's per-run id.
    pub processor_id: ProcessorUniqueId,
    /// The node's name.
    pub name: String,
}

/// The operations on one loaded stream's graph.
///
/// Implemented by [`LoadedStreamInThisRuntime`](crate::core::runtime::LoadedStreamInThisRuntime),
/// and by the control plane's test stubs. Callers use this trait and don't
/// need to know the underlying implementation.
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
///   Use these from sync code: `stream.add_processor(spec)`
///
/// The sync methods internally use `block_on`, so they must NOT be called from
/// within a tokio task (will panic). Use the async variants in async contexts.
pub trait RuntimeOperations: Send + Sync {
    // =========================================================================
    // Async Methods (primary implementation - safe from any context)
    // =========================================================================

    /// Add a processor to the graph asynchronously. Returns its id and the
    /// name it received.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the ID in fire-and-forget scenarios.
    fn add_processor_async(&self, spec: ProcessorSpec) -> BoxFuture<'_, Result<NodeInTheGraph>>;

    /// The node `node_name` names once cast, refused by name — listing the
    /// names the graph holds — when no node has it.
    fn the_node_named(&self, node_name: &str) -> Result<NodeInTheGraph>;

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
    /// `channel` is the port's address, `<runtime name>/<node>/<port>` — this
    /// runtime's own name for a port on one of its nodes; `count` bounds the tap to that
    /// many bags then ends, `None` streams live until the returned
    /// [`TapSubscription`] is dropped. The tap takes a subscriber slot on the
    /// channel with no publisher re-open; the channel reserves one slot beyond
    /// its destination cap, but iceoryx2 counts slots rather than naming them,
    /// so an attach fails with [`Error::TapSlotOccupied`] only once every slot
    /// is held. An unwired / unknown channel fails with
    /// [`Error::TapChannelNotFound`], and an address naming another runtime
    /// with [`Error::InvalidPortAddress`].
    ///
    /// There is no sync variant: a tap yields a live streaming handle, not a
    /// one-shot result, so blocking on it is never the intent. Host-side only —
    /// a plugin cdylib cannot own the host's `!Send` subscriber, so
    /// implementations reachable only across the plugin ABI reject this with
    /// [`Error::NotSupported`].
    ///
    /// [`Error::TapSlotOccupied`]: crate::core::error::Error::TapSlotOccupied
    /// [`Error::TapChannelNotFound`]: crate::core::error::Error::TapChannelNotFound
    /// [`Error::InvalidPortAddress`]: crate::core::error::Error::InvalidPortAddress
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
    // Identity
    // =========================================================================

    /// The name the runtime this stream is loaded in gives its tap channels
    /// and node-registry row.
    fn this_runtimes_name(&self) -> &str;

    // =========================================================================
    // Lifecycle
    // =========================================================================

    /// Ask for this stream's shutdown, with a human-readable `reason` logged
    /// for attribution. Every other stream in the runtime keeps running.
    ///
    /// A *request*, not a teardown: the stream's level moves to graceful and
    /// the stream is stopped and unloaded off the caller's thread. Idempotent
    /// — requesting twice is not an error and escalates nothing.
    ///
    /// Fire-and-forget with no completion payload, so unlike every other sync
    /// method on this trait it never `block_on`s and therefore cannot deadlock
    /// when called from inside a tokio task.
    fn request_this_streams_shutdown(&self, reason: &str) -> Result<()>;

    // =========================================================================
    // Introspection
    // =========================================================================

    /// Export graph state as JSON including topology, processor states, metrics, and buffer levels.
    fn to_json(&self) -> Result<serde_json::Value>;
}
