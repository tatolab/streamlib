// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use super::Runner;
use super::RuntimeStatus;
use super::operations::{BoxFuture, NodeInTheGraph, RuntimeOperations};
use super::runtime::TokioRuntimeVariant;
use super::surface_image_exchange::exchange_published_surface_id_for_png_image_bytes;
use crate::core::RuntimeContext;
use crate::core::compiler::{Compiler, PendingOperation};
use crate::core::graph::{
    GraphEdgeWithComponents, GraphNodeWithComponents, LinkUniqueId, PendingDeletionComponent,
    ProcessorUniqueId, StateComponent, node_names_listed_for_a_refusal,
};
use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec, ProcessorState};
use crate::core::pubsub::{Event, PUBSUB, RuntimeEvent, topics};
use crate::core::runtime::ExchangedPublishedSurfaceFramePngImage;
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, PortDirection, Result};
use crate::iceoryx2::ChannelName;
use tracing::Instrument as _;

// =============================================================================
// Core Implementation Functions ('static async fns for spawn compatibility)
// =============================================================================

/// The context a graph mutation compiles against the moment it is logged —
/// `Some` once the runtime is started, `None` while the graph is still being
/// built ahead of `start()`, which commits the whole batch itself, and `None`
/// on a processor's own execution thread, which never waits for a compile.
type LiveCommitContext = Option<Arc<RuntimeContext>>;

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
        return Ok(());
    };
    let compiler = Arc::clone(compiler);
    tokio::task::spawn_blocking(move || compiler.commit(&runtime_ctx))
        .await
        .map_err(|join_failure| {
            Error::Runtime(format!(
                "the graph change's compile task did not finish: {join_failure}"
            ))
        })?
}

/// Core implementation for add_processor - takes owned Arcs for 'static lifetime.
///
/// Reports the node it became, id and name together. Both come out
/// of the one `compiler.scope` that added the node, so a caller that needs the
/// name never has to ask a second time — and never races a concurrent removal
/// into being told its own successful add does not exist.
async fn add_processor_impl(
    compiler: Arc<Compiler>,
    live: LiveCommitContext,
    spec: ProcessorSpec,
) -> Result<NodeInTheGraph> {
    let emit_will_add = |id: &ProcessorUniqueId| {
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillAddProcessor {
                processor_id: id.clone(),
            }),
        );
    };

    let emit_did_add = |id: &ProcessorUniqueId| {
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidAddProcessor {
                processor_id: id.clone(),
            }),
        );
    };

    // A type nobody registered may still be resolvable by name — the wheel
    // resolves a Python class import path the way `rt.add` would. A resolver
    // that fails names why; one that is absent leaves the registry miss below
    // to say the type is unknown.
    PROCESSOR_REGISTRY.resolve_processor_type_if_unregistered(&spec.name)?;

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
            return Err(Error::UnknownProcessorType {
                ident: ident_for_err,
            });
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

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
    );

    Ok(added)
}

/// Core implementation for remove_processor - takes owned Arcs for 'static lifetime.
async fn remove_processor_impl(
    compiler: Arc<Compiler>,
    live: LiveCommitContext,
    processor_id: ProcessorUniqueId,
) -> Result<()> {
    compiler.scope(|graph, tx| {
        if !graph.traversal().v(&processor_id).exists() {
            return Err(Error::ProcessorNotFound(processor_id.to_string()));
        }

        // Every link on the node is removed as a link, which reclaims both
        // endpoints' ports. Dropping the node alone cascades its edges out of
        // the graph with the peers' publishers, subscribers and notifiers
        // still held against their channels.
        let inbound_links: Vec<LinkUniqueId> = graph
            .traversal_mut()
            .v(&processor_id)
            .in_e()
            .iter()
            .map(|link| link.id.clone())
            .collect();
        let outbound_links: Vec<LinkUniqueId> = graph
            .traversal_mut()
            .v(&processor_id)
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

        if let Some(node) = graph.traversal_mut().v(&processor_id).first_mut() {
            node.insert(PendingDeletionComponent);
        }

        tx.log(PendingOperation::RemoveProcessor(processor_id.clone()));

        Ok(())
    })?;

    let removal_outcome = commit_live_graph_change(&compiler, live).await;
    // A removal that abandoned the processor's thread still took the processor
    // out of the graph, so it is announced before the call fails naming it.
    if removal_outcome.is_err()
        && compiler.scope(|graph, _tx| graph.traversal().v(&processor_id).exists())
    {
        return removal_outcome;
    }

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillRemoveProcessor {
            processor_id: processor_id.clone(),
        }),
    );

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRemoveProcessor {
            processor_id: processor_id.clone(),
        }),
    );

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
    );

    removal_outcome
}

/// Core implementation for connect - takes owned Arcs for 'static lifetime.
///
/// A link is pure plumbing. Connect inspects no type, compares no type, and
/// never warns — a mismatch surfaces as a decode failure at the consuming
/// processor's read.
#[tracing::instrument(
    name = "runtime.connect",
    skip(compiler, live),
    fields(from = %from, to = %to),
)]
async fn connect_impl(
    compiler: Arc<Compiler>,
    live: LiveCommitContext,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<LinkUniqueId> {
    let from = from.with_its_port_name_cast()?;
    let to = to.with_its_port_name_cast()?;

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillConnect {
            from: from.clone(),
            to: to.clone(),
        }),
    );

    let link_id = apply_a_link(&compiler, from.clone(), to.clone())?;

    commit_live_graph_change(&compiler, live).await?;

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidConnect {
            link_id: link_id.to_string(),
            from,
            to,
        }),
    );

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
    );

    Ok(link_id)
}

/// The node `node_name` names on this runtime once cast.
///
/// Refused by name when this runtime holds no node by that name, listing the
/// ones it does.
fn the_node_this_runtime_names(
    compiler: &Arc<Compiler>,
    node_name: &str,
) -> Result<NodeInTheGraph> {
    compiler.scope(|graph, _tx| {
        if let Some(named) = graph.traversal().v_with_node_name(node_name).first() {
            return Ok(NodeInTheGraph {
                processor_id: named.id.clone(),
                name: named.display_name.clone(),
            });
        }
        let every_node = graph.traversal().v(());
        Err(Error::ProcessorNotFound(format!(
            "no node on this runtime is named {node_name:?}. This runtime holds: {}",
            node_names_listed_for_a_refusal(
                every_node.iter().map(|node| node.display_name.as_str())
            )
        )))
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
            // Validate source processor + output port.
            {
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
            }
            refuse_a_destination_this_graph_cannot_take(graph, &to)?;

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
async fn disconnect_impl(
    compiler: Arc<Compiler>,
    live: LiveCommitContext,
    link_id: LinkUniqueId,
) -> Result<()> {
    let link_info = compiler.scope(|graph, tx| {
        let (from_value, to_value) = graph
            .traversal()
            .e(&link_id)
            .first()
            .map(|l| (l.from_port(), l.to_port()))
            .ok_or_else(|| Error::NotFound(format!("Link '{}' not found", link_id)))?;

        let info = (from_value.clone(), to_value.clone());

        if let Some(link) = graph.traversal_mut().e(&link_id).first_mut() {
            link.insert(PendingDeletionComponent);
        }

        tx.log(PendingOperation::RemoveLink(link_id.clone()));

        Ok::<_, Error>(info)
    })?;

    commit_live_graph_change(&compiler, live).await?;

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillDisconnect {
            link_id: link_id.to_string(),
            from_port: link_info.0.to_string(),
            to_port: link_info.1.to_string(),
        }),
    );

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidDisconnect {
            link_id: link_id.to_string(),
            from_port: link_info.0.to_string(),
            to_port: link_info.1.to_string(),
        }),
    );

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
    );

    Ok(())
}

impl Runner {
    /// The context a graph mutation compiles against right away, or `None`
    /// while the graph is still being built ahead of `start()` — and `None`
    /// from a processor's own execution thread, where the mutation is left to
    /// the graph-change listener as every mutation was before inline commits.
    fn live_commit_context(&self) -> LiveCommitContext {
        if !this_thread_may_commit_inline() {
            return None;
        }
        if *self.status.lock() != RuntimeStatus::Started {
            return None;
        }
        self.runtime_context.lock().clone()
    }

    /// Add a processor and report the node it became: its id and its name — the
    /// requested one cast, or the class's short name with any `-2` suffix.
    pub fn add_processor_reporting_its_name(&self, spec: ProcessorSpec) -> Result<NodeInTheGraph> {
        let live = self.live_commit_context();
        match &self.tokio_runtime_variant {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => {
                let compiler = Arc::clone(&self.compiler);
                rt.block_on(add_processor_impl(compiler, live, spec))
            }
            TokioRuntimeVariant::ExternalTokioHandle(handle) => {
                let compiler = Arc::clone(&self.compiler);
                let (tx, rx) = std::sync::mpsc::channel();
                handle.spawn(async move {
                    let result = add_processor_impl(compiler, live, spec).await;
                    let _ = tx.send(result);
                });
                rx.recv()
                    .map_err(|_| Error::Runtime("Task channel closed".into()))?
            }
        }
    }
}

// =============================================================================
// RuntimeOperations Implementation
// =============================================================================

impl RuntimeOperations for Runner {
    // =========================================================================
    // Async Methods (delegate to _impl functions)
    // =========================================================================

    fn add_processor_async(&self, spec: ProcessorSpec) -> BoxFuture<'_, Result<NodeInTheGraph>> {
        let compiler = Arc::clone(&self.compiler);
        let live = self.live_commit_context();
        Box::pin(add_processor_impl(compiler, live, spec))
    }

    fn the_node_named(&self, node_name: &str) -> Result<NodeInTheGraph> {
        the_node_this_runtime_names(&self.compiler, node_name)
    }

    fn remove_processor_async(&self, processor_id: ProcessorUniqueId) -> BoxFuture<'_, Result<()>> {
        let compiler = Arc::clone(&self.compiler);
        let live = self.live_commit_context();
        Box::pin(remove_processor_impl(compiler, live, processor_id))
    }

    fn connect_async(
        &self,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
    ) -> BoxFuture<'_, Result<LinkUniqueId>> {
        let compiler = Arc::clone(&self.compiler);
        let live = self.live_commit_context();
        Box::pin(connect_impl(compiler, live, from, to))
    }

    fn disconnect_async(&self, link_id: LinkUniqueId) -> BoxFuture<'_, Result<()>> {
        let compiler = Arc::clone(&self.compiler);
        let live = self.live_commit_context();
        Box::pin(disconnect_impl(compiler, live, link_id))
    }

    fn to_json_async(&self) -> BoxFuture<'_, Result<serde_json::Value>> {
        Box::pin(async move { Runner::to_json(self) })
    }

    #[tracing::instrument(name = "runtime.tap", skip(self), fields(channel = %channel, count = ?count))]
    fn tap_async(
        &self,
        channel: String,
        count: Option<usize>,
    ) -> BoxFuture<'_, Result<crate::core::runtime::TapSubscription>> {
        // Resolve the address the caller named to the source that publishes to
        // it, and that source's iceoryx2 sizing, from the live graph BEFORE
        // spawning: the same derivation the compiler op used to open the
        // service, so the tap's publisher-free reopen requests identical,
        // iceoryx2-verified parameters. A channel name carries a source's
        // processor id, which is nothing a caller could be expected to spell.
        let resolved = self.compiler.scope(
            |graph, _tx| -> Result<(String, crate::iceoryx2::ChannelSizing)> {
                let source = crate::core::compiler::compiler_ops::find_the_source_a_caller_named(
                    graph,
                    self.this_runtimes_name_on_the_mesh(),
                    &channel,
                )?;
                let sizing = crate::core::compiler::compiler_ops::resolve_channel_sizing(
                    graph,
                    &self.iceoryx2_node,
                    &source,
                )?;
                let channel_service_name =
                    crate::core::compiler::compiler_ops::channel_service_name(&source)?;
                Ok((channel_service_name, sizing))
            },
        );

        let node = self.iceoryx2_node.clone();
        Box::pin(async move {
            let (channel, sizing) = resolved?;
            // The reserved-slot subscriber is `!Send` and lives on a dedicated
            // OS thread; `start_channel_tap` blocks briefly for its subscribe
            // outcome, so it runs on a blocking pool, off the async worker.
            tokio::task::spawn_blocking(move || {
                crate::core::runtime::tap::start_channel_tap(node, channel, sizing, count)
            })
            .await
            .map_err(|join_error| {
                Error::Runtime(format!(
                    "channel-tap start task failed to join: {join_error}"
                ))
            })?
        })
    }

    fn exchange_published_surface_id_for_png_image_bytes_async(
        &self,
        published_surface_id: String,
        downscale_long_edge_pixel_cap: Option<u32>,
    ) -> BoxFuture<'_, Result<ExchangedPublishedSurfaceFramePngImage>> {
        // Duration is the only interesting thing about this operation, so the
        // span is built here and entered by the async block rather than
        // attached to this fn: an instrumented `-> BoxFuture` fn opens and
        // closes its span while the future is *built*, covering none of the
        // copy, the join or the encode.
        let exchange_span = tracing::info_span!(
            "runtime.exchange",
            surface_id = %published_surface_id,
            downscale_long_edge_pixel_cap = ?downscale_long_edge_pixel_cap,
        );

        // Read the context BEFORE spawning: a node that has not started has
        // no pool to claim from and no device to convert on, and saying so
        // here names the actual state rather than failing inside a resolve.
        let gpu_context = self
            .runtime_context
            .lock()
            .as_ref()
            .map(|runtime_context| runtime_context.gpu.clone())
            .ok_or_else(|| {
                Error::Runtime(
                    "the runtime has no GPU context, so no surface can be exchanged for an \
                     image; start the runtime first"
                        .into(),
                )
            });

        Box::pin(
            async move {
                let gpu_context = gpu_context?;
                // The copy blocks on the GPU and the encode on the CPU; both
                // run off the async worker so a tap streaming on the same
                // control plane keeps its cadence.
                tokio::task::spawn_blocking(move || {
                    exchange_published_surface_id_for_png_image_bytes(
                        &gpu_context,
                        &published_surface_id,
                        downscale_long_edge_pixel_cap,
                    )
                })
                .await
                .map_err(|join_error| {
                    Error::Runtime(format!(
                        "surface-exchange task failed to join: {join_error}"
                    ))
                })?
            }
            .instrument(exchange_span),
        )
    }

    // =========================================================================
    // Sync Methods (variant-aware blocking strategy)
    // =========================================================================

    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
        self.add_processor_reporting_its_name(spec)
            .map(|added| added.processor_id)
    }

    fn remove_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        match &self.tokio_runtime_variant {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => {
                rt.block_on(self.remove_processor_async(processor_id.clone()))
            }
            TokioRuntimeVariant::ExternalTokioHandle(handle) => {
                let compiler = Arc::clone(&self.compiler);
                let live = self.live_commit_context();
                let processor_id = processor_id.clone();
                let (tx, rx) = std::sync::mpsc::channel();
                handle.spawn(async move {
                    let result = remove_processor_impl(compiler, live, processor_id).await;
                    let _ = tx.send(result);
                });
                rx.recv()
                    .map_err(|_| Error::Runtime("Task channel closed".into()))?
            }
        }
    }

    fn connect(&self, from: OutputLinkPortRef, to: InputLinkPortRef) -> Result<LinkUniqueId> {
        match &self.tokio_runtime_variant {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => rt.block_on(self.connect_async(from, to)),
            TokioRuntimeVariant::ExternalTokioHandle(handle) => {
                let compiler = Arc::clone(&self.compiler);
                let live = self.live_commit_context();
                let (tx, rx) = std::sync::mpsc::channel();
                handle.spawn(async move {
                    let result = connect_impl(compiler, live, from, to).await;
                    let _ = tx.send(result);
                });
                rx.recv()
                    .map_err(|_| Error::Runtime("Task channel closed".into()))?
            }
        }
    }

    fn this_runtimes_name_on_the_mesh(&self) -> &str {
        self.runtime_mesh.runtime_name()
    }

    fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()> {
        match &self.tokio_runtime_variant {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => {
                rt.block_on(self.disconnect_async(link_id.clone()))
            }
            TokioRuntimeVariant::ExternalTokioHandle(handle) => {
                let compiler = Arc::clone(&self.compiler);
                let live = self.live_commit_context();
                let link_id = link_id.clone();
                let (tx, rx) = std::sync::mpsc::channel();
                handle.spawn(async move {
                    let result = disconnect_impl(compiler, live, link_id).await;
                    let _ = tx.send(result);
                });
                rx.recv()
                    .map_err(|_| Error::Runtime("Task channel closed".into()))?
            }
        }
    }

    // =========================================================================
    // Lifecycle
    // =========================================================================

    fn request_runtime_shutdown(&self, reason: &str) -> Result<()> {
        crate::core::runtime::request_runtime_shutdown(reason)
    }

    // =========================================================================
    // Introspection
    // =========================================================================

    fn to_json(&self) -> Result<serde_json::Value> {
        Runner::to_json(self)
    }
}

#[cfg(test)]
mod inline_commit_thread_guard_tests {
    use super::*;

    /// The guard is per thread: a processor's thread opts out of inline
    /// commits and no other thread is affected by it.
    #[test]
    fn a_processor_execution_thread_never_commits_inline_and_other_threads_still_do() {
        assert!(this_thread_may_commit_inline());

        let from_a_processor_thread = std::thread::spawn(|| {
            mark_this_thread_as_a_processor_execution_thread();
            this_thread_may_commit_inline()
        })
        .join()
        .expect("the marked thread finishes");
        assert!(
            !from_a_processor_thread,
            "a processor's thread must not wait for a compile"
        );

        assert!(this_thread_may_commit_inline(), "the marker is per thread");
    }
}

#[cfg(test)]
mod connect_wires_without_inspecting_a_port_tests {
    //! Connect-path revert lock: a link is pure plumbing. Connect inspects
    //! nothing about either port beyond its existence, and wires in silence —
    //! not even advisorily. A payload mismatch is the consumer's decode failure
    //! at read, and nothing at wiring time hints at it. Reintroducing any
    //! inspection or comparison in [`connect_impl`] fails this module.

    use std::sync::Arc;

    use serde_json::Value;

    use parking_lot::Mutex;

    use super::{
        connect_impl, disconnect_impl, remove_processor_impl, the_node_this_runtime_names,
    };
    use crate::core::compiler::{Compiler, PendingOperation};
    use crate::core::descriptors::ProcessorClassImportPath;
    use crate::core::descriptors::{PortDescriptor, ProcessorClassShortName, ProcessorDescriptor};
    use crate::core::graph::{
        GraphEdgeWithComponents, InputLinkPortRef, LinkUniqueId, OutputLinkPortRef,
        PendingDeletionComponent, ProcessorUniqueId,
    };
    use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
    use crate::core::pubsub::{Event, PUBSUB, RuntimeEvent};
    use crate::core::test_support::CapturedTracingWarnings;
    use crate::core::{Error, Result};

    const PRODUCER_TYPE: &str = "ConnectSilenceProducer";
    const CONSUMER_TYPE: &str = "ConnectSilenceConsumer";

    fn class_short_name(ty: &str) -> ProcessorClassShortName {
        ProcessorClassShortName::new(ty).unwrap()
    }

    fn producer_class_path() -> ProcessorClassImportPath {
        ProcessorClassImportPath::new(format!("{}::{PRODUCER_TYPE}", module_path!())).unwrap()
    }

    fn consumer_class_path() -> ProcessorClassImportPath {
        ProcessorClassImportPath::new(format!("{}::{CONSUMER_TYPE}", module_path!())).unwrap()
    }

    /// Register the producer and consumer descriptors this module wires.
    fn register_producer_and_consumer_descriptors() {
        static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
        REGISTERED_ONCE_PER_PROCESS.call_once(|| {
            let mut producer = ProcessorDescriptor::new(
                class_short_name(PRODUCER_TYPE),
                producer_class_path(),
                "producer",
            );
            producer
                .outputs
                .push(PortDescriptor::new("out", "output", true));
            PROCESSOR_REGISTRY
                .register_descriptor_only(producer)
                .expect("register producer descriptor");

            let mut consumer = ProcessorDescriptor::new(
                class_short_name(CONSUMER_TYPE),
                consumer_class_path(),
                "consumer",
            );
            consumer
                .inputs
                .push(PortDescriptor::new("in", "input", true).with_delivery_profile("newest"));
            PROCESSOR_REGISTRY
                .register_descriptor_only(consumer)
                .expect("register consumer descriptor");
        });
    }

    /// Fresh compiler holding one producer and one consumer node, plus the
    /// wiring refs for the producer's `out` and the consumer's `in`.
    fn compiler_holding_a_producer_and_consumer_node()
    -> (Arc<Compiler>, OutputLinkPortRef, InputLinkPortRef) {
        let compiler = Arc::new(Compiler::new());
        let (from_id, to_id): (ProcessorUniqueId, ProcessorUniqueId) =
            compiler.scope(|graph, _tx| {
                let from = graph
                    .traversal_mut()
                    .add_v(ProcessorSpec::new(producer_class_path(), Value::Null))
                    .expect("the node is named")
                    .first()
                    .expect("producer node must be created")
                    .id
                    .clone();
                let to = graph
                    .traversal_mut()
                    .add_v(ProcessorSpec::new(consumer_class_path(), Value::Null))
                    .expect("the node is named")
                    .first()
                    .expect("consumer node must be created")
                    .id
                    .clone();
                (from, to)
            });
        (
            compiler,
            OutputLinkPortRef::new(from_id, "out"),
            InputLinkPortRef::new(to_id, "in"),
        )
    }

    #[test]
    fn connect_wires_a_producer_to_a_consumer_without_warning() {
        register_producer_and_consumer_descriptors();
        let (compiler, from, to) = compiler_holding_a_producer_and_consumer_node();
        let (result, captured) = CapturedTracingWarnings::captured_while(|| {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("current-thread runtime")
                .block_on(connect_impl(compiler, None, from, to))
        });

        result.expect("connect must wire any two ports — a link is pure plumbing");
        assert!(
            captured.is_empty(),
            "connect must emit no WARN when wiring a link; captured: {captured:?}"
        );
    }

    /// Removing a processor removes each of its links as a link, ahead of the
    /// node, so the compile reclaims both endpoints' ports through the link
    /// path. Revert lock: log only `RemoveProcessor` and the node's removal
    /// cascades its edges out of the graph with the peer's publisher,
    /// subscriber and notifier still held against their channels.
    #[test]
    fn remove_processor_logs_every_incident_link_ahead_of_the_node() {
        register_producer_and_consumer_descriptors();
        let (compiler, from, to) = compiler_holding_a_producer_and_consumer_node();
        let consumer_id = to.processor_id().clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime");

        let link_id = runtime
            .block_on(connect_impl(Arc::clone(&compiler), None, from, to))
            .expect("the link is created");
        runtime
            .block_on(remove_processor_impl(
                Arc::clone(&compiler),
                None,
                consumer_id.clone(),
            ))
            .expect("the consumer is removed");

        let logged = compiler.logged_pending_operations();
        let link_removal = logged
            .iter()
            .position(|op| matches!(op, PendingOperation::RemoveLink(id) if *id == link_id))
            .expect("the link's own removal is logged");
        let node_removal = logged
            .iter()
            .position(
                |op| matches!(op, PendingOperation::RemoveProcessor(id) if *id == consumer_id),
            )
            .expect("the node's removal is logged");
        assert!(
            link_removal < node_removal,
            "the link goes before the node it hangs off; logged {logged:?}"
        );
        let link_is_marked_for_deletion = compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .e(&link_id)
                .first()
                .map(|link| link.has::<PendingDeletionComponent>())
                .unwrap_or(false)
        });
        assert!(
            link_is_marked_for_deletion,
            "the link is marked pending deletion like the node"
        );
    }

    /// Run `connect` on a current-thread runtime, the way the two tests above
    /// do.
    fn connect_on_this_thread(
        compiler: &Arc<Compiler>,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
    ) -> Result<LinkUniqueId> {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime")
            .block_on(connect_impl(Arc::clone(compiler), None, from, to))
    }

    /// The display name the graph gave one of the fixture's nodes.
    fn the_display_name_the_graph_gave(
        compiler: &Arc<Compiler>,
        processor_id: &ProcessorUniqueId,
    ) -> String {
        compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .v(processor_id)
                .first()
                .expect("the node is in the graph")
                .display_name
                .clone()
        })
    }

    /// Run `disconnect` on a current-thread runtime, the way `connect` is run.
    fn disconnect_on_this_thread(compiler: &Arc<Compiler>, link_id: LinkUniqueId) -> Result<()> {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime")
            .block_on(disconnect_impl(Arc::clone(compiler), None, link_id))
    }

    /// A node name this runtime does not hold is refused by name, listing the
    /// names it holds.
    #[test]
    fn a_node_name_this_runtime_does_not_hold_is_refused_listing_what_it_holds() {
        register_producer_and_consumer_descriptors();
        let (compiler, from, _to) = compiler_holding_a_producer_and_consumer_node();
        let displayed = the_display_name_the_graph_gave(&compiler, from.processor_id());

        let refusal = the_node_this_runtime_names(&compiler, "NoSuchProcessor")
            .expect_err("a node name this runtime does not hold is refused")
            .to_string();

        assert!(refusal.contains("NoSuchProcessor"), "{refusal}");
        assert!(refusal.contains(&displayed), "{refusal}");
    }

    /// The disconnect events name each end's own port on a link whose two port
    /// names differ.
    ///
    /// Mental-revert: rebuild either endpoint from the other's port name — the
    /// shape this path carried until #2292 — and a link `out` → `in` announces
    /// itself as `out` → `out`, so a listener keyed on the port it watches
    /// never sees the disconnect.
    #[test]
    fn the_disconnect_events_name_each_ends_own_port_when_the_two_differ() {
        use crate::core::pubsub::{EventListener, topics};

        register_producer_and_consumer_descriptors();
        let (compiler, from, to) = compiler_holding_a_producer_and_consumer_node();
        let link_id = connect_on_this_thread(&compiler, from.clone(), to.clone())
            .expect("the fixture's two nodes connect");

        struct RecordingTheDisconnectEvents(Arc<Mutex<Vec<(String, String)>>>);
        impl EventListener for RecordingTheDisconnectEvents {
            fn on_event(&mut self, event: &Event) -> Result<()> {
                if let Event::RuntimeGlobal(
                    RuntimeEvent::RuntimeWillDisconnect {
                        from_port, to_port, ..
                    }
                    | RuntimeEvent::RuntimeDidDisconnect {
                        from_port, to_port, ..
                    },
                ) = event
                {
                    self.0.lock().push((from_port.clone(), to_port.clone()));
                }
                Ok(())
            }
        }

        let announced = Arc::new(Mutex::new(Vec::new()));
        let listener: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(
            RecordingTheDisconnectEvents(Arc::clone(&announced)),
        ));
        PUBSUB
            .subscribe(topics::RUNTIME_GLOBAL, Arc::clone(&listener))
            .expect("subscribe establishes the subscriber");

        disconnect_on_this_thread(&compiler, link_id).expect("the link disconnects");

        // Delivery is not synchronous with the publish.
        let gave_up_at = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < gave_up_at && announced.lock().len() < 2 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let announced = announced.lock().clone();
        assert_eq!(
            announced.len(),
            2,
            "both the will- and did-disconnect events are published: {announced:?}"
        );
        for (from_port, to_port) in announced {
            assert_eq!(from_port, from.to_string(), "the source end names `out`");
            assert_eq!(to_port, to.to_string(), "the destination end names `in`");
        }
    }

    /// A destination this runtime does not hold, or a port it does not have,
    /// is refused with the typed error a caller can act on.
    #[test]
    fn a_destination_this_runtime_cannot_take_is_refused_with_a_typed_error() {
        register_producer_and_consumer_descriptors();
        let (compiler, from, to) = compiler_holding_a_producer_and_consumer_node();

        assert!(matches!(
            connect_on_this_thread(
                &compiler,
                from.clone(),
                InputLinkPortRef::new("Pnobody", "in")
            ),
            Err(Error::ProcessorNotFound(_))
        ));
        assert!(matches!(
            connect_on_this_thread(
                &compiler,
                from,
                InputLinkPortRef::new(to.processor_id().clone(), "no_such_port")
            ),
            Err(Error::ProcessorPortNotFound { .. })
        ));
    }
}
