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

/// What a live connect would make of one link, judged as it judges the link and made nowhere.
#[derive(Debug, Clone, PartialEq)]
pub enum WhatALiveConnectMakesOfALink {
    /// It would make the link: the verdict of two typed ends that connect, `None` for ends
    /// their families judge.
    Makes(Option<Box<crate::core::descriptors::PinLinkVerdict>>),
    /// The link reaches a node that runs as no node of its own, such as a fragment, so the live
    /// graph judges it only as it is reconciled to a document holding it.
    JudgesItOnlyAsItIsMade,
}

/// One writer's batch of graph edits on one stream, from
/// [`RuntimeOperations::open_a_graph_edit_batch`].
#[must_use = "a batch of graph edits is closed, or taken back when it is dropped"]
pub trait GraphEditBatch: Send {
    /// Apply `graph_operation` to the stream's live graph inside the batch, as
    /// [`RuntimeOperations::apply_a_stream_graph_operation_live`] applies it on its own, the rule
    /// a reactive reader sets left to the close.
    fn apply_a_stream_graph_operation_live(
        &self,
        graph_operation: streamlib_project_repository::StreamGraphOperation,
    ) -> BoxFuture<'_, Result<streamlib_project_repository::StreamGraphOperation>>;

    /// Close the batch: refused, and taken back, when the graph it leaves holds a reader that
    /// runs reactively fed only by function node islands, or when its one commit is refused;
    /// otherwise compiled in one commit, and each event of what stands announced. A refusal
    /// says how far the batch was taken back.
    ///
    /// # Panics
    ///
    /// When called from inside an asynchronous task: it waits for its commit on the engine's
    /// runtime, as every synchronous method of [`RuntimeOperations`] that waits does.
    fn close(self: Box<Self>) -> std::result::Result<(), GraphEditBatchRefused>;
}

/// How far a batch of graph edits whose close was refused was taken back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HowFarARefusedBatchWasTakenBack {
    /// Every edit: the stream is as it was before the batch opened.
    Whole,
    /// Its one commit failed after it took something out of the running stream or built into
    /// it, so only what nothing built or wired holds was taken back: the caller reverses each
    /// edit it applied, as it would edits each committed on its own.
    AsFarAsNothingBuiltOrWiredHoldsIt,
}

/// A batch of graph edits whose close was refused: why, and how far it was taken back.
#[derive(Debug)]
pub struct GraphEditBatchRefused {
    pub refusal: crate::core::error::Error,
    pub taken_back: HowFarARefusedBatchWasTakenBack,
}

impl std::fmt::Display for GraphEditBatchRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.refusal.fmt(f)
    }
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

    /// Hold `graph_operation`, which this stream's graph document holds and its live graph
    /// refused in the runtime's words `refusal`, as an edit not live yet: listed among the
    /// stream's diagnostics and drawn in its `graph` until its playback stops.
    fn hold_an_edit_its_document_holds_not_live(
        &self,
        graph_operation: &streamlib_project_repository::StreamGraphOperation,
        refusal: &str,
    );

    /// What a live connect of `link`, each node by its UUID, would make of it on this stream's
    /// live graph, judged as [`Self::apply_a_stream_graph_operation_live`] judges a connect and
    /// made nowhere; refused as the connect is refused.
    fn what_a_live_connect_makes_of_the_link(
        &self,
        link: &streamlib_project_repository::StreamGraphDocumentLink,
    ) -> Result<WhatALiveConnectMakesOfALink>;

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

    /// The ports a node of type `node_type` has, as this stream resolves the type: the way in,
    /// from outside the engine, to the one lookup of a port by its current name or a name it
    /// had, and to the one order a value held under several of its names is read in.
    fn the_ports_a_node_type_declares(
        &self,
        node_type: &str,
    ) -> Option<crate::core::GraphNodePorts>;

    /// Refuse `value`, set on input `input` of the node named `node_name` of type `node_type`
    /// and held on the graph, with `None` taking it off, when the input does not take it, as this
    /// stream resolves the type: among them a value on the input of an output a panel writes,
    /// which holds no saved value; `input_values_after_the_set` are every input value the node
    /// holds once it is made.
    fn refuse_an_input_value_its_input_does_not_take(
        &self,
        node_type: &NodeClassImportPath,
        node_name: &str,
        input_values_after_the_set: &serde_json::Map<String, serde_json::Value>,
        input: &str,
        value: Option<&serde_json::Value>,
    ) -> std::result::Result<(), crate::core::graph::input_values::InputValueRefusal>;

    /// Refuse `value`, saved on input `input` of the node named `node_name` of type `node_type`,
    /// when that input is the input of an output a panel writes, which holds what a panel writes,
    /// live, and no saved value; taking a value off is no refusal.
    fn refuse_a_saved_value_on_the_input_of_an_output_a_panel_writes(
        &self,
        node_type: &NodeClassImportPath,
        node_name: &str,
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

    /// This stream's graph as `graph` answers it: its running graph, or, while its graph
    /// document holds edits the running graph did not take, the document's graph over it, each
    /// part not live yet carrying `live: false`. Every edit and check reads `to_json_async`.
    fn the_graph_as_answered_async(&self) -> BoxFuture<'_, Result<serde_json::Value>>;

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

    /// The UUID of the running view node whose UUID, as `graph` lists it under `id`, is
    /// `node_id`, and one of whose outputs a panel writes `value` onto, as `port_name`: the node
    /// whose input of that output's name the write sets, live, which it publishes on the output.
    /// Refused while the stream is stopped; for a node not of a view type, not running, or with
    /// no such output; and for a value no panel writes there: any onto an Image or Packets
    /// output, and `null`.
    fn the_running_view_node_a_panel_writes(
        &self,
        node_id: String,
        port_name: String,
        value: serde_json::Value,
    ) -> BoxFuture<'_, Result<uuid::Uuid>>;

    /// The top-level `GraphInput` declaring the stream's input `input_name`, whose `value` input
    /// a set naming no node sets, and why its node does not run while the stream's playback is
    /// not stopped. Refused for a name no top-level `GraphInput` declares, naming the stream's
    /// inputs.
    fn the_top_level_graph_input_declaring(
        &self,
        input_name: String,
    ) -> BoxFuture<'_, Result<crate::core::runtime::TopLevelGraphInputDeclaringAStreamInput>>;

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

    /// Open a batch of graph edits on this stream for one writer, once no other writer's are
    /// open on it: each operation applied through it is logged into one compiler transaction,
    /// and closing it judges the graph the batch leaves and compiles it in one commit. Dropped
    /// open, the batch is taken back as far as nothing built or wired holds it. A start, a stop
    /// or a resume of the stream waits for the batch to close.
    ///
    /// # Panics
    ///
    /// When it waits for another writer from inside an asynchronous task, as every synchronous
    /// method of this trait that waits does.
    fn open_a_graph_edit_batch(self: std::sync::Arc<Self>) -> Result<Box<dyn GraphEditBatch>>;

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
