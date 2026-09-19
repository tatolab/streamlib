// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use super::Runner;
use super::RuntimeStatus;
use super::mesh::RuntimeMeshMembership;
use super::mesh_address_chunk::{
    first_reason_this_is_not_one_mesh_address_chunk, what_one_mesh_address_chunk_may_be,
};
use super::operations::{BoxFuture, RuntimeOperations};
use super::runtime::TokioRuntimeVariant;
use super::surface_image_exchange::exchange_published_surface_id_for_png_image_bytes;
use crate::core::RuntimeContext;
use crate::core::compiler::{Compiler, PendingOperation};
use crate::core::graph::{
    GraphEdgeWithComponents, GraphNodeWithComponents, LinkRequestUniqueId, LinkUniqueId,
    MeshPortAddress, PendingDeletionComponent, ProcessorUniqueId, StateComponent,
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

/// Refuse `requested_display_name` unless it is one legal mesh address chunk.
///
/// Beside the add path rather than in the grammar module: the grammar knows
/// nothing about processors, and the remedy this names is the add's own.
fn refuse_a_display_name_that_is_not_one_mesh_address_chunk(
    requested_display_name: &str,
) -> Result<()> {
    match first_reason_this_is_not_one_mesh_address_chunk(requested_display_name) {
        None => Ok(()),
        Some(what_is_wrong) => Err(Error::Configuration(format!(
            "display name {requested_display_name:?} cannot be one chunk of a processor's mesh \
             address: {what_is_wrong}. {}. Rename the processor, or leave `display_name` out to \
             take the class's own short name",
            what_one_mesh_address_chunk_may_be()
        ))),
    }
}

/// Core implementation for add_processor - takes owned Arcs for 'static lifetime.
///
/// Reports the display name the graph assigned alongside the id. Both come out
/// of the one `compiler.scope` that added the node, so a caller that needs the
/// name never has to ask a second time — and never races a concurrent removal
/// into being told its own successful add does not exist.
async fn add_processor_impl(
    compiler: Arc<Compiler>,
    live: LiveCommitContext,
    spec: ProcessorSpec,
) -> Result<(ProcessorUniqueId, String)> {
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

    // Before anything else: the display name is the processor's part of its
    // mesh address, so a name that cannot be one address chunk is a wiring
    // error whatever door the add came through — `rt.add`, Rust, or the
    // control plane's `add_processor`.
    if let Some(requested_display_name) = spec.display_name.as_deref() {
        refuse_a_display_name_that_is_not_one_mesh_address_chunk(requested_display_name)?;
    }

    // A type nobody registered may still be resolvable by name — the wheel
    // resolves a Python class import path the way `rt.add` would. A resolver
    // that fails names why; one that is absent leaves the registry miss below
    // to say the type is unknown.
    PROCESSOR_REGISTRY.resolve_processor_type_if_unregistered(&spec.name)?;

    // Held so a typed `UnknownProcessorType` can name what was asked for —
    // `spec` is moved into `add_v`.
    let ident_for_err = spec.name.clone();

    let added = compiler.scope(|graph, tx| -> Result<(ProcessorUniqueId, String)> {
        let (node_id, assigned_display_name) = graph
            .traversal_mut()
            .add_v(spec)
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
        Ok((node_id, assigned_display_name))
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
///
/// A source naming a port on another runtime never waits on the mesh: the link
/// is applied here and now, `awaiting_remote` until that runtime turns up.
#[tracing::instrument(
    name = "runtime.connect",
    skip(compiler, live, runtime_mesh),
    fields(from = %from, to = %to),
)]
async fn connect_impl(
    compiler: Arc<Compiler>,
    live: LiveCommitContext,
    runtime_mesh: Arc<RuntimeMeshMembership>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<LinkUniqueId> {
    // An address naming this runtime's own name is a local reference, resolved
    // by display name — so an app can spell one of its own ports the way a peer
    // spells it and get the ordinary local link.
    let from = resolve_a_source_addressing_this_runtimes_own_port(&compiler, &runtime_mesh, from)?;
    let to = resolve_a_destination_addressing_this_runtimes_own_port(&compiler, &runtime_mesh, to)?;

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillConnect {
            from: from.clone(),
            to: to.clone(),
        }),
    );

    let link_id = match from.clone() {
        OutputLinkPortRef::OnAnotherRuntime(address) => {
            apply_a_link_from_another_runtime(&compiler, &runtime_mesh, address, to.clone())?
        }
        OutputLinkPortRef::OnThisRuntime {
            processor_id,
            port_name,
        } => apply_a_link_from_this_runtime(&compiler, processor_id, port_name, to.clone())?,
    };

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

/// Turn a mesh address naming this runtime's own name into the local reference
/// it means, and leave every other source alone.
///
/// Refused by name when this runtime holds no processor under that display
/// name, listing the ones it does — the local half of the offered-port refusal
/// a peer gets.
fn resolve_a_source_addressing_this_runtimes_own_port(
    compiler: &Arc<Compiler>,
    runtime_mesh: &RuntimeMeshMembership,
    from: OutputLinkPortRef,
) -> Result<OutputLinkPortRef> {
    let Some(address) = from.mesh_port_address() else {
        return Ok(from);
    };
    if !address.names_the_runtime(runtime_mesh.runtime_name()) {
        return Ok(from);
    }
    let processor_id = the_processor_this_runtime_displays_as(compiler, address)?;
    Ok(OutputLinkPortRef::new(processor_id, address.port_name()))
}

/// Turn a mesh address naming this runtime's own name into the local reference
/// it means, and leave every other destination alone.
///
/// The destination mirror of the source resolver above, and the reason a push
/// an app aims at itself is an ordinary local link rather than a request this
/// runtime sends to itself.
fn resolve_a_destination_addressing_this_runtimes_own_port(
    compiler: &Arc<Compiler>,
    runtime_mesh: &RuntimeMeshMembership,
    to: InputLinkPortRef,
) -> Result<InputLinkPortRef> {
    let Some(address) = to.mesh_port_address() else {
        return Ok(to);
    };
    if !address.names_the_runtime(runtime_mesh.runtime_name()) {
        return Ok(to);
    }
    let processor_id = the_processor_this_runtime_displays_as(compiler, address)?;
    Ok(InputLinkPortRef::new(processor_id, address.port_name()))
}

/// The processor this runtime displays under `address`'s display name.
///
/// Refused by name when this runtime holds no processor under it, listing the
/// ones it does — the local half of the offered-port refusal a peer gets.
fn the_processor_this_runtime_displays_as(
    compiler: &Arc<Compiler>,
    address: &MeshPortAddress,
) -> Result<ProcessorUniqueId> {
    compiler.scope(|graph, _tx| {
        if let Some(named) = graph
            .traversal()
            .v_with_display_name(address.processor_display_name())
            .first()
        {
            return Ok(named.id.clone());
        }
        let mut display_names: Vec<String> = graph
            .traversal()
            .v(())
            .iter()
            .map(|node| node.display_name.clone())
            .collect();
        display_names.sort();
        Err(Error::ProcessorNotFound(format!(
            "no processor on this runtime is displayed as {:?}, which {address} names. This \
             runtime is displaying: {}",
            address.processor_display_name(),
            if display_names.is_empty() {
                "nothing".to_string()
            } else {
                display_names.join(", ")
            }
        )))
    })
}

/// Apply a link both of whose ends are on this runtime — the path every local
/// link has always taken.
fn apply_a_link_from_this_runtime(
    compiler: &Arc<Compiler>,
    from_processor: ProcessorUniqueId,
    from_port: String,
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
                    .v(&from_processor)
                    .first()
                    .ok_or_else(|| Error::ProcessorNotFound(from_processor.to_string()))?;
                if !from_node.has_output(&from_port) {
                    return Err(Error::ProcessorPortNotFound {
                        processor_id: from_processor.to_string(),
                        port_name: from_port.clone(),
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
            let channel = crate::iceoryx2::source_channel_name(from_processor.as_str(), &from_port)
                .map_err(|source| Error::InvalidLink(source.to_string()))?;

            let link_id = graph
                .traversal_mut()
                .add_e(OutputLinkPortRef::new(from_processor, from_port), to)
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

/// Apply a link carrying from a port on another runtime.
///
/// Its destination side is queued for the compiler like any other link's: the
/// channel it subscribes to is derived from the address, so it is wired whether
/// or not that runtime is here. Only the source side waits — the mesh opens the
/// ingress that publishes onto that channel once the runtime turns up and says
/// it offers the port. The destination is validated now, with the same typed
/// refusals a local link meets.
fn apply_a_link_from_another_runtime(
    compiler: &Arc<Compiler>,
    runtime_mesh: &RuntimeMeshMembership,
    source: MeshPortAddress,
    to: InputLinkPortRef,
) -> Result<LinkUniqueId> {
    // Deliberately says nothing about the runtime it names: whether that one is
    // absent, silent or perfectly healthy is not known until the mesh has
    // looked, and this end may be the one that is off the mesh. The first
    // resolution pass overwrites it with what it actually found.
    let waiting_on = format!(
        "this link has just been applied and the {} mesh has not resolved it yet",
        runtime_mesh.mesh_name()
    );
    let (link_id, how_far_it_has_got) = compiler.scope(|graph, tx| -> Result<_> {
        refuse_a_destination_this_graph_cannot_take(graph, &to)?;

        let link = graph
            .traversal_mut()
            .add_link_from_another_runtime(source.clone(), to)
            .first_mut()
            .ok_or_else(|| Error::GraphError("failed to create link after validation".into()))?;
        // Wired like any other link: the destination subscribes to the
        // engine-named channel the address derives, which the ingress publishes
        // onto once the source runtime turns up. The state the link *reports*
        // stays `awaiting_remote` until it does, off the cell below, because
        // nothing carries before then.
        link.state = crate::core::graph::LinkState::AwaitingRemote;
        link.insert(crate::core::graph::LinkStateComponent(
            crate::core::graph::LinkState::AwaitingRemote,
        ));
        let resolution =
            crate::core::graph::RemoteLinkResolutionComponent::awaiting_remote(waiting_on);
        let how_far_it_has_got = resolution.its_cell();
        link.insert_component_without_rendering_it(resolution);
        let link_id = link.id.clone();
        tx.log(PendingOperation::AddLink(link_id.clone()));
        Ok((link_id, how_far_it_has_got))
    })?;

    // The mesh takes it from here: it resolves the address, refuses by name
    // what it cannot carry, and opens the ingress when it can.
    runtime_mesh.note_a_link_from_another_runtime(source, link_id.clone(), how_far_it_has_got);
    Ok(link_id)
}

/// The address a peer reads one of this runtime's own output ports at.
///
/// Refused by name when the port is not there, because a request naming a port
/// this runtime does not publish would wait on an egress that never starts and
/// say nothing about why.
fn this_runtimes_address_for_one_of_its_own_output_ports(
    compiler: &Arc<Compiler>,
    this_runtimes_name: &str,
    processor_id: &ProcessorUniqueId,
    port_name: &str,
) -> Result<MeshPortAddress> {
    let display_name = compiler.scope(|graph, _tx| {
        let node = graph
            .traversal()
            .v(processor_id)
            .first()
            .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))?;
        if !node.has_output(port_name) {
            return Err(Error::ProcessorPortNotFound {
                processor_id: processor_id.to_string(),
                port_name: port_name.to_string(),
                direction: PortDirection::Output,
            });
        }
        Ok(node.display_name.clone())
    })?;
    MeshPortAddress::new(this_runtimes_name, display_name, port_name)
}

/// Refuse a destination this graph has no processor or no such input port for,
/// with the typed error a caller can act on.
fn refuse_a_destination_this_graph_cannot_take(
    graph: &crate::core::graph::Graph,
    to: &InputLinkPortRef,
) -> Result<()> {
    // A destination naming this runtime's own name was resolved to a local
    // reference before this, so anything still remote belongs to another
    // runtime — and the runtime that owns an input is the one that applies the
    // link. `connect` cannot apply this one anywhere.
    let Some(destination_processor_id) = to.processor_id_on_this_runtime() else {
        return Err(Error::InvalidLink(format!(
            "the destination {to} is a port on another runtime, and only the runtime that owns an \
             input applies a link into it. Ask that runtime for the link with \
             `request_link_on_remote_input_runtime` instead of connecting it here"
        )));
    };
    let to_node = graph
        .traversal()
        .v(destination_processor_id)
        .first()
        .ok_or_else(|| Error::ProcessorNotFound(destination_processor_id.to_string()))?;
    if !to_node.has_input(to.port_name()) {
        return Err(Error::ProcessorPortNotFound {
            processor_id: destination_processor_id.to_string(),
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

    /// Add a processor and report the display name the graph assigned it, which
    /// is the requested one only when no other node already answered to it.
    pub fn add_processor_reporting_assigned_display_name(
        &self,
        spec: ProcessorSpec,
    ) -> Result<(ProcessorUniqueId, String)> {
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

    fn add_processor_async(&self, spec: ProcessorSpec) -> BoxFuture<'_, Result<ProcessorUniqueId>> {
        let compiler = Arc::clone(&self.compiler);
        let live = self.live_commit_context();
        Box::pin(async move {
            add_processor_impl(compiler, live, spec)
                .await
                .map(|(processor_id, _assigned_display_name)| processor_id)
        })
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
        let runtime_mesh = Arc::clone(&self.runtime_mesh);
        Box::pin(connect_impl(compiler, live, runtime_mesh, from, to))
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
        // Resolve what the caller named to the source that publishes to it,
        // and that source's iceoryx2 sizing, from the live graph BEFORE
        // spawning: the same derivation the compiler op used to open the
        // service, so the tap's publisher-free reopen requests identical,
        // iceoryx2-verified parameters. A port on another runtime is named by
        // its mesh address and tapped on the channel its ingress writes — the
        // channel name is hashed from that address and is nothing a caller
        // could be expected to spell.
        let resolved = self.compiler.scope(
            |graph, _tx| -> Result<(String, crate::iceoryx2::ChannelSizing)> {
                let source = crate::core::compiler::compiler_ops::find_the_source_a_caller_named(
                    graph, &channel,
                )
                .ok_or_else(|| Error::TapChannelNotFound(channel.clone()))?;
                let sizing = crate::core::compiler::compiler_ops::resolve_channel_sizing(
                    graph,
                    &self.iceoryx2_node,
                    &source,
                )?;
                let channel_service_name = match source.mesh_port_address() {
                    Some(address) => {
                        let addressed: String = address.to_string();
                        crate::iceoryx2::mesh_ingress_channel_name(&addressed).into_string()
                    }
                    None => channel.clone(),
                };
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
        self.add_processor_reporting_assigned_display_name(spec)
            .map(|(processor_id, _assigned_display_name)| processor_id)
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
                let runtime_mesh = Arc::clone(&self.runtime_mesh);
                let (tx, rx) = std::sync::mpsc::channel();
                handle.spawn(async move {
                    let result = connect_impl(compiler, live, runtime_mesh, from, to).await;
                    let _ = tx.send(result);
                });
                rx.recv()
                    .map_err(|_| Error::Runtime("Task channel closed".into()))?
            }
        }
    }

    fn runtime_name(&self) -> String {
        self.runtime_mesh.runtime_name().to_string()
    }

    fn request_link_on_remote_input_runtime(
        &self,
        from: OutputLinkPortRef,
        to: MeshPortAddress,
    ) -> Result<LinkRequestUniqueId> {
        if to.names_the_runtime(self.runtime_mesh.runtime_name()) {
            return Err(Error::InvalidLink(format!(
                "the destination {to} is a port on this runtime, which applies its own links. \
                 Use `connect` instead of asking across the mesh for one"
            )));
        }
        // A processor id never appears on the mesh, so a source on this runtime
        // becomes the address a peer can read it at: this runtime's name, the
        // processor's display name, and the port.
        let source_address = match from {
            OutputLinkPortRef::OnAnotherRuntime(address) => address,
            OutputLinkPortRef::OnThisRuntime {
                processor_id,
                port_name,
            } => this_runtimes_address_for_one_of_its_own_output_ports(
                &self.compiler,
                self.runtime_mesh.runtime_name(),
                &processor_id,
                &port_name,
            )?,
        };
        let link_request_id = LinkRequestUniqueId::new();
        let input_runtime_name = to.runtime_name().to_string();
        self.runtime_mesh.ask_another_runtime_for_a_link(
            crate::core::runtime::mesh::ALinkRequestOnTheMesh::asking_for_a_link(
                link_request_id.clone(),
                source_address,
                to,
                self.runtime_mesh.runtime_name(),
            ),
            &input_runtime_name,
        );
        Ok(link_request_id)
    }

    fn request_disconnect_on_remote_input_runtime(
        &self,
        input_runtime_name: String,
        link_id: LinkUniqueId,
    ) -> Result<LinkRequestUniqueId> {
        if input_runtime_name == self.runtime_mesh.runtime_name() {
            return Err(Error::InvalidLink(format!(
                "the link {link_id} is on this runtime, which removes its own links. Use \
                 `disconnect` instead of asking across the mesh for one"
            )));
        }
        let link_request_id = LinkRequestUniqueId::new();
        self.runtime_mesh.ask_another_runtime_for_a_link(
            crate::core::runtime::mesh::ALinkRequestOnTheMesh::asking_for_a_link_to_go(
                link_request_id.clone(),
                link_id,
                self.runtime_mesh.runtime_name(),
            ),
            &input_runtime_name,
        );
        Ok(link_request_id)
    }

    fn cancel_link_request(&self, link_request_id: &LinkRequestUniqueId) -> Result<()> {
        if self.runtime_mesh.cancel_a_link_request(link_request_id) {
            return Ok(());
        }
        Err(Error::NotFound(format!(
            "this runtime is holding no link request {link_request_id}. It was applied, it was \
             cancelled already, or it was never made here — `graph` lists the ones it holds \
             under `mesh.link_requests_awaiting_runtime`."
        )))
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

    use super::{RuntimeMeshMembership, connect_impl, disconnect_impl, remove_processor_impl};
    use crate::core::pubsub::{Event, PUBSUB, RuntimeEvent};
    use crate::core::compiler::{Compiler, PendingOperation};
    use crate::core::descriptors::ProcessorClassImportPath;
    use crate::core::descriptors::{PortDescriptor, ProcessorClassShortName, ProcessorDescriptor};
    use crate::core::graph::{
        GraphEdgeWithComponents, InputLinkPortRef, LinkUniqueId, MeshPortAddress,
        OutputLinkPortRef, PendingDeletionComponent, ProcessorUniqueId,
    };
    use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
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

    /// A membership naming this runtime and its mesh, on no network — enough
    /// for `connect` to tell an address naming this runtime from one naming
    /// another.
    fn a_membership_off_any_mesh() -> Arc<RuntimeMeshMembership> {
        Arc::new(RuntimeMeshMembership::that_never_reached_its_mesh(
            THIS_RUNTIMES_NAME,
            "a-test-mesh",
        ))
    }

    /// The name the membership above answers to.
    const THIS_RUNTIMES_NAME: &str = "this-runtime";

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
                    .first()
                    .expect("producer node must be created")
                    .id
                    .clone();
                let to = graph
                    .traversal_mut()
                    .add_v(ProcessorSpec::new(consumer_class_path(), Value::Null))
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
                .block_on(connect_impl(
                    compiler,
                    None,
                    a_membership_off_any_mesh(),
                    from,
                    to,
                ))
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
        let consumer_id = to
            .processor_id_on_this_runtime()
            .expect("a test consumer is on this runtime")
            .clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime");

        let link_id = runtime
            .block_on(connect_impl(
                Arc::clone(&compiler),
                None,
                a_membership_off_any_mesh(),
                from,
                to,
            ))
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
            .block_on(connect_impl(
                Arc::clone(compiler),
                None,
                a_membership_off_any_mesh(),
                from,
                to,
            ))
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

    /// An address naming this runtime's own name is a local reference: it
    /// resolves by display name and the link is an ordinary edge, wired by the
    /// compiler like any other.
    #[test]
    fn an_address_naming_this_runtime_is_resolved_by_display_name() {
        register_producer_and_consumer_descriptors();
        let (compiler, from, to) = compiler_holding_a_producer_and_consumer_node();
        let producer = from
            .processor_id_on_this_runtime()
            .cloned()
            .expect("the fixture's source is local");
        let displayed = the_display_name_the_graph_gave(&compiler, &producer);

        let link_id = connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::on_another_runtime(
                MeshPortAddress::new(THIS_RUNTIMES_NAME, displayed, "out")
                    .expect("a legal address"),
            ),
            to,
        )
        .expect("an address naming this runtime wires locally");

        compiler.scope(|graph, _tx| {
            let link = graph
                .traversal()
                .e(&link_id)
                .first()
                .expect("the link is in the graph");
            assert_eq!(
                link.from_port().processor_id_on_this_runtime(),
                Some(&producer),
                "the address must resolve to the node the display name labels"
            );
        });
        assert!(
            compiler
                .logged_pending_operations()
                .iter()
                .any(|op| matches!(op, PendingOperation::AddLink(id) if *id == link_id)),
            "a link resolved locally is wired by the compiler like any other"
        );
    }

    /// A display name this runtime does not hold is refused by name, listing
    /// what it is displaying — the local half of the offered-port refusal.
    #[test]
    fn a_display_name_this_runtime_does_not_hold_is_refused_listing_what_it_displays() {
        register_producer_and_consumer_descriptors();
        let (compiler, from, to) = compiler_holding_a_producer_and_consumer_node();
        let displayed = the_display_name_the_graph_gave(
            &compiler,
            &from
                .processor_id_on_this_runtime()
                .cloned()
                .expect("the fixture's source is local"),
        );

        let refusal = connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::on_another_runtime(
                MeshPortAddress::new(THIS_RUNTIMES_NAME, "NoSuchProcessor", "out")
                    .expect("a legal address"),
            ),
            to,
        )
        .expect_err("a display name this runtime does not hold is refused")
        .to_string();

        assert!(refusal.contains("NoSuchProcessor"), "{refusal}");
        assert!(refusal.contains(&displayed), "{refusal}");
    }

    /// A destination address naming this runtime's own name is a local
    /// reference too, so an app can push at itself with the spelling a peer
    /// would use and get the ordinary local link.
    #[test]
    fn a_destination_address_naming_this_runtime_is_resolved_by_display_name() {
        register_producer_and_consumer_descriptors();
        let (compiler, from, to) = compiler_holding_a_producer_and_consumer_node();
        let consumer = to
            .processor_id_on_this_runtime()
            .cloned()
            .expect("the fixture's destination is local");
        let displayed = the_display_name_the_graph_gave(&compiler, &consumer);

        let link_id = connect_on_this_thread(
            &compiler,
            from,
            InputLinkPortRef::on_another_runtime(
                MeshPortAddress::new(THIS_RUNTIMES_NAME, displayed, "in").expect("a legal address"),
            ),
        )
        .expect("an address naming this runtime wires locally");

        compiler.scope(|graph, _tx| {
            let link = graph
                .traversal()
                .e(&link_id)
                .first()
                .expect("the link is in the graph");
            assert_eq!(
                link.to_port().processor_id_on_this_runtime(),
                Some(&consumer),
                "the address must resolve to the node the display name labels"
            );
        });
    }

    /// A destination display name this runtime does not hold is refused the
    /// same way a source one is, listing what it is displaying.
    #[test]
    fn a_destination_display_name_this_runtime_does_not_hold_is_refused_listing_what_it_displays() {
        register_producer_and_consumer_descriptors();
        let (compiler, from, to) = compiler_holding_a_producer_and_consumer_node();
        let displayed = the_display_name_the_graph_gave(
            &compiler,
            &to.processor_id_on_this_runtime()
                .cloned()
                .expect("the fixture's destination is local"),
        );

        let refusal = connect_on_this_thread(
            &compiler,
            from,
            InputLinkPortRef::on_another_runtime(
                MeshPortAddress::new(THIS_RUNTIMES_NAME, "NoSuchProcessor", "in")
                    .expect("a legal address"),
            ),
        )
        .expect_err("a display name this runtime does not hold is refused")
        .to_string();

        assert!(refusal.contains("NoSuchProcessor"), "{refusal}");
        assert!(refusal.contains(&displayed), "{refusal}");
    }

    /// A destination on *another* runtime is refused by name and told which
    /// door takes one: only the runtime that owns an input applies a link into
    /// it, so `connect` has nowhere to put this link.
    ///
    /// Mental-revert: let it through and the graph gains an edge whose
    /// destination names no node here, which the wiring op would open a channel
    /// nobody reads for.
    #[test]
    fn a_destination_on_another_runtime_is_refused_naming_the_door_that_takes_one() {
        register_producer_and_consumer_descriptors();
        let (compiler, from, _to) = compiler_holding_a_producer_and_consumer_node();

        let refusal = connect_on_this_thread(
            &compiler,
            from,
            InputLinkPortRef::on_another_runtime(
                MeshPortAddress::new("studio-display-9f3c", "DisplayWindow", "video")
                    .expect("a legal address"),
            ),
        )
        .expect_err("connect applies a link here and cannot apply one there")
        .to_string();

        assert!(
            refusal.contains("studio-display-9f3c/DisplayWindow/video"),
            "{refusal}"
        );
        assert!(
            refusal.contains("request_link_on_remote_input_runtime"),
            "the refusal must name the door that does take one: {refusal}"
        );
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

    /// A source on another runtime lands `awaiting_remote` while its
    /// destination side is queued for the compiler like any other link's — the
    /// channel it subscribes to is derived from the address, and nothing about
    /// it needs that runtime to be here.
    ///
    /// This runtime is off any mesh, so what it is waiting on is *itself*. A
    /// reason naming the source runtime would report a peer that may be
    /// perfectly healthy as the thing that is missing.
    #[test]
    fn a_source_on_another_runtime_waits_on_this_runtimes_own_mesh_while_its_destination_wires() {
        register_producer_and_consumer_descriptors();
        let (compiler, _from, to) = compiler_holding_a_producer_and_consumer_node();

        let link_id = connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::on_another_runtime(
                MeshPortAddress::new("bench-cam-a1b2", "CameraSource", "video")
                    .expect("a legal address"),
            ),
            to,
        )
        .expect("connect never waits on the mesh");

        let rendered = compiler.scope(|graph, _tx| {
            crate::core::json_schema::LinkOutput::of_a_link_on_the_runtime_named(
                graph
                    .traversal()
                    .e(&link_id)
                    .first()
                    .expect("the link is in the graph"),
                THIS_RUNTIMES_NAME,
            )
        });
        assert_eq!(
            serde_json::to_value(&rendered.state).expect("the state renders"),
            serde_json::json!("awaiting_remote")
        );
        let waiting_on = rendered
            .awaiting_remote_reason
            .expect("a waiting link says what it is waiting on");
        assert!(
            waiting_on.contains("this runtime is not on the"),
            "a runtime off its own mesh says so rather than blaming the source: {waiting_on}"
        );
        assert!(
            !waiting_on.contains("bench-cam-a1b2"),
            "nothing has looked for that runtime yet, so nothing may be said about it:              {waiting_on}"
        );
        assert_eq!(
            serde_json::to_value(&rendered.source).expect("the source renders"),
            serde_json::json!({
                "runtime_name": "bench-cam-a1b2",
                "processor_display_name": "CameraSource",
                "port_name": "video",
            })
        );
        assert!(
            compiler
                .logged_pending_operations()
                .iter()
                .any(|op| matches!(op, PendingOperation::AddLink(id) if *id == link_id)),
            "the destination wires onto the channel the address derives, whether or not the \
             source runtime is here yet"
        );
    }

    /// A destination this runtime does not hold is refused the same way for a
    /// remote source as for a local one — `connect`'s own refusals, unchanged.
    #[test]
    fn a_remote_source_meets_the_same_destination_refusals_a_local_one_does() {
        register_producer_and_consumer_descriptors();
        let (compiler, _from, to) = compiler_holding_a_producer_and_consumer_node();
        let source = || {
            OutputLinkPortRef::on_another_runtime(
                MeshPortAddress::new("bench-cam-a1b2", "CameraSource", "video")
                    .expect("a legal address"),
            )
        };

        assert!(matches!(
            connect_on_this_thread(&compiler, source(), InputLinkPortRef::new("Pnobody", "in")),
            Err(Error::ProcessorNotFound(_))
        ));
        assert!(matches!(
            connect_on_this_thread(
                &compiler,
                source(),
                InputLinkPortRef::new(
                    to.processor_id_on_this_runtime()
                        .expect("a test consumer is on this runtime")
                        .clone(),
                    "no_such_port"
                )
            ),
            Err(Error::ProcessorPortNotFound { .. })
        ));
    }
}
