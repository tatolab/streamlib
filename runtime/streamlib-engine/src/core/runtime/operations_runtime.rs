// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use super::Runner;
use super::operations::{BoxFuture, RuntimeOperations};
use super::runtime::TokioRuntimeVariant;
use crate::core::compiler::{Compiler, PendingOperation};
use crate::core::graph::{
    GraphEdgeWithComponents, GraphNodeWithComponents, LinkUniqueId, PendingDeletionComponent,
    ProcessorUniqueId, StateComponent,
};
use crate::core::processors::{ProcessorSpec, ProcessorState};
use crate::core::pubsub::{Event, PUBSUB, RuntimeEvent, topics};
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, PortDirection, Result};
use streamlib_idents::ChannelName;

// =============================================================================
// Core Implementation Functions ('static async fns for spawn compatibility)
// =============================================================================

/// Core implementation for add_processor - takes owned Arcs for 'static lifetime.
///
/// `lazy_error` carries the outcome of the lazy plugin-discovery step the
/// caller ran before this (see [`Runner::lazily_load_provider_for_processor_type`]):
/// `Some` when discovery was ambiguous or a discovered package failed to load.
/// On a registry miss it is returned in place of the generic
/// [`Error::UnknownProcessorType`], so the app sees the specific recoverable
/// reason while the failed node is still surfaced in the graph for
/// observability. `None` (type available after the lazy load, or no package
/// provided it) leaves the existing behavior unchanged.
async fn add_processor_impl(
    compiler: Arc<Compiler>,
    spec: ProcessorSpec,
    lazy_error: Option<Error>,
) -> Result<ProcessorUniqueId> {
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

    // Hold a diagnostic ident so we can surface a typed `UnknownProcessorType`
    // error if the registry doesn't know this type — `spec` is moved into
    // `add_v`. A version-free reference projects to `(org, package, type)@0.0.0`.
    let ident_for_err = spec.name.to_diagnostic_ident();

    let processor_id = compiler.scope(|graph, tx| -> Result<ProcessorUniqueId> {
        let node_id = graph
            .traversal_mut()
            .add_v(spec)
            .first()
            .map(|node| node.id.clone())
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
            .map(|state_component| matches!(*state_component.0.lock(), ProcessorState::Error))
            .unwrap_or(false);

        if registry_miss {
            emit_will_add(&node_id);
            emit_did_add(&node_id);
            // A specific lazy-discovery failure (ambiguous providers, or a
            // discovered package that failed to load) supersedes the generic
            // unknown-type error; a plain "no package provides it" leaves
            // `lazy_error` None and falls back to UnknownProcessorType.
            return Err(lazy_error.unwrap_or(Error::UnknownProcessorType {
                ident: ident_for_err,
            }));
        }

        emit_will_add(&node_id);
        tx.log(PendingOperation::AddProcessor(node_id.clone()));
        emit_did_add(&node_id);
        Ok(node_id)
    })?;

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
    );

    Ok(processor_id)
}

/// Core implementation for remove_processor - takes owned Arcs for 'static lifetime.
async fn remove_processor_impl(
    compiler: Arc<Compiler>,
    processor_id: ProcessorUniqueId,
) -> Result<()> {
    compiler.scope(|graph, tx| {
        if !graph.traversal().v(&processor_id).exists() {
            return Err(Error::ProcessorNotFound(processor_id.to_string()));
        }

        if let Some(node) = graph.traversal_mut().v(&processor_id).first_mut() {
            node.insert(PendingDeletionComponent);
        }

        tx.log(PendingOperation::RemoveProcessor(processor_id.clone()));

        Ok(())
    })?;

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

    Ok(())
}

/// Core implementation for connect - takes owned Arcs for 'static lifetime.
async fn connect_impl(
    compiler: Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<LinkUniqueId> {
    let from_processor = from.processor_id.clone();
    let from_port = from.port_name.clone();
    let to_processor = to.processor_id.clone();
    let to_port = to.port_name.clone();

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillConnect {
            from_processor: from_processor.clone(),
            from_port: from_port.clone(),
            to_processor: to_processor.clone(),
            to_port: to_port.clone(),
        }),
    );

    let (link_id, channel) = compiler.scope(|graph, tx| -> Result<(LinkUniqueId, ChannelName)> {
        // Validate endpoints + ports FIRST — before the channel-name
        // derivation — so a missing processor/port reads as the typed
        // ProcessorNotFound / ProcessorPortNotFound and never gets masked by an
        // InvalidLink from the wire-name grammar. The `add_e` call still checks
        // defensively; this pre-validation is what gets the typed error out.
        // Validate source processor + output port.
        {
            let from_node = graph
                .traversal()
                .v(&from.processor_id)
                .first()
                .ok_or_else(|| Error::ProcessorNotFound(from.processor_id.to_string()))?;
            if !from_node.has_output(&from.port_name) {
                return Err(Error::ProcessorPortNotFound {
                    processor_id: from.processor_id.to_string(),
                    port_name: from.port_name.clone(),
                    direction: PortDirection::Output,
                });
            }
        }
        // Validate target processor + input port.
        {
            let to_node = graph
                .traversal()
                .v(&to.processor_id)
                .first()
                .ok_or_else(|| Error::ProcessorNotFound(to.processor_id.to_string()))?;
            if !to_node.has_input(&to.port_name) {
                return Err(Error::ProcessorPortNotFound {
                    processor_id: to.processor_id.to_string(),
                    port_name: to.port_name.clone(),
                    direction: PortDirection::Input,
                });
            }
        }

        // The one channel name this link publishes to / subscribes from — the
        // deterministic `connect()` sugar (#1416). Intra-node it currently maps
        // onto the per-destination iceoryx2 service; the name is what phase [L]
        // cross-node routing keys on. Endpoints are validated above, so a
        // grammar failure here is a genuinely-illegal PORT name (author error),
        // surfaced as InvalidLink. Processor ids are lowercased inside
        // `connect_channel_name`; underscore is legal and rides through.
        // Deriving inside the transaction means an illegal port name rolls the
        // pending link back rather than committing a half-built edge.
        let channel = streamlib_idents::connect_channel_name(
            from.processor_id.as_str(),
            &from.port_name,
            to.processor_id.as_str(),
            &to.port_name,
        )
        .map_err(|source| Error::InvalidLink(source.to_string()))?;

        let link_id = graph
            .traversal_mut()
            .add_e(from, to)
            .inspect(|link| tx.log(PendingOperation::AddLink(link.id.clone())))
            .first()
            .map(|link| link.id.clone())
            .ok_or_else(|| Error::GraphError("failed to create link after validation".into()))?;
        Ok((link_id, channel))
    })?;

    tracing::debug!(
        link_id = %link_id,
        channel = channel.as_str(),
        "connect assigned channel"
    );

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidConnect {
            link_id: link_id.to_string(),
            from_port,
            to_port,
        }),
    );

    PUBSUB.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
    );

    Ok(link_id)
}

/// Core implementation for disconnect - takes owned Arcs for 'static lifetime.
async fn disconnect_impl(compiler: Arc<Compiler>, link_id: LinkUniqueId) -> Result<()> {
    let link_info = compiler.scope(|graph, tx| {
        let (from_value, to_value) = graph
            .traversal()
            .e(&link_id)
            .first()
            .map(|l| (l.from_port(), l.to_port()))
            .ok_or_else(|| Error::NotFound(format!("Link '{}' not found", link_id)))?;

        let info = (
            OutputLinkPortRef::new(from_value.processor_id.clone(), to_value.port_name.clone()),
            InputLinkPortRef::new(to_value.processor_id.clone(), to_value.port_name.clone()),
        );

        if let Some(link) = graph.traversal_mut().e(&link_id).first_mut() {
            link.insert(PendingDeletionComponent);
        }

        tx.log(PendingOperation::RemoveLink(link_id.clone()));

        Ok::<_, Error>(info)
    })?;

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

// =============================================================================
// RuntimeOperations Implementation
// =============================================================================

impl RuntimeOperations for Runner {
    // =========================================================================
    // Async Methods (delegate to _impl functions)
    // =========================================================================

    fn add_processor_async(&self, spec: ProcessorSpec) -> BoxFuture<'_, Result<ProcessorUniqueId>> {
        let compiler = Arc::clone(&self.compiler);
        Box::pin(async move {
            // Lazy plugin auto-discovery: if the referenced processor type
            // isn't registered, discover + load the providing package from
            // streamlib_modules/ on first reference. A recoverable failure is
            // threaded into add_processor_impl (the runtime keeps running).
            let lazy_error = self
                .lazily_load_provider_for_processor_type(&spec.name)
                .await;
            add_processor_impl(compiler, spec, lazy_error).await
        })
    }

    fn remove_processor_async(&self, processor_id: ProcessorUniqueId) -> BoxFuture<'_, Result<()>> {
        let compiler = Arc::clone(&self.compiler);
        Box::pin(remove_processor_impl(compiler, processor_id))
    }

    fn connect_async(
        &self,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
    ) -> BoxFuture<'_, Result<LinkUniqueId>> {
        let compiler = Arc::clone(&self.compiler);
        Box::pin(connect_impl(compiler, from, to))
    }

    fn disconnect_async(&self, link_id: LinkUniqueId) -> BoxFuture<'_, Result<()>> {
        let compiler = Arc::clone(&self.compiler);
        Box::pin(disconnect_impl(compiler, link_id))
    }

    fn to_json_async(&self) -> BoxFuture<'_, Result<serde_json::Value>> {
        Box::pin(async move { Runner::to_json(self) })
    }

    // =========================================================================
    // Sync Methods (variant-aware blocking strategy)
    // =========================================================================

    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
        match &self.tokio_runtime_variant {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => {
                // The async path runs the lazy plugin-discovery step itself.
                rt.block_on(self.add_processor_async(spec))
            }
            TokioRuntimeVariant::ExternalTokioHandle(handle) => {
                // Can't `.await` the borrowing lazy-load future in the spawned
                // 'static task, so drive lazy discovery to completion here
                // (blocking) and hand the outcome to the owned add_processor_impl.
                let lazy_error =
                    self.lazily_load_provider_for_processor_type_blocking(&spec.name);
                let compiler = Arc::clone(&self.compiler);
                let (tx, rx) = std::sync::mpsc::channel();
                handle.spawn(async move {
                    let result = add_processor_impl(compiler, spec, lazy_error).await;
                    let _ = tx.send(result);
                });
                rx.recv()
                    .map_err(|_| Error::Runtime("Task channel closed".into()))?
            }
        }
    }

    fn remove_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        match &self.tokio_runtime_variant {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => {
                rt.block_on(self.remove_processor_async(processor_id.clone()))
            }
            TokioRuntimeVariant::ExternalTokioHandle(handle) => {
                let compiler = Arc::clone(&self.compiler);
                let processor_id = processor_id.clone();
                let (tx, rx) = std::sync::mpsc::channel();
                handle.spawn(async move {
                    let result = remove_processor_impl(compiler, processor_id).await;
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
                let (tx, rx) = std::sync::mpsc::channel();
                handle.spawn(async move {
                    let result = connect_impl(compiler, from, to).await;
                    let _ = tx.send(result);
                });
                rx.recv()
                    .map_err(|_| Error::Runtime("Task channel closed".into()))?
            }
        }
    }

    fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()> {
        match &self.tokio_runtime_variant {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => {
                rt.block_on(self.disconnect_async(link_id.clone()))
            }
            TokioRuntimeVariant::ExternalTokioHandle(handle) => {
                let compiler = Arc::clone(&self.compiler);
                let link_id = link_id.clone();
                let (tx, rx) = std::sync::mpsc::channel();
                handle.spawn(async move {
                    let result = disconnect_impl(compiler, link_id).await;
                    let _ = tx.send(result);
                });
                rx.recv()
                    .map_err(|_| Error::Runtime("Task channel closed".into()))?
            }
        }
    }

    // =========================================================================
    // Introspection
    // =========================================================================

    fn to_json(&self) -> Result<serde_json::Value> {
        Runner::to_json(self)
    }
}

#[cfg(test)]
mod channel_wire_bound_tests {
    // The channel-name grammar (streamlib_idents) and the fixed PortKey wire
    // capacity (streamlib_ipc_types) live in separate crates that cannot depend
    // on each other, so the max-length bound is duplicated. This engine layer
    // depends on both and is the one place that reconciles them: a channel name
    // that passes the grammar must always fit the wire.
    #[test]
    fn channel_name_bound_matches_port_key_wire_capacity() {
        assert_eq!(
            streamlib_idents::MAX_CHANNEL_NAME_BYTES,
            streamlib_ipc_types::PortKey::MAX_NAME_BYTES,
            "channel-name grammar bound drifted from the PortKey wire capacity"
        );
    }

    #[test]
    fn generated_connect_channel_name_fits_the_wire() {
        // A generated name — including the hash-suffixed over-budget path —
        // must construct a PortKey without the fallible constructor rejecting it.
        let long = "verylongprocessorname".repeat(4);
        let channel =
            streamlib_idents::connect_channel_name(&long, "outputport", "sinkproc", "inputport")
                .expect("a grammar-legal set of endpoints must produce a channel name");
        streamlib_ipc_types::PortKey::new(channel.as_str())
            .expect("a grammar-legal channel name must always fit the PortKey wire");
    }
}
