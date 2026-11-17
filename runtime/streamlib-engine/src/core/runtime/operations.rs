// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::descriptors::NodeClassImportPath;
use crate::core::error::Result;
use crate::core::graph::input_values::{
    InputValueToSetLive, InputValuesSetLive, LiveInputValueSetRefusal,
};
use crate::core::graph::{LinkUniqueId, NodeRunId};
use crate::core::nodes::NodeSpec;
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
    pub node_run_id: NodeRunId,
    /// The node's name.
    pub name: String,
}

pub trait RuntimeOperations: Send + Sync {
    // =========================================================================
    // Async Methods (primary implementation - safe from any context)
    // =========================================================================

    fn add_node_async(&self, spec: NodeSpec) -> BoxFuture<'_, Result<NodeInTheGraph>>;

    /// The node `node_name` names once cast, refused by name — listing the
    /// names the graph holds — when no node has it.
    fn the_node_named(&self, node_name: &str) -> Result<NodeInTheGraph>;

    fn remove_node_async(&self, node_run_id: NodeRunId) -> BoxFuture<'_, Result<()>>;

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

    /// Apply `graph_operation` to this stream's live graph, each node by its UUID, answering
    /// it as applied: an added node under the name it took.
    fn apply_a_stream_graph_operation_live(
        &self,
        graph_operation: streamlib_project_repository::StreamGraphOperation,
    ) -> BoxFuture<'_, Result<streamlib_project_repository::StreamGraphOperation>>;

    /// The name a node of type `node_type` added with none takes at a level whose nodes are
    /// named `names_at_the_level`, as this stream resolves the type.
    fn the_name_a_node_added_with_none_takes(
        &self,
        node_type: &str,
        names_at_the_level: &std::collections::HashSet<String>,
    ) -> Result<String>;

    /// Refuse a node of type `node_type` named `node_name` holding `input_values` that a live
    /// add to this stream would refuse: a type it does not resolve once described, or a config
    /// the type's config schema refuses.
    fn refuse_a_node_a_live_add_would_refuse(
        &self,
        node_type: &str,
        node_name: &str,
        input_values: &serde_json::Map<String, serde_json::Value>,
    ) -> BoxFuture<'_, Result<()>>;

    /// The config a node of type `node_type` holding `input_values` takes whole, as this stream
    /// resolves the type: the input values its config schema lists.
    fn the_config_a_node_holding_the_input_values_takes(
        &self,
        node_type: &str,
        input_values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value>;

    /// The name the port of a node of type `node_type` that `name` finds on its `direction`
    /// side has now, when `name` is one that port had; `None` for a current name and for one
    /// that finds no port, as this stream resolves the type.
    fn the_current_name_a_former_port_name_finds(
        &self,
        node_type: &str,
        direction: crate::core::PortDirection,
        name: &str,
    ) -> Option<String>;

    /// Refuse `value`, set on input `input` of the node named `node_name` of type `node_type`,
    /// with `None` taking it off, when the input does not take it, as this stream resolves the
    /// type; `input_values_after_the_set` are every input value the node holds once it is made.
    fn refuse_an_input_value_its_input_does_not_take(
        &self,
        node_type: &NodeClassImportPath,
        node_name: &str,
        input_values_after_the_set: &serde_json::Map<String, serde_json::Value>,
        input: &str,
        value: Option<&serde_json::Value>,
    ) -> std::result::Result<(), crate::core::graph::input_values::InputValueRefusal>;

    /// Whether a node of type `node_type` still takes a config, as this stream resolves the
    /// type: its config schema lists a field, or the stream does not describe it.
    fn node_type_takes_a_config(&self, node_type: &NodeClassImportPath) -> bool;

    /// Whether an exposure of `exposed_config_fields` on a node of type `node_type` is recorded:
    /// true for a type that still takes a config; false for one that takes none, on which each
    /// name must be one of its inputs, refused by name otherwise.
    fn an_exposure_of_config_fields_is_recorded_on(
        &self,
        node_type: &NodeClassImportPath,
        exposed_config_fields: &[String],
    ) -> Result<bool>;

    /// Set each of `sets` live, apart from the graph, when each is on an input its node reads
    /// each run: every value checked before any is set, then each handed to the helper and the
    /// islands reading it and announced as `InputValueDidChange`, with no graph edit and no
    /// compile; whole or not at all. None is set when one is held on the graph, so the set takes
    /// the path a graph edit takes.
    fn set_input_values_live(
        &self,
        sets: &[InputValueToSetLive],
    ) -> std::result::Result<InputValuesSetLive, LiveInputValueSetRefusal>;

    /// Bring each stream fragment of this stream's live graph to the stream it references, as
    /// that stream's graph document, or its import's, is on disk now.
    fn reconcile_each_stream_fragment_to_the_stream_it_references_live(
        &self,
    ) -> BoxFuture<'_, Result<()>>;

    /// Export graph state as JSON asynchronously.
    fn to_json_async(&self) -> BoxFuture<'_, Result<serde_json::Value>>;

    /// Hold output port `port_name` of the node whose UUID, as `graph` lists it under `id`, is
    /// `node_id` open for a viewer,
    /// answering how the viewer subscribes to its channel's reserved slot from
    /// its own process. No node is added and the graph is not changed; the
    /// channel stays open until what this answers drops. Refused while the
    /// node does not run, and when its helper process cannot open the port.
    /// On macOS, process `process_id_of_the_viewer` is admitted to the
    /// surface-sharing service while the viewer stays open.
    fn open_a_viewer_of_an_output_port(
        &self,
        node_id: String,
        port_name: String,
        process_id_of_the_viewer: Option<u32>,
    ) -> BoxFuture<'_, Result<crate::core::runtime::OutputPortViewerHeldOpen>>;

    /// Hand `value` to the view node whose UUID, as `graph` lists it under `id`, is `node_id`,
    /// which publishes it on its output `port_name` as that port's family carries it. Live data:
    /// nothing is recorded, and the last value written is what a reader keeps.
    fn write_a_value_onto_an_output_of_a_view_node(
        &self,
        node_id: String,
        port_name: String,
        value: serde_json::Value,
    ) -> BoxFuture<'_, Result<()>>;

    /// Hand `value` to the stream's top-level `GraphInput` declaring the Input `input_name`,
    /// which publishes it on its `value` output to every reader, answering the Input's family.
    /// Live data: nothing is recorded in the graph or history and the last write wins; a `kept`
    /// Input keeps it in the project's store, and a `reset` one returns to its default at Stop.
    /// While the project is stopped, a `kept` Input keeps the value for its next Play and a
    /// `reset` one is refused.
    fn write_a_value_to_a_top_level_graph_input(
        &self,
        input_name: String,
        value: serde_json::Value,
    ) -> BoxFuture<'_, Result<crate::core::descriptors::PortFamily>>;

    // =========================================================================
    // Sync Methods (convenience wrappers - NOT safe from tokio tasks)
    // =========================================================================

    fn add_node(&self, spec: NodeSpec) -> Result<NodeRunId>;

    fn remove_node(&self, node_run_id: &NodeRunId) -> Result<()>;

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

    /// Ask that function node `function_node_id`, whose call crashed or stalled
    /// the helper of `reader_id`, move with every function node downstream of
    /// it out of that reader's islands into a helper process of its own.
    ///
    /// A request that never blocks: the graph-change listener compiles it, and
    /// the reader's link wiring learns how the compile ended. A compile plans
    /// it only while `promotion_request_number` is the request the reader's
    /// link wiring still awaits.
    fn request_a_function_node_promotion_out_of_the_islands_of_the_reader_it_crashed(
        &self,
        function_node_id: &NodeRunId,
        reader_id: &NodeRunId,
        promotion_request_number: u64,
        reason: &str,
    ) -> Result<()>;

    // =========================================================================
    // Introspection
    // =========================================================================

    fn to_json(&self) -> Result<serde_json::Value>;
}
