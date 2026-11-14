// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashSet;
use std::sync::Arc;

use serde::Serialize;
use std::path::{Path, PathBuf};

use super::LoadedStreamInThisRuntime;
use super::RuntimeStatus;
use super::operations::{BoxFuture, NodeInTheGraph, RuntimeOperations};
use super::processor_interpreter_launch_record::ProcessorInterpreterLaunchRecordOfOneStream;
use super::runtime::TokioRuntimeVariant;
use crate::core::RuntimeContext;
use crate::core::compiler::{Compiler, CompilerTransactionHandle, PendingOperation};
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, LinkUniqueId,
    PendingDeletionComponent, ProcessorInstanceComponent, ProcessorUniqueId, StateComponent,
    node_named_or_refused, refuse_a_link_between_ports_whose_families_do_not_match,
};
use crate::core::graph_snapshot::NodeChangeADrawnLinkBlocks;
use crate::core::logging::{
    carrying_this_threads_project_log_route_entry, polled_in_a_project_log_route_entry,
};
use crate::core::processors::{ProcessorSpec, ProcessorState};
use crate::core::pubsub::{LoadedStreamIdentity, RuntimeEvent};
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, PortDirection, Result};
use crate::iceoryx2::ChannelName;

// =============================================================================
// Core Implementation Functions ('static async fns for spawn compatibility)
// =============================================================================

/// The context a graph mutation compiles against the moment it is logged —
/// `Some` once the runtime is started, `None` while the graph is still being
/// built ahead of `start()`, which commits the whole batch itself, and `None`
/// on a processor's own execution thread, which never waits for a compile.
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
    /// Set on a processor's execution thread. A mutation issued from one must
    /// not wait for the compile: removing that very processor holds the commit
    /// gate while it joins the thread, and the wait would never end.
    static IS_A_PROCESSOR_EXECUTION_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Mark the calling thread as one that runs a processor, so a graph mutation it
/// issues is left to the graph-change listener rather than compiled inline.
pub(crate) fn mark_this_thread_as_a_processor_execution_thread() {
    IS_A_PROCESSOR_EXECUTION_THREAD.with(|marker| marker.set(true));
}

fn this_thread_may_commit_inline() -> bool {
    !IS_A_PROCESSOR_EXECUTION_THREAD.with(|marker| marker.get())
}

/// Compile the operations a mutation just logged, so the caller learns whether
/// its change took. A batch that fails to spawn or wire is otherwise discarded
/// with a log line as its only trace, after the caller was already told the
/// change was accepted. The compile blocks — it waits for a spawned processor
/// to report ready — so it runs off the async worker.
async fn commit_live_graph_change(compiler: &Arc<Compiler>, live: LiveCommitContext) -> Result<()> {
    let Some(runtime_ctx) = live else {
        take_out_now_each_node_and_link_pending_deletion_no_commit_builds(compiler);
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
        let nodes_pending_deletion: HashSet<ProcessorUniqueId> = this_streams_graph
            .traversal()
            .v(())
            .iter()
            .filter(|node| {
                node.has::<PendingDeletionComponent>() && !node.has::<ProcessorInstanceComponent>()
            })
            .map(|node| node.id.clone())
            .collect();
        tx.retract_every_operation_naming(&nodes_pending_deletion, &links_pending_deletion);
        for link_id in &links_pending_deletion {
            this_streams_graph.traversal_mut().e(link_id).drop();
        }
        for processor_id in &nodes_pending_deletion {
            this_streams_graph.traversal_mut().v(processor_id).drop();
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

/// Core implementation for add_processor - takes owned Arcs for 'static lifetime.
///
/// Reports the node it became, id and name together. Both come out
/// of the one `compiler.scope` that added the node, so a caller that needs the
/// name never has to ask a second time — and never races a concurrent removal
/// into being told its own successful add does not exist.
pub(super) async fn add_processor_impl(
    live_graph_change: LiveGraphChangeOnOneStream,
    processor_interpreter_launch_record: Arc<ProcessorInterpreterLaunchRecordOfOneStream>,
    lend_directory: Option<PathBuf>,
    spec: ProcessorSpec,
    config_references_of_the_added_node: ConfigReferencesOfTheAddedNode,
) -> Result<NodeInTheGraph> {
    let LiveGraphChangeOnOneStream {
        compiler,
        live,
        stream,
    } = live_graph_change;
    let emit_will_add = |id: &ProcessorUniqueId| {
        stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeWillAddProcessor {
            processor_id: id.clone(),
        });
    };

    let emit_did_add = |id: &ProcessorUniqueId| {
        stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeDidAddProcessor {
            processor_id: id.clone(),
        });
    };

    // A type this stream does not resolve yet is described in the stream's
    // own interpreter, off the async worker: a describe runs for as long as
    // the module's import does.
    let a_load_holds_its_type_as_a_placeholder = config_references_of_the_added_node
        == ConfigReferencesOfTheAddedNode::ValidatedWithTheWholeGraphBeforeTheAdd
        && processor_interpreter_launch_record
            .node_types_this_stream_resolves()
            .placeholder_of_the_node_type_not_described(&spec.name)
            .is_some();
    if !a_load_holds_its_type_as_a_placeholder
        && processor_interpreter_launch_record.a_live_add_must_describe(&spec.name)
    {
        let node_type = spec.name.clone();
        let launch_record = Arc::clone(&processor_interpreter_launch_record);
        tokio::task::spawn_blocking(carrying_this_threads_project_log_route_entry(move || {
            launch_record.describe_and_register_a_type_a_live_add_names(
                &node_type,
                lend_directory.as_deref(),
            )
        }))
        .await
        .map_err(|join_failure| {
            Error::Runtime(format!(
                "the describe of a node type did not finish: {join_failure}"
            ))
        })??;
    }

    // Held so a typed `UnknownProcessorType` can name what was asked for —
    // `spec` is moved into `add_v`.
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
            .map(|state_component| state_component.current() == ProcessorState::Error)
            .unwrap_or(false);

        if registry_miss {
            emit_will_add(&node_id);
            emit_did_add(&node_id);
            if a_load_holds_its_type_as_a_placeholder {
                return Ok(NodeInTheGraph {
                    processor_id: node_id,
                    name: assigned_display_name,
                });
            }
            return Err(Error::UnknownProcessorType {
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
                        node.processor_type.clone(),
                        node.config.clone().unwrap_or_default(),
                    )
                })
            else {
                return Err(Error::GraphError("Could not create node".into()));
            };
            let node_types = processor_interpreter_launch_record.node_types_this_stream_resolves();
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
                graph.traversal_mut().v(&node_id).drop();
                return Err(refusal);
            }
        }

        emit_will_add(&node_id);
        tx.log(PendingOperation::AddProcessor(node_id.clone()));
        emit_did_add(&node_id);
        Ok(NodeInTheGraph {
            processor_id: node_id,
            name: assigned_display_name,
        })
    })?;

    commit_live_graph_change(&compiler, live).await?;

    stream.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);

    Ok(added)
}

/// Core implementation for remove_processor - takes owned Arcs for 'static lifetime.
/// Mark `processor_id` and every link on it pending deletion, each link logged as a link's
/// removal ahead of the node's, so the compile reclaims both endpoints' ports through the link
/// path; dropping the node alone would cascade its edges out of the graph with the peers'
/// publishers, subscribers and notifiers still held against their channels.
fn mark_the_node_and_each_of_its_links_pending_deletion(
    graph: &mut Graph,
    tx: &CompilerTransactionHandle,
    processor_id: &ProcessorUniqueId,
) -> Result<()> {
    if !graph.traversal().v(processor_id).exists() {
        return Err(Error::ProcessorNotFound(processor_id.to_string()));
    }
    let inbound_links: Vec<LinkUniqueId> = graph
        .traversal_mut()
        .v(processor_id)
        .in_e()
        .iter()
        .map(|link| link.id.clone())
        .collect();
    let outbound_links: Vec<LinkUniqueId> = graph
        .traversal_mut()
        .v(processor_id)
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
    if let Some(node) = graph.traversal_mut().v(processor_id).first_mut() {
        node.insert(PendingDeletionComponent);
    }
    tx.log(PendingOperation::RemoveProcessor(processor_id.clone()));
    Ok(())
}

pub(super) async fn remove_processor_impl(
    live_graph_change: LiveGraphChangeOnOneStream,
    processor_id: ProcessorUniqueId,
) -> Result<()> {
    let LiveGraphChangeOnOneStream {
        compiler,
        live,
        stream,
    } = live_graph_change;
    compiler.scope(|graph, tx| {
        mark_the_node_and_each_of_its_links_pending_deletion(graph, tx, &processor_id)
    })?;

    let removal_outcome = commit_live_graph_change(&compiler, live).await;
    // A removal that abandoned the processor's thread still took the processor
    // out of the graph, so it is announced before the call fails naming it.
    if removal_outcome.is_err()
        && compiler.scope(|graph, _tx| graph.traversal().v(&processor_id).exists())
    {
        return removal_outcome;
    }

    stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeWillRemoveProcessor {
        processor_id: processor_id.clone(),
    });
    stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeDidRemoveProcessor {
        processor_id: processor_id.clone(),
    });
    stream.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);

    removal_outcome
}

/// Core implementation for connect - takes owned Arcs for 'static lifetime.
///
/// Connect compares the two ports' families, names, codec and formats and
/// refuses a mismatch by name before any channel exists.
#[tracing::instrument(
    name = "stream.connect",
    skip(live_graph_change),
    fields(from = %from, to = %to),
)]
pub(super) async fn connect_impl(
    live_graph_change: LiveGraphChangeOnOneStream,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<LinkUniqueId> {
    let LiveGraphChangeOnOneStream {
        compiler,
        live,
        stream,
    } = live_graph_change;
    let from = from.with_its_port_name_cast()?;
    let to = to.with_its_port_name_cast()?;

    stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeWillConnect {
        from: from.clone(),
        to: to.clone(),
    });

    let link_id = apply_a_link(&compiler, from.clone(), to.clone())?;

    commit_live_graph_change(&compiler, live).await?;

    stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeDidConnect {
        link_id: link_id.to_string(),
        from,
        to,
    });
    stream.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);

    Ok(link_id)
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
            processor_id: named.id.clone(),
            name: named.display_name.clone(),
        })
    })
}

/// Add the link from `from` to `to` to the graph and queue it for wiring.
fn apply_a_link(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<LinkUniqueId> {
    let (link_id, channel) =
        compiler.scope(|graph, tx| -> Result<(LinkUniqueId, ChannelName)> {
            // Validate endpoints + ports FIRST — before the channel-name
            // derivation — so a missing processor/port reads as the typed
            // ProcessorNotFound / ProcessorPortNotFound and never gets masked by an
            // InvalidLink from the wire-name grammar. The `add_e` call still checks
            // defensively; this pre-validation is what gets the typed error out.
            refuse_a_source_this_graph_cannot_take(graph, &from)?;
            refuse_a_destination_this_graph_cannot_take(graph, &to)?;
            refuse_a_link_between_ports_whose_families_do_not_match(graph, &from, &to)?;

            // The one channel this link's source output port publishes to — keyed
            // on the SOURCE only (`{src_processor}/{src_output}`), so every link
            // from this output port shares one channel / one publisher / N
            // subscribers (D1, #1419). Endpoints are validated above, so a grammar
            // failure here is a genuinely-illegal source PORT name (author error),
            // surfaced as InvalidLink. The processor id is lowercased inside
            // `source_channel_name`; underscore is legal and rides through. Deriving
            // inside the transaction means an illegal port name rolls the pending
            // link back rather than committing a half-built edge.
            let channel = crate::iceoryx2::source_channel_name(
                from.processor_id().as_str(),
                from.port_name(),
            )
            .map_err(|source| Error::InvalidLink(source.to_string()))?;

            let link_id = graph
                .traversal_mut()
                .add_e(from, to)
                .inspect(|link| tx.log(PendingOperation::AddLink(link.id.clone())))
                .first()
                .map(|link| link.id.clone())
                .ok_or_else(|| {
                    Error::GraphError("failed to create link after validation".into())
                })?;

            Ok((link_id, channel))
        })?;

    tracing::debug!(
        link_id = %link_id,
        channel = channel.as_str(),
        "connect assigned channel"
    );
    Ok(link_id)
}

/// Refuse a source this graph has no processor or no such output port for,
/// with the typed error a caller can act on.
fn refuse_a_source_this_graph_cannot_take(
    graph: &crate::core::graph::Graph,
    from: &OutputLinkPortRef,
) -> Result<()> {
    let from_node = graph
        .traversal()
        .v(from.processor_id())
        .first()
        .ok_or_else(|| Error::ProcessorNotFound(from.processor_id().to_string()))?;
    if !from_node.has_output(from.port_name()) {
        return Err(Error::ProcessorPortNotFound {
            processor_id: from.processor_id().to_string(),
            port_name: from.port_name().to_string(),
            direction: PortDirection::Output,
        });
    }
    Ok(())
}

/// Refuse a destination this graph has no processor or no such input port for,
/// with the typed error a caller can act on.
fn refuse_a_destination_this_graph_cannot_take(
    graph: &crate::core::graph::Graph,
    to: &InputLinkPortRef,
) -> Result<()> {
    let to_node = graph
        .traversal()
        .v(to.processor_id())
        .first()
        .ok_or_else(|| Error::ProcessorNotFound(to.processor_id().to_string()))?;
    if !to_node.has_input(to.port_name()) {
        return Err(Error::ProcessorPortNotFound {
            processor_id: to.processor_id().to_string(),
            port_name: to.port_name().to_string(),
            direction: PortDirection::Input,
        });
    }
    Ok(())
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
    /// The context a graph mutation compiles against right away, or `None`
    /// while the stream is still being built ahead of `start()` — and `None`
    /// from a processor's own execution thread, where the mutation is left to
    /// the graph-change listener.
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

    /// Update a processor's configuration and compile it right away when the stream runs, so
    /// a configuration the running processor refuses is refused here, and the node keeps the
    /// configuration it holds.
    pub(super) async fn update_processor_config_compiled_right_away<C: Serialize>(
        &self,
        processor_id: &ProcessorUniqueId,
        config: C,
    ) -> Result<()> {
        self.log_a_processor_config_update_for_the_next_commit(processor_id, config)?;
        commit_live_graph_change(&self.compiler, self.live_commit_context()).await?;
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
            .processor_interpreter_lend_directory
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

    /// Add a processor and report the node it became: its id and its name — the
    /// requested one cast, or the class's short name with any `-2` suffix.
    pub fn add_processor_reporting_its_name(&self, spec: ProcessorSpec) -> Result<NodeInTheGraph> {
        self.block_on_the_engines_tokio_runtime(add_processor_impl(
            self.live_graph_change(),
            Arc::clone(&self.processor_interpreter_launch_record),
            self.lend_directory(),
            spec,
            ConfigReferencesOfTheAddedNode::CheckedAgainstTheGraphItJoins,
        ))
    }

    /// Add one node of a graph load whose references were validated with the
    /// whole graph, so a reference may name a node the load adds later.
    pub(crate) fn add_processor_of_a_validated_graph_load(
        &self,
        spec: ProcessorSpec,
    ) -> Result<NodeInTheGraph> {
        self.block_on_the_engines_tokio_runtime(add_processor_impl(
            self.live_graph_change(),
            Arc::clone(&self.processor_interpreter_launch_record),
            self.lend_directory(),
            spec,
            ConfigReferencesOfTheAddedNode::ValidatedWithTheWholeGraphBeforeTheAdd,
        ))
    }

    /// Bring this stream's live graph to the graph `graph_document` holds, from synchronous
    /// code, as [`Self::reconcile_the_live_graph_to_the_graph_document`] does.
    pub(super) fn reconcile_the_live_graph_to_the_graph_document_from_synchronous_code(
        self: &Arc<Self>,
        graph_document: serde_json::Value,
    ) -> Result<()> {
        let stream = Arc::clone(self);
        self.block_on_the_engines_tokio_runtime(async move {
            stream
                .reconcile_the_live_graph_to_the_graph_document(graph_document)
                .await
        })
    }
}

// =============================================================================
// RuntimeOperations Implementation
// =============================================================================

impl RuntimeOperations for LoadedStreamInThisRuntime {
    fn add_processor_async(&self, spec: ProcessorSpec) -> BoxFuture<'_, Result<NodeInTheGraph>> {
        self.in_this_streams_log_route(add_processor_impl(
            self.live_graph_change(),
            Arc::clone(&self.processor_interpreter_launch_record),
            self.lend_directory(),
            spec,
            ConfigReferencesOfTheAddedNode::CheckedAgainstTheGraphItJoins,
        ))
    }

    fn the_node_named(&self, node_name: &str) -> Result<NodeInTheGraph> {
        the_node_this_stream_names(&self.compiler, self.stream_id(), node_name)
    }

    fn remove_processor_async(&self, processor_id: ProcessorUniqueId) -> BoxFuture<'_, Result<()>> {
        if let Err(refusal) = self.refuse_a_node_change_a_drawn_link_names(
            &processor_id,
            NodeChangeADrawnLinkBlocks::Removal,
        ) {
            return Box::pin(std::future::ready(Err(refusal)));
        }
        self.in_this_streams_log_route(remove_processor_impl(
            self.live_graph_change(),
            processor_id,
        ))
    }

    fn connect_async(
        &self,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
    ) -> BoxFuture<'_, Result<LinkUniqueId>> {
        self.in_this_streams_log_route(connect_impl(self.live_graph_change(), from, to))
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

    fn the_name_a_node_added_with_none_takes(
        &self,
        node_type: &str,
        names_at_the_level: &std::collections::HashSet<String>,
    ) -> Result<String> {
        self.the_name_a_node_of_the_type_takes_at_a_level(node_type, names_at_the_level)
    }

    fn refuse_a_node_a_live_add_would_refuse(
        &self,
        node_type: &str,
        node_name: &str,
        config: &serde_json::Value,
    ) -> BoxFuture<'_, Result<()>> {
        let launch_record = Arc::clone(&self.processor_interpreter_launch_record);
        let lend_directory = self.lend_directory();
        let node_type = ProcessorClassImportPath::new(node_type)
            .map_err(|refusal| Error::GraphError(format!("node type `{node_type}`: {refusal}")));
        let node_name = node_name.to_owned();
        let config = config.clone();
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
                return Err(Error::UnknownProcessorType { ident: node_type });
            }
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

    fn write_a_value_onto_an_output_of_a_view_node(
        &self,
        node_id: String,
        port_name: String,
        value: serde_json::Value,
    ) -> BoxFuture<'_, Result<()>> {
        self.in_this_streams_log_route(
            self.hand_a_value_written_by_a_panel_to_a_view_node(node_id, port_name, value),
        )
    }

    fn write_a_value_to_a_top_level_graph_input(
        &self,
        input_name: String,
        value: serde_json::Value,
    ) -> BoxFuture<'_, Result<crate::core::descriptors::PortFamily>> {
        self.in_this_streams_log_route(
            self.hand_a_value_written_to_a_top_level_graph_input(input_name, value),
        )
    }

    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
        self.add_processor_reporting_its_name(spec)
            .map(|added| added.processor_id)
    }

    fn remove_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        self.refuse_a_node_change_a_drawn_link_names(
            processor_id,
            NodeChangeADrawnLinkBlocks::Removal,
        )?;
        self.block_on_the_engines_tokio_runtime(remove_processor_impl(
            self.live_graph_change(),
            processor_id.clone(),
        ))
    }

    fn connect(&self, from: OutputLinkPortRef, to: InputLinkPortRef) -> Result<LinkUniqueId> {
        self.block_on_the_engines_tokio_runtime(connect_impl(self.live_graph_change(), from, to))
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
        function_node_id: &ProcessorUniqueId,
        reader_id: &ProcessorUniqueId,
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
