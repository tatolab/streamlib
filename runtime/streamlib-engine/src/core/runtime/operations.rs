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

/// One end of a link named before its node runs: the node's name and type, and the port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortOfANodeOfAType<'end> {
    /// The node's name, which a refusal names it by.
    pub node_name: &'end str,
    /// The node's type.
    pub node_type: &'end str,
    /// The port's name.
    pub port: &'end str,
}

/// What a live connect would make of one link, judged as it judges the link and made nowhere.
#[derive(Debug, Clone, PartialEq)]
pub enum WhatALiveConnectMakesOfALink {
    /// It would make the link: the verdict of two typed ends that connect, `None` while an end
    /// awaits the pin type its first link gives it. A verdict that connects through a convert node
    /// names the node a transaction's `connect` places between the two ends, connecting each to it.
    Makes(Option<Box<crate::core::descriptors::PinLinkVerdict>>),
    /// The link reaches a node that runs as no node of its own, such as a fragment, so the live
    /// graph judges it only as it is reconciled to a document holding it.
    JudgesItOnlyAsItIsMade,
    /// The link, at a level of a placed stream, reaches a node of that level that is not built
    /// to judge it, such as one whose type is not described.
    ReachesANodeNotBuilt(ANodeNotBuilt),
    /// The link reaches a port of a node type the stream has not described yet, which only an
    /// add of a node of the type describes, so it is judged only once one is placed.
    JudgesItOnlyOnceItsNodeTypeIsDescribed,
}

impl WhatALiveConnectMakesOfALink {
    /// The verdict the connect judges the link by: that of two typed ends, `None` while an end
    /// awaits the pin type its first link gives it; refused with why, in the runtime's words, when
    /// it makes the link without judging it.
    pub fn the_verdict_it_judges_by(
        self,
    ) -> std::result::Result<Option<Box<crate::core::descriptors::PinLinkVerdict>>, String> {
        match self {
            Self::Makes(verdict) => Ok(verdict),
            Self::JudgesItOnlyAsItIsMade => Err(
                "the link reaches a node that runs as no node of its own, such as a fragment"
                    .to_owned(),
            ),
            Self::ReachesANodeNotBuilt(not_built) => Err(not_built.to_string()),
            Self::JudgesItOnlyOnceItsNodeTypeIsDescribed => Err(
                "the node type is not described yet; place a node of it first, with `add_node`, \
                 and check the link to that node"
                    .to_owned(),
            ),
        }
    }
}

/// A node of a placed stream's level that is not built to judge a link reaching it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ANodeNotBuilt {
    /// The node's name, as the level names it.
    pub node_name: String,
    /// Why it is not built, in the runtime's words.
    pub why: String,
}

impl std::fmt::Display for ANodeNotBuilt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "the link reaches node `{}`, which is not built to judge it: {}",
            self.node_name, self.why
        )
    }
}

/// A port of a node type, by the type's import path and the port's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ANodeTypesPort<'a> {
    /// The node type's import path, as an add takes it.
    pub node_type: &'a str,
    /// The port's name, as the node type declares it.
    pub port: &'a str,
    /// The member of the bundle the port carries that the end is; `None` for the whole port.
    pub member: Option<&'a str>,
}

/// One writer's batch of graph edits on one stream, from
/// [`RuntimeOperations::open_a_graph_edit_batch`]. It is used on the thread that opened it: a
/// start, a stop or a resume of the stream on that thread is refused while it is open, since
/// each waits for it to close.
#[must_use = "a batch of graph edits is closed, or taken back when it is dropped"]
pub trait GraphEditBatch: Send {
    /// Apply `graph_operation` to the stream's live graph inside the batch, as
    /// [`RuntimeOperations::apply_a_stream_graph_operation_live`] applies it on its own, the rule
    /// a reactive reader sets left to the close.
    fn apply_a_stream_graph_operation_live(
        &self,
        graph_operation: streamlib_project_repository::StreamGraphOperation,
    ) -> BoxFuture<'_, Result<streamlib_project_repository::StreamGraphOperation>>;

    /// Bring the stream's live graph, inside the batch, to `graph_document`, its graph document
    /// as an edit staged it, or, with none, to itself as drawn, as a load of it would make it:
    /// each stream fragment placing a stream `stream_documents_an_edit_staged` holds places it
    /// as the edit staged it, and any other as its document, or its import's, is on disk now.
    /// Refused, the batch left to be taken back, as the reconcile refuses.
    fn reconcile_the_live_graph_to_the_streams_an_edit_staged<'batch>(
        &'batch self,
        graph_document: Option<serde_json::Value>,
        stream_documents_an_edit_staged: &'batch streamlib_project_repository::StreamDocumentsAnEditStaged,
    ) -> BoxFuture<'batch, Result<()>>;

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
    /// it as applied: an added node under the name it took. A running node it sets a setting
    /// read at setup on is not handed its new values but left to rebuild: the caller rebuilds
    /// it with [`Self::rebuild_each_node_a_setting_read_at_setup_changed`].
    fn apply_a_stream_graph_operation_live(
        &self,
        graph_operation: streamlib_project_repository::StreamGraphOperation,
    ) -> BoxFuture<'_, Result<streamlib_project_repository::StreamGraphOperation>>;

    /// Rebuild, once each, every running node whose setting read at setup a live operation set
    /// since the last rebuild, and wait for each to finish its setup; the UUID of each rebuilt.
    /// A stream not started rebuilds none, its nodes built from the values saved at its start.
    /// Refused in the node's own words when one does not build with the values set, the node
    /// left as its setup left it, for the caller to put its earlier values back and rebuild it.
    fn rebuild_each_node_a_setting_read_at_setup_changed(
        &self,
    ) -> BoxFuture<'_, Result<Vec<uuid::Uuid>>>;

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
    /// made nowhere; refused as the connect is refused, but for a link that connects through a
    /// convert node, which answers the verdict naming it.
    fn what_a_live_connect_makes_of_the_link(
        &self,
        link: &streamlib_project_repository::StreamGraphDocumentLink,
    ) -> Result<WhatALiveConnectMakesOfALink>;

    /// The verdict of `link`, between two nodes this stream's live graph runs, each by its UUID,
    /// by its two typed ends, whether the graph holds the link or not; `None` when either end is
    /// no typed port of a node that runs.
    fn the_verdict_of_a_link_between_two_running_nodes(
        &self,
        link: &streamlib_project_repository::StreamGraphDocumentLink,
    ) -> Option<crate::core::descriptors::PinLinkVerdict>;

    /// The verdict of a link from the output `from` names to the input `to` names, judged by the
    /// ports this stream resolves each node type to declare, among the conversions it resolves,
    /// before either node runs; `None` when either port is untyped or not declared.
    fn the_verdict_of_a_link_between_two_node_types(
        &self,
        from: PortOfANodeOfAType<'_>,
        to: PortOfANodeOfAType<'_>,
    ) -> Option<crate::core::descriptors::PinLinkVerdict>;

    /// What a live connect would make of each link between `held`, an end of this stream's
    /// live graph by its node's UUID and of `held_direction`, and one of `node_types_ports`, in
    /// their order, were a node of the type placed as an add with no input values places it:
    /// judged as [`Self::what_a_live_connect_makes_of_the_link`] judges a link, the port by what
    /// it declares, and made nowhere; each refused as its connect would be refused. Nothing is
    /// described: a node type the stream has not described yet is answered
    /// [`WhatALiveConnectMakesOfALink::JudgesItOnlyOnceItsNodeTypeIsDescribed`]. The judgement
    /// runs in this stream's log route, so what it logs reaches the stream's log.
    fn what_a_live_connect_makes_of_links_with_node_types_ports<'a>(
        &'a self,
        held: &'a streamlib_project_repository::StreamGraphDocumentLinkEnd,
        held_direction: crate::core::PortDirection,
        node_types_ports: &'a [ANodeTypesPort<'a>],
    ) -> BoxFuture<'a, Result<Vec<Result<WhatALiveConnectMakesOfALink>>>>;

    /// What a connect made now would make of each of `links`, in their order, at `level`, a
    /// level of a placed stream as its graph document holds it, each node by its UUID there,
    /// made nowhere: judged among that level's nodes, by their types as this stream resolves
    /// them and the conversions it registers, as [`Self::what_a_live_connect_makes_of_the_link`]
    /// judges a link of the live graph, each link among the ones before it that connect made;
    /// each refused as its connect is refused.
    fn what_a_connect_makes_of_each_link_at_a_placed_level(
        &self,
        level: &streamlib_project_repository::StreamGraphDocument,
        links: &[streamlib_project_repository::StreamGraphDocumentLink],
    ) -> Vec<Result<WhatALiveConnectMakesOfALink>>;

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

    /// The ports a node of type `node_type` has, as this stream resolves the type: the way in,
    /// from outside the engine, to the one lookup of a port by its current name or a name it
    /// had, and to the one order a value held under several of its names is read in.
    fn the_ports_a_node_type_declares(
        &self,
        node_type: &str,
    ) -> Option<crate::core::GraphNodePorts>;

    /// The pin groups a node of type `node_type` repeats, as this stream resolves the type; none
    /// for a type it does not resolve.
    fn the_pin_groups_a_node_type_declares(
        &self,
        node_type: &str,
    ) -> Vec<crate::core::descriptors::PinGroupDescriptor>;

    /// Refuse `value`, set on input `input` of the node named `node_name` of type `node_type`,
    /// or on its member `member` when one is named, and held on the graph, with `None` taking it
    /// off, when the input does not take it, as this stream resolves the type: among them a value
    /// on the input of an output a panel writes, which holds no saved value;
    /// `input_values_after_the_set` are every input value the node holds once it is made.
    fn refuse_an_input_value_its_input_does_not_take(
        &self,
        node_type: &NodeClassImportPath,
        node_name: &str,
        input_values_after_the_set: &serde_json::Map<String, serde_json::Value>,
        input: &str,
        member: Option<&str>,
        value: Option<&serde_json::Value>,
    ) -> std::result::Result<(), crate::core::graph::input_values::InputValueRefusal>;

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
