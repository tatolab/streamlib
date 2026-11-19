// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashSet;
use std::sync::Arc;

use serde::Serialize;
use std::path::{Path, PathBuf};

use super::LoadedStreamInThisRuntime;
use super::RuntimeStatus;
use super::node_interpreters::node_interpreter_launch_record::NodeInterpreterLaunchRecordOfOneStream;
use super::operations::{
    BoxFuture, NodeInTheGraph, RuntimeOperations, WhatALiveConnectMakesOfALink,
};
use super::runtime::TokioRuntimeVariant;
use super::streams_loaded_in_this_runtime::live_graph_reconciled_to_a_graph_document::LinksAReconcileMakes;
use crate::core::RuntimeContext;
use crate::core::compiler::{Compiler, CompilerTransactionHandle, PendingOperation};
use crate::core::descriptors::NodeClassImportPath;
use crate::core::graph::graph_snapshot::NodeChangeADrawnLinkBlocks;
use crate::core::graph::input_values::{
    InputValueToSetLive, InputValuesSetLive, LiveInputValueSetRefusal,
};
use crate::core::graph::{
    ALinkConnected, Graph, GraphEdgeWithComponents, GraphNodeWithComponents, LinkUniqueId,
    NodeInstanceComponent, NodeRunId, PendingDeletionComponent, StateComponent, WhyTheNodeLeaves,
    node_named_or_refused,
};
use crate::core::logging::{
    carrying_this_threads_project_log_route_entry, polled_in_a_project_log_route_entry,
};
use crate::core::nodes::{NodeSpec, NodeState};
use crate::core::pubsub::{LoadedStreamIdentity, RuntimeEvent};
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};

mod link_connected_between_two_ports;
mod link_ends_this_graph_cannot_take;
pub(super) use link_connected_between_two_ports::{
    connect_impl, the_verdict_connect_takes_on_a_link,
};

// =============================================================================
// Core Implementation Functions ('static async fns for spawn compatibility)
// =============================================================================

type LiveCommitContext = Option<Arc<RuntimeContext>>;

/// What every graph change on one stream works through: the stream's
/// compiler, the context it compiles against right away, if any, and the
/// stream whose topic it publishes on.
pub(super) struct LiveGraphChangeOnOneStream {
    compiler: Arc<Compiler>,
    live: LiveCommitContext,
    stream: LoadedStreamIdentity,
}

thread_local! {
    static IS_A_NODE_EXECUTION_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn mark_this_thread_as_a_node_execution_thread() {
    IS_A_NODE_EXECUTION_THREAD.with(|marker| marker.set(true));
}

fn this_thread_may_commit_inline() -> bool {
    !IS_A_NODE_EXECUTION_THREAD.with(|marker| marker.get())
}

async fn commit_live_graph_change(compiler: &Arc<Compiler>, live: LiveCommitContext) -> Result<()> {
    let Some(runtime_ctx) = live else {
        take_out_now_each_node_and_link_pending_deletion_no_commit_builds(compiler);
        hand_each_config_update_to_its_node_now_no_commit_builds(compiler);
        return Ok(());
    };
    let compiler = Arc::clone(compiler);
    tokio::task::spawn_blocking(carrying_this_threads_project_log_route_entry(move || {
        compiler.commit(&runtime_ctx)
    }))
    .await
    .map_err(|join_failure| {
        Error::Runtime(format!(
            "the graph change's compile task did not finish: {join_failure}"
        ))
    })?
}

/// On a stream no commit builds, a stopped one or one not started, take each node and link
/// pending deletion out of the graph now, with every operation logged for it: the graph it holds
/// to read and edit then shows the removal, and the UUID and name it held are free again.
fn take_out_now_each_node_and_link_pending_deletion_no_commit_builds(compiler: &Compiler) {
    compiler.scope(|this_streams_graph, tx| {
        let links_pending_deletion: HashSet<LinkUniqueId> = this_streams_graph
            .traversal()
            .e(())
            .iter()
            .filter(|link| link.has::<PendingDeletionComponent>())
            .map(|link| link.id.clone())
            .collect();
        let nodes_pending_deletion: HashSet<NodeRunId> = this_streams_graph
            .traversal()
            .v(())
            .iter()
            .filter(|node| {
                node.has::<PendingDeletionComponent>() && !node.has::<NodeInstanceComponent>()
            })
            .map(|node| node.id.clone())
            .collect();
        tx.retract_every_operation_naming(&nodes_pending_deletion, &links_pending_deletion);
        for link_id in &links_pending_deletion {
            this_streams_graph.traversal_mut().e(link_id).drop();
        }
        for node_run_id in &nodes_pending_deletion {
            if !this_streams_graph.drop_the_node(node_run_id, WhyTheNodeLeaves::Removed) {
                tracing::warn!("`{node_run_id}`, pending deletion, was not dropped from the graph");
            }
        }
    });
}

/// On a stream no commit builds, a stopped one or one not started, hand each config update
/// logged for a node no instance runs to that node now: the graph it holds to read and edit then
/// shows the config, and the node is built from it at the next start.
fn hand_each_config_update_to_its_node_now_no_commit_builds(compiler: &Compiler) {
    compiler.scope(|this_streams_graph, tx| {
        let nodes_no_instance_runs: HashSet<NodeRunId> = this_streams_graph
            .traversal()
            .v(())
            .iter()
            .filter(|node| !node.has::<NodeInstanceComponent>())
            .map(|node| node.id.clone())
            .collect();
        for (node_run_id, config_to_apply) in tx.take_each_config_update_of(&nodes_no_instance_runs)
        {
            if let Some(node) = this_streams_graph
                .traversal_mut()
                .v(&node_run_id)
                .first_mut()
            {
                node.set_config(config_to_apply);
            }
        }
    });
}

/// Whether an added node's config references are checked against the graph it joins.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfigReferencesOfTheAddedNode {
    /// A live add: every node a reference names must already be in the graph.
    CheckedAgainstTheGraphItJoins,
    /// A graph load: the whole graph was validated before its first node was
    /// added, so a reference may name a node listed after it.
    ValidatedWithTheWholeGraphBeforeTheAdd,
}

fn a_load_holds_the_type_as_a_placeholder(
    node_interpreter_launch_record: &NodeInterpreterLaunchRecordOfOneStream,
    node_type: &NodeClassImportPath,
    config_references_of_the_added_node: ConfigReferencesOfTheAddedNode,
) -> bool {
    config_references_of_the_added_node
        == ConfigReferencesOfTheAddedNode::ValidatedWithTheWholeGraphBeforeTheAdd
        && node_interpreter_launch_record
            .node_types_this_stream_resolves()
            .placeholder_of_the_node_type_not_described(node_type)
            .is_some()
}

/// Describe `node_type` in the stream's own interpreter when the stream does not resolve it
/// yet, off the async worker, since a describe runs for as long as the module's import does;
/// a type a load holds as a placeholder is not described. Once the type is described a later
/// call does nothing, so an add that splits a node's input values by its type describes it
/// first, and the add itself describes nothing again.
pub(super) async fn describe_the_type_an_add_names_when_it_must(
    node_interpreter_launch_record: &Arc<NodeInterpreterLaunchRecordOfOneStream>,
    node_type: &NodeClassImportPath,
    lend_directory: Option<PathBuf>,
    config_references_of_the_added_node: ConfigReferencesOfTheAddedNode,
) -> Result<()> {
    if a_load_holds_the_type_as_a_placeholder(
        node_interpreter_launch_record,
        node_type,
        config_references_of_the_added_node,
    ) || !node_interpreter_launch_record.a_live_add_must_describe(node_type)
    {
        return Ok(());
    }
    let node_type = node_type.clone();
    let launch_record = Arc::clone(node_interpreter_launch_record);
    tokio::task::spawn_blocking(carrying_this_threads_project_log_route_entry(move || {
        launch_record
            .describe_and_register_a_type_a_live_add_names(&node_type, lend_directory.as_deref())
    }))
    .await
    .map_err(|join_failure| {
        Error::Runtime(format!(
            "the describe of a node type did not finish: {join_failure}"
        ))
    })?
}

pub(super) async fn add_node_impl(
    live_graph_change: LiveGraphChangeOnOneStream,
    node_interpreter_launch_record: Arc<NodeInterpreterLaunchRecordOfOneStream>,
    lend_directory: Option<PathBuf>,
    spec: NodeSpec,
    config_references_of_the_added_node: ConfigReferencesOfTheAddedNode,
) -> Result<NodeInTheGraph> {
    let LiveGraphChangeOnOneStream {
        compiler,
        live,
        stream,
    } = live_graph_change;
    let emit_will_add = |id: &NodeRunId| {
        stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeWillAddNode {
            node_run_id: id.clone(),
        });
    };

    let emit_did_add = |id: &NodeRunId| {
        stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeDidAddNode {
            node_run_id: id.clone(),
        });
    };

    let a_load_holds_its_type_as_a_placeholder = a_load_holds_the_type_as_a_placeholder(
        &node_interpreter_launch_record,
        &spec.name,
        config_references_of_the_added_node,
    );
    describe_the_type_an_add_names_when_it_must(
        &node_interpreter_launch_record,
        &spec.name,
        lend_directory,
        config_references_of_the_added_node,
    )
    .await?;

    let ident_for_err = spec.name.clone();

    let added = compiler.scope(|graph, tx| -> Result<NodeInTheGraph> {
        let (node_id, assigned_display_name) = graph
            .traversal_mut()
            .add_v(spec)?
            .first()
            .map(|node| (node.id.clone(), node.display_name.clone()))
            .ok_or_else(|| Error::GraphError("Could not create node".into()))?;

        // Registry miss: `add_v` already attached `StateComponent(Error)` so
        // the failed node is visible via `GET /api/graph`. Skip pending-op
        // logging so the compiler doesn't try to spawn it. Emit the
        // graph-changed events so subscribers see the new node, then surface
        // the typed error.
        let registry_miss = graph
            .traversal()
            .v(&node_id)
            .first()
            .and_then(|node| node.get::<StateComponent>())
            .map(|state_component| state_component.current() == NodeState::Error)
            .unwrap_or(false);

        if registry_miss {
            emit_will_add(&node_id);
            emit_did_add(&node_id);
            if a_load_holds_its_type_as_a_placeholder {
                return Ok(NodeInTheGraph {
                    node_run_id: node_id,
                    name: assigned_display_name,
                });
            }
            return Err(Error::UnknownNodeType {
                ident: ident_for_err,
            });
        }

        if config_references_of_the_added_node
            == ConfigReferencesOfTheAddedNode::CheckedAgainstTheGraphItJoins
        {
            let Some((name_of_the_added_node, type_of_the_added_node, config_of_the_added_node)) =
                graph.traversal().v(&node_id).first().map(|node| {
                    (
                        node.display_name.clone(),
                        node.node_type.clone(),
                        node.config.clone().unwrap_or_default(),
                    )
                })
            else {
                return Err(Error::GraphError("Could not create node".into()));
            };
            let node_types = node_interpreter_launch_record.node_types_this_stream_resolves();
            if let Err(refusal) =
                crate::core::graph::refuse_a_config_its_node_types_config_schema_refuses(
                    &name_of_the_added_node,
                    &type_of_the_added_node,
                    node_types,
                    &config_of_the_added_node,
                )
                .and_then(|()| {
                    crate::core::graph::refuse_a_live_config_naming_a_node_the_graph_does_not_hold(
                        graph,
                        node_types,
                        &node_id,
                        &config_of_the_added_node,
                    )
                })
            {
                if !graph.drop_the_node(&node_id, WhyTheNodeLeaves::Removed) {
                    tracing::warn!("`{node_id}`, whose add was refused, was not dropped");
                }
                return Err(refusal);
            }
        }

        emit_will_add(&node_id);
        tx.log(PendingOperation::AddNode(node_id.clone()));
        emit_did_add(&node_id);
        Ok(NodeInTheGraph {
            node_run_id: node_id,
            name: assigned_display_name,
        })
    })?;

    commit_live_graph_change(&compiler, live).await?;

    stream.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);

    Ok(added)
}

fn mark_the_node_and_each_of_its_links_pending_deletion(
    graph: &mut Graph,
    tx: &CompilerTransactionHandle,
    node_run_id: &NodeRunId,
) -> Result<()> {
    if !graph.traversal().v(node_run_id).exists() {
        return Err(Error::NodeNotFound(node_run_id.to_string()));
    }
    let inbound_links: Vec<LinkUniqueId> = graph
        .traversal_mut()
        .v(node_run_id)
        .in_e()
        .iter()
        .map(|link| link.id.clone())
        .collect();
    let outbound_links: Vec<LinkUniqueId> = graph
        .traversal_mut()
        .v(node_run_id)
        .out_e()
        .iter()
        .map(|link| link.id.clone())
        .collect();
    for link_id in inbound_links.into_iter().chain(outbound_links) {
        if let Some(link) = graph.traversal_mut().e(&link_id).first_mut() {
            link.insert(PendingDeletionComponent);
        }
        tx.log(PendingOperation::RemoveLink(link_id));
    }
    if let Some(node) = graph.traversal_mut().v(node_run_id).first_mut() {
        node.insert(PendingDeletionComponent);
    }
    tx.log(PendingOperation::RemoveNode(
        node_run_id.clone(),
        WhyTheNodeLeaves::Removed,
    ));
    Ok(())
}

pub(super) async fn remove_node_impl(
    live_graph_change: LiveGraphChangeOnOneStream,
    node_run_id: NodeRunId,
) -> Result<()> {
    let LiveGraphChangeOnOneStream {
        compiler,
        live,
        stream,
    } = live_graph_change;
    compiler.scope(|graph, tx| {
        mark_the_node_and_each_of_its_links_pending_deletion(graph, tx, &node_run_id)
    })?;

    let removal_outcome = commit_live_graph_change(&compiler, live).await;
    if removal_outcome.is_err()
        && compiler.scope(|graph, _tx| graph.traversal().v(&node_run_id).exists())
    {
        return removal_outcome;
    }

    stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeWillRemoveNode {
        node_run_id: node_run_id.clone(),
    });
    stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeDidRemoveNode {
        node_run_id: node_run_id.clone(),
    });
    stream.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);

    removal_outcome
}

/// The node `node_name` names once cast in the stream `stream_id`, whose
/// graph `compiler` holds.
///
/// Refused by name when the stream holds no node by that name, listing the
/// ones it does.
fn the_node_this_stream_names(
    compiler: &Arc<Compiler>,
    stream_id: &str,
    node_name: &str,
) -> Result<NodeInTheGraph> {
    compiler.scope(|graph, _tx| {
        node_named_or_refused(graph, stream_id, node_name).map(|named| NodeInTheGraph {
            node_run_id: named.id.clone(),
            name: named.display_name.clone(),
        })
    })
}

/// Core implementation for disconnect - takes owned Arcs for 'static lifetime.
pub(super) async fn disconnect_impl(
    live_graph_change: LiveGraphChangeOnOneStream,
    link_id: LinkUniqueId,
) -> Result<()> {
    let LiveGraphChangeOnOneStream {
        compiler,
        live,
        stream,
    } = live_graph_change;
    let link_info = compiler.scope(|graph, tx| {
        let (from_value, to_value) = graph
            .traversal()
            .e(&link_id)
            .first()
            .map(|l| (l.source(), l.to_port()))
            .ok_or_else(|| Error::NotFound(format!("Link '{}' not found", link_id)))?;

        let info = (from_value.clone(), to_value.clone());

        if let Some(link) = graph.traversal_mut().e(&link_id).first_mut() {
            link.insert(PendingDeletionComponent);
        }

        tx.log(PendingOperation::RemoveLink(link_id.clone()));

        Ok::<_, Error>(info)
    })?;

    commit_live_graph_change(&compiler, live).await?;

    stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeWillDisconnect {
        link_id: link_id.to_string(),
        from_port: link_info.0.to_string(),
        to_port: link_info.1.to_string(),
    });
    stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeDidDisconnect {
        link_id: link_id.to_string(),
        from_port: link_info.0.to_string(),
        to_port: link_info.1.to_string(),
    });
    stream.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);

    Ok(())
}

impl LoadedStreamInThisRuntime {
    fn live_commit_context(&self) -> LiveCommitContext {
        if !this_thread_may_commit_inline() {
            return None;
        }
        if *self.status.lock() != RuntimeStatus::Started {
            return None;
        }
        self.runtime_context.lock().clone()
    }

    /// A graph change on this stream, compiled right away when it may be.
    pub(super) fn live_graph_change(&self) -> LiveGraphChangeOnOneStream {
        LiveGraphChangeOnOneStream {
            compiler: Arc::clone(&self.compiler),
            live: self.live_commit_context(),
            stream: self.loaded_stream_identity().clone(),
        }
    }

    pub(super) async fn update_node_config_compiled_right_away<C: Serialize>(
        &self,
        node_run_id: &NodeRunId,
        config: C,
    ) -> Result<()> {
        self.log_a_node_config_update_for_the_next_commit(node_run_id, config)?;
        commit_live_graph_change(&self.compiler, self.live_commit_context()).await?;
        self.hold_each_live_value_whose_lifetime_the_config_changed(node_run_id);
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        Ok(())
    }

    /// Update the config of `node_run_id` to what `build` makes of the config it will hold once
    /// the updates already logged apply, built and logged in one step, then commit it.
    pub(super) async fn update_node_config_built_from_the_one_it_holds(
        &self,
        node_run_id: &NodeRunId,
        build: impl FnOnce(Option<serde_json::Value>) -> Result<serde_json::Value>,
    ) -> Result<()> {
        self.log_a_node_config_update_for_the_next_commit_built_from(node_run_id, build)?;
        commit_live_graph_change(&self.compiler, self.live_commit_context()).await?;
        self.hold_each_live_value_whose_lifetime_the_config_changed(node_run_id);
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        Ok(())
    }

    fn tokio_runtime_variant(&self) -> &TokioRuntimeVariant {
        &self
            .engine_resources_shared_by_every_stream()
            .tokio_runtime_variant
    }

    pub(super) fn lend_directory(&self) -> Option<PathBuf> {
        self.engine_resources_shared_by_every_stream()
            .node_interpreter_lend_directory
            .get()
            .map(Path::to_path_buf)
    }

    /// `operation`, polled with this stream's log route entered wherever it
    /// runs.
    fn in_this_streams_log_route<'operation, T>(
        &self,
        operation: impl std::future::Future<Output = Result<T>> + Send + 'operation,
    ) -> BoxFuture<'operation, Result<T>> {
        Box::pin(polled_in_a_project_log_route_entry(
            Arc::clone(self.log_route()),
            operation,
        ))
    }

    /// Run `operation` to completion from synchronous code, whichever tokio
    /// runtime the engine runs on, with this stream's log route entered.
    fn block_on_the_engines_tokio_runtime<T: Send + 'static>(
        &self,
        operation: impl std::future::Future<Output = Result<T>> + Send + 'static,
    ) -> Result<T> {
        let operation = self.in_this_streams_log_route(operation);
        match self.tokio_runtime_variant() {
            TokioRuntimeVariant::TokioRuntimeTheRunnerOwns(handle) => handle.block_on(operation),
            TokioRuntimeVariant::ExternalTokioHandle(handle) => {
                let (tx, rx) = std::sync::mpsc::channel();
                handle.spawn(async move {
                    let _ = tx.send(operation.await);
                });
                rx.recv()
                    .map_err(|_| Error::Runtime("Task channel closed".into()))?
            }
        }
    }

    pub fn add_node_reporting_its_name(&self, spec: NodeSpec) -> Result<NodeInTheGraph> {
        self.block_on_the_engines_tokio_runtime(add_node_impl(
            self.live_graph_change(),
            Arc::clone(&self.node_interpreter_launch_record),
            self.lend_directory(),
            spec,
            ConfigReferencesOfTheAddedNode::CheckedAgainstTheGraphItJoins,
        ))
    }

    /// Add one node of a graph load whose references were validated with the
    /// whole graph, so a reference may name a node the load adds later.
    pub(crate) fn add_node_of_a_validated_graph_load(
        &self,
        spec: NodeSpec,
    ) -> Result<NodeInTheGraph> {
        self.block_on_the_engines_tokio_runtime(add_node_impl(
            self.live_graph_change(),
            Arc::clone(&self.node_interpreter_launch_record),
            self.lend_directory(),
            spec,
            ConfigReferencesOfTheAddedNode::ValidatedWithTheWholeGraphBeforeTheAdd,
        ))
    }

    /// Connect one link of a saved graph `from` to `to` as a load makes it: one whose pin types
    /// or Image formats alone refuse it is kept when its ports' families take it, and one an
    /// island cannot take is left to the commit that builds the stream to refuse.
    pub(crate) fn connect_a_link_of_a_saved_graph(
        &self,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
    ) -> Result<LinkUniqueId> {
        self.block_on_the_engines_tokio_runtime(connect_impl(
            self.live_graph_change(),
            from,
            to,
            ALinkConnected::OfASavedGraph,
        ))
    }

    /// Bring this stream's live graph to the graph `graph_document` holds, from synchronous
    /// code, as [`Self::reconcile_the_live_graph_to_the_graph_document`] does.
    pub(super) fn reconcile_the_live_graph_to_the_graph_document_from_synchronous_code(
        self: &Arc<Self>,
        graph_document: serde_json::Value,
        links_it_makes: LinksAReconcileMakes,
    ) -> Result<()> {
        let stream = Arc::clone(self);
        self.block_on_the_engines_tokio_runtime(async move {
            stream
                .reconcile_the_live_graph_to_the_graph_document(graph_document, links_it_makes)
                .await
        })
    }
}

// =============================================================================
// RuntimeOperations Implementation
// =============================================================================

impl RuntimeOperations for LoadedStreamInThisRuntime {
    fn add_node_async(&self, spec: NodeSpec) -> BoxFuture<'_, Result<NodeInTheGraph>> {
        self.in_this_streams_log_route(add_node_impl(
            self.live_graph_change(),
            Arc::clone(&self.node_interpreter_launch_record),
            self.lend_directory(),
            spec,
            ConfigReferencesOfTheAddedNode::CheckedAgainstTheGraphItJoins,
        ))
    }

    fn the_node_named(&self, node_name: &str) -> Result<NodeInTheGraph> {
        the_node_this_stream_names(&self.compiler, self.stream_id(), node_name)
    }

    fn remove_node_async(&self, node_run_id: NodeRunId) -> BoxFuture<'_, Result<()>> {
        if let Err(refusal) = self.refuse_a_node_change_a_drawn_link_names(
            &node_run_id,
            NodeChangeADrawnLinkBlocks::Removal,
        ) {
            return Box::pin(std::future::ready(Err(refusal)));
        }
        self.in_this_streams_log_route(remove_node_impl(self.live_graph_change(), node_run_id))
    }

    fn connect_async(
        &self,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
    ) -> BoxFuture<'_, Result<LinkUniqueId>> {
        self.in_this_streams_log_route(connect_impl(
            self.live_graph_change(),
            from,
            to,
            ALinkConnected::MadeNow,
        ))
    }

    fn disconnect_async(&self, link_id: LinkUniqueId) -> BoxFuture<'_, Result<()>> {
        self.in_this_streams_log_route(disconnect_impl(self.live_graph_change(), link_id))
    }

    fn apply_a_stream_graph_operation_live(
        &self,
        graph_operation: streamlib_project_repository::StreamGraphOperation,
    ) -> BoxFuture<'_, Result<streamlib_project_repository::StreamGraphOperation>> {
        self.in_this_streams_log_route(
            self.apply_a_stream_graph_operation_to_the_live_graph(graph_operation),
        )
    }

    fn hold_an_edit_its_document_holds_not_live(
        &self,
        graph_operation: &streamlib_project_repository::StreamGraphOperation,
        refusal: &str,
    ) {
        LoadedStreamInThisRuntime::hold_an_edit_its_document_holds_not_live(
            self,
            graph_operation,
            refusal,
        );
    }

    fn what_a_live_connect_makes_of_the_link(
        &self,
        link: &streamlib_project_repository::StreamGraphDocumentLink,
    ) -> Result<WhatALiveConnectMakesOfALink> {
        self.what_a_connect_makes_of_the_link(link)
    }

    fn the_name_a_node_added_with_none_takes(
        &self,
        node_type: &str,
        names_at_the_level: &std::collections::HashSet<String>,
    ) -> Result<String> {
        self.the_name_a_node_of_the_type_takes_at_a_level(node_type, names_at_the_level)
    }

    fn the_ports_a_node_type_declares(
        &self,
        node_type: &str,
    ) -> Option<crate::core::GraphNodePorts> {
        self.node_types_this_stream_resolves()
            .port_info(&NodeClassImportPath::new(node_type).ok()?)
    }

    fn the_config_a_node_holding_the_input_values_takes(
        &self,
        node_type: &str,
        input_values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let node_type = NodeClassImportPath::new(node_type)
            .map_err(|refusal| Error::GraphError(format!("node type `{node_type}`: {refusal}")))?;
        Ok(
            crate::core::graph::legacy_config_of_a_node::config_of_a_node(
                self.node_types_this_stream_resolves(),
                &node_type,
                input_values,
            ),
        )
    }

    fn refuse_an_input_value_its_input_does_not_take(
        &self,
        node_type: &NodeClassImportPath,
        node_name: &str,
        input_values_after_the_set: &serde_json::Map<String, serde_json::Value>,
        input: &str,
        value: Option<&serde_json::Value>,
    ) -> std::result::Result<(), crate::core::graph::input_values::InputValueRefusal> {
        self.refuse_a_saved_value_on_the_input_of_an_output_a_panel_writes(
            node_type, node_name, input, value,
        )?;
        let node = crate::core::graph::input_values::NodeAnInputValueIsSetOn {
            name: node_name,
            node_type,
            input_values_after_the_set,
        };
        crate::core::graph::input_values::refuse_an_input_value_its_input_does_not_take(
            self.node_types_this_stream_resolves(),
            &node,
            input,
            value,
        )
    }

    fn refuse_a_saved_value_on_the_input_of_an_output_a_panel_writes(
        &self,
        node_type: &NodeClassImportPath,
        node_name: &str,
        input: &str,
        value: Option<&serde_json::Value>,
    ) -> std::result::Result<(), crate::core::graph::input_values::InputValueRefusal> {
        crate::core::nodes::refuse_a_saved_value_on_the_input_of_an_output_a_panel_writes(
            self.node_types_this_stream_resolves(),
            node_type,
            node_name,
            input,
            value,
        )
    }

    fn node_type_takes_a_config(&self, node_type: &NodeClassImportPath) -> bool {
        crate::core::graph::input_values::node_type_takes_a_config(
            self.node_types_this_stream_resolves(),
            node_type,
        )
    }

    fn set_input_values_live(
        &self,
        sets: &[InputValueToSetLive],
    ) -> std::result::Result<InputValuesSetLive, LiveInputValueSetRefusal> {
        LoadedStreamInThisRuntime::set_input_values_live(self, sets)
    }

    fn an_exposure_of_config_fields_is_recorded_on(
        &self,
        node_type: &NodeClassImportPath,
        exposed_config_fields: &[String],
    ) -> Result<bool> {
        crate::core::graph::input_values::an_exposure_of_config_fields_is_recorded_on(
            self.node_types_this_stream_resolves(),
            node_type,
            exposed_config_fields,
        )
    }

    fn refuse_a_node_a_live_add_would_refuse(
        &self,
        node_type: &str,
        node_name: &str,
        input_values: &serde_json::Map<String, serde_json::Value>,
    ) -> BoxFuture<'_, Result<()>> {
        let launch_record = Arc::clone(&self.node_interpreter_launch_record);
        let lend_directory = self.lend_directory();
        let node_type = NodeClassImportPath::new(node_type)
            .map_err(|refusal| Error::GraphError(format!("node type `{node_type}`: {refusal}")));
        let node_name = node_name.to_owned();
        let input_values = input_values.clone();
        self.in_this_streams_log_route(async move {
            let node_type = node_type?;
            if launch_record.a_live_add_must_describe(&node_type) {
                let launch_record = Arc::clone(&launch_record);
                let described_type = node_type.clone();
                tokio::task::spawn_blocking(carrying_this_threads_project_log_route_entry(
                    move || {
                        launch_record.describe_and_register_a_type_a_live_add_names(
                            &described_type,
                            lend_directory.as_deref(),
                        )
                    },
                ))
                .await
                .map_err(|join_failure| {
                    Error::Runtime(format!(
                        "the describe of a node type did not finish: {join_failure}"
                    ))
                })??;
            }
            let node_types = launch_record.node_types_this_stream_resolves();
            if node_types.port_info(&node_type).is_none() {
                return Err(Error::UnknownNodeType { ident: node_type });
            }
            let config = crate::core::graph::legacy_config_of_a_node::config_of_a_node(
                node_types,
                &node_type,
                &input_values,
            );
            node_types.refuse_a_node_this_stream_cannot_add(&node_name, &node_type, &config)?;
            crate::core::graph::refuse_a_config_its_node_types_config_schema_refuses(
                &node_name, &node_type, node_types, &config,
            )
        })
    }

    fn reconcile_each_stream_fragment_to_the_stream_it_references_live(
        &self,
    ) -> BoxFuture<'_, Result<()>> {
        self.in_this_streams_log_route(
            self.reconcile_each_stream_fragment_to_the_stream_it_references(),
        )
    }

    fn to_json_async(&self) -> BoxFuture<'_, Result<serde_json::Value>> {
        Box::pin(async move { LoadedStreamInThisRuntime::to_json(self) })
    }

    fn open_a_viewer_of_an_output_port(
        &self,
        node_id: String,
        port_name: String,
        process_id_of_the_viewer: Option<u32>,
    ) -> BoxFuture<'_, Result<crate::core::runtime::OutputPortViewerHeldOpen>> {
        self.in_this_streams_log_route(async move {
            self.open_a_viewer_of_an_output_port_holding_its_channel(
                &node_id,
                &port_name,
                process_id_of_the_viewer,
            )
            .await
        })
    }

    fn the_running_view_node_a_panel_writes(
        &self,
        node_id: String,
        port_name: String,
        value: serde_json::Value,
    ) -> BoxFuture<'_, Result<uuid::Uuid>> {
        self.in_this_streams_log_route(
            self.read_the_running_view_node_a_panel_writes(node_id, port_name, value),
        )
    }

    fn the_top_level_graph_input_declaring(
        &self,
        input_name: String,
    ) -> BoxFuture<'_, Result<crate::core::runtime::TopLevelGraphInputDeclaringAStreamInput>> {
        self.in_this_streams_log_route(self.read_the_top_level_graph_input_declaring(input_name))
    }

    fn add_node(&self, spec: NodeSpec) -> Result<NodeRunId> {
        self.add_node_reporting_its_name(spec)
            .map(|added| added.node_run_id)
    }

    fn remove_node(&self, node_run_id: &NodeRunId) -> Result<()> {
        self.refuse_a_node_change_a_drawn_link_names(
            node_run_id,
            NodeChangeADrawnLinkBlocks::Removal,
        )?;
        self.block_on_the_engines_tokio_runtime(remove_node_impl(
            self.live_graph_change(),
            node_run_id.clone(),
        ))
    }

    fn connect(&self, from: OutputLinkPortRef, to: InputLinkPortRef) -> Result<LinkUniqueId> {
        self.block_on_the_engines_tokio_runtime(connect_impl(
            self.live_graph_change(),
            from,
            to,
            ALinkConnected::MadeNow,
        ))
    }

    fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()> {
        self.block_on_the_engines_tokio_runtime(disconnect_impl(
            self.live_graph_change(),
            link_id.clone(),
        ))
    }

    fn request_this_streams_shutdown(&self, reason: &str) -> Result<()> {
        self.ask_for_this_streams_shutdown(reason);
        Ok(())
    }

    fn request_a_function_node_promotion_out_of_the_islands_of_the_reader_it_crashed(
        &self,
        function_node_id: &NodeRunId,
        reader_id: &NodeRunId,
        promotion_request_number: u64,
        reason: &str,
    ) -> Result<()> {
        self.compiler.scope(|_graph, tx| {
            tx.log(
                PendingOperation::PromoteAFunctionNodeOutOfTheIslandsOfTheReaderItCrashed {
                    function_node_id: function_node_id.clone(),
                    reader_id: reader_id.clone(),
                    promotion_request_number,
                    reason: reason.to_string(),
                },
            );
        });
        self.loaded_stream_identity()
            .publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        Ok(())
    }

    fn to_json(&self) -> Result<serde_json::Value> {
        LoadedStreamInThisRuntime::to_json(self)
    }
}

#[cfg(test)]
mod inline_commit_thread_guard_tests;

#[cfg(test)]
mod connect_wires_a_matching_link_in_silence_tests;

#[cfg(test)]
mod connect_refuses_ports_whose_families_do_not_match_tests;

#[cfg(test)]
mod connect_judges_two_typed_pins_by_their_verdict_tests;

#[cfg(test)]
mod connect_refuses_an_island_into_the_app_process_tests;
