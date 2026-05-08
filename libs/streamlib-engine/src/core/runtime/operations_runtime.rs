// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use super::operations::{BoxFuture, RuntimeOperations};
use super::runtime::TokioRuntimeVariant;
use super::StreamRuntime;
use crate::core::compiler::{Compiler, PendingOperation};
use crate::core::graph::{
    GraphEdgeWithComponents, GraphNodeWithComponents, LinkUniqueId, PendingDeletionComponent,
    ProcessorUniqueId,
};
use crate::core::processors::ProcessorSpec;
use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};
use crate::core::{InputLinkPortRef, OutputLinkPortRef, Result, StreamError};

// =============================================================================
// Core Implementation Functions ('static async fns for spawn compatibility)
// =============================================================================

/// Core implementation for add_processor - takes owned Arcs for 'static lifetime.
async fn add_processor_impl(
    compiler: Arc<Compiler>,
    spec: ProcessorSpec,
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

    let processor_id = compiler.scope(|graph, tx| {
        graph
            .traversal_mut()
            .add_v(spec)
            .inspect(|node| emit_will_add(&node.id))
            .inspect(|node| tx.log(PendingOperation::AddProcessor(node.id.clone())))
            .inspect(|node| emit_did_add(&node.id))
            .first()
            .map(|node| node.id.clone())
            .ok_or_else(|| StreamError::GraphError("Could not create node".into()))
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
            return Err(StreamError::ProcessorNotFound(processor_id.to_string()));
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
            from_processor,
            from_port: from_port.clone(),
            to_processor,
            to_port: to_port.clone(),
        }),
    );

    let link_id = compiler.scope(|graph, tx| {
        let id = graph
            .traversal_mut()
            .add_e(from, to)
            .inspect(|link| tx.log(PendingOperation::AddLink(link.id.clone())))
            .first()
            .map(|link| link.id.clone())
            .ok_or_else(|| StreamError::GraphError("failed to create link".into()))?;

        Ok::<_, StreamError>(id)
    })?;

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
            .ok_or_else(|| StreamError::NotFound(format!("Link '{}' not found", link_id)))?;

        let info = (
            OutputLinkPortRef::new(from_value.processor_id.clone(), to_value.port_name.clone()),
            InputLinkPortRef::new(to_value.processor_id.clone(), to_value.port_name.clone()),
        );

        if let Some(link) = graph.traversal_mut().e(&link_id).first_mut() {
            link.insert(PendingDeletionComponent);
        }

        tx.log(PendingOperation::RemoveLink(link_id.clone()));

        Ok::<_, StreamError>(info)
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

impl RuntimeOperations for StreamRuntime {
    // =========================================================================
    // Async Methods (delegate to _impl functions)
    // =========================================================================

    fn add_processor_async(&self, spec: ProcessorSpec) -> BoxFuture<'_, Result<ProcessorUniqueId>> {
        let compiler = Arc::clone(&self.compiler);
        Box::pin(add_processor_impl(compiler, spec))
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
        Box::pin(async move { StreamRuntime::to_json(self) })
    }

    // =========================================================================
    // Sync Methods (variant-aware blocking strategy)
    // =========================================================================

    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
        match &self.tokio_runtime_variant {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => {
                rt.block_on(self.add_processor_async(spec))
            }
            TokioRuntimeVariant::ExternalTokioHandle(handle) => {
                let compiler = Arc::clone(&self.compiler);
                let (tx, rx) = std::sync::mpsc::channel();
                handle.spawn(async move {
                    let result = add_processor_impl(compiler, spec).await;
                    let _ = tx.send(result);
                });
                rx.recv()
                    .map_err(|_| StreamError::Runtime("Task channel closed".into()))?
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
                    .map_err(|_| StreamError::Runtime("Task channel closed".into()))?
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
                    .map_err(|_| StreamError::Runtime("Task channel closed".into()))?
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
                    .map_err(|_| StreamError::Runtime("Task channel closed".into()))?
            }
        }
    }

    // =========================================================================
    // Introspection
    // =========================================================================

    fn to_json(&self) -> Result<serde_json::Value> {
        StreamRuntime::to_json(self)
    }
}
