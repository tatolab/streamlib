// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::operations::{BoxFuture, RuntimeOperations};
use super::StreamRuntime;
use crate::core::compiler::PendingOperation;
use crate::core::graph::{
    GraphEdgeWithComponents, GraphNodeWithComponents, LinkUniqueId, PendingDeletionComponent,
    ProcessorUniqueId,
};
use crate::core::processors::ProcessorSpec;
use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};
use crate::core::{InputLinkPortRef, OutputLinkPortRef, Result, StreamError};

impl RuntimeOperations for StreamRuntime {
    // =========================================================================
    // Async Methods (primary implementation)
    // =========================================================================

    fn add_processor_async(&self, spec: ProcessorSpec) -> BoxFuture<'_, Result<ProcessorUniqueId>> {
        Box::pin(async move {
            // Declare side effects upfront
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

            // Use compiler.scope() to access graph and transaction
            let processor_id = self.compiler.scope(|graph, tx| {
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

            // Notify listeners that graph changed (triggers commit via GraphChangeListener)
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
            );

            Ok(processor_id)
        })
    }

    fn remove_processor_async(&self, processor_id: ProcessorUniqueId) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Validate processor exists and mark for deletion
            self.compiler.scope(|graph, tx| {
                if !graph.traversal().v(&processor_id).exists() {
                    return Err(StreamError::ProcessorNotFound(processor_id.to_string()));
                }

                // Mark for soft-delete by adding PendingDeletion component
                if let Some(node) = graph.traversal_mut().v(&processor_id).first_mut() {
                    node.insert(PendingDeletionComponent);
                }

                // Queue operation for commit
                tx.log(PendingOperation::RemoveProcessor(processor_id.clone()));

                Ok(())
            })?;

            // Emit WillRemoveProcessor before the action
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillRemoveProcessor {
                    processor_id: processor_id.clone(),
                }),
            );

            // Emit DidRemoveProcessor after queueing (actual removal happens at commit)
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRemoveProcessor {
                    processor_id: processor_id.clone(),
                }),
            );

            // Notify listeners that graph changed (triggers commit via GraphChangeListener)
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
            );

            Ok(())
        })
    }

    fn connect_async(
        &self,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
    ) -> BoxFuture<'_, Result<LinkUniqueId>> {
        Box::pin(async move {
            // Capture for events before moving into add_e
            let from_processor = from.processor_id.clone();
            let from_port = from.port_name.clone();
            let to_processor = to.processor_id.clone();
            let to_port = to.port_name.clone();

            // Emit WillConnect before the action
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillConnect {
                    from_processor,
                    from_port: from_port.clone(),
                    to_processor,
                    to_port: to_port.clone(),
                }),
            );

            // Use compiler.scope() to access graph and transaction
            let link_id = self.compiler.scope(|graph, tx| {
                let id = graph
                    .traversal_mut()
                    .add_e(from, to)
                    .inspect(|link| tx.log(PendingOperation::AddLink(link.id.clone())))
                    .first()
                    .map(|link| link.id.clone())
                    .ok_or_else(|| StreamError::GraphError("failed to create link".into()))?;

                Ok::<_, StreamError>(id)
            })?;

            // Emit DidConnect after the action
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidConnect {
                    link_id: link_id.to_string(),
                    from_port,
                    to_port,
                }),
            );

            // Notify listeners that graph changed (triggers commit via GraphChangeListener)
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
            );

            Ok(link_id)
        })
    }

    fn disconnect_async(&self, link_id: LinkUniqueId) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Validate link exists and get info for events, then mark for deletion
            let link_info = self.compiler.scope(|graph, tx| {
                let (from_value, to_value) = graph
                    .traversal()
                    .e(&link_id)
                    .first()
                    .map(|l| (l.from_port(), l.to_port()))
                    .ok_or_else(|| {
                        StreamError::NotFound(format!("Link '{}' not found", link_id))
                    })?;

                let info = (
                    OutputLinkPortRef::new(
                        from_value.processor_id.clone(),
                        to_value.port_name.clone(),
                    ),
                    InputLinkPortRef::new(
                        to_value.processor_id.clone(),
                        to_value.port_name.clone(),
                    ),
                );

                // Mark for soft-delete by adding PendingDeletion component to link
                if let Some(link) = graph.traversal_mut().e(&link_id).first_mut() {
                    link.insert(PendingDeletionComponent);
                }

                // Queue operation for commit
                tx.log(PendingOperation::RemoveLink(link_id.clone()));

                Ok::<_, StreamError>(info)
            })?;

            // Emit WillDisconnect before the action
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimeWillDisconnect {
                    link_id: link_id.to_string(),
                    from_port: link_info.0.to_string(),
                    to_port: link_info.1.to_string(),
                }),
            );

            // Emit DidDisconnect after queueing (actual removal happens at commit)
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidDisconnect {
                    link_id: link_id.to_string(),
                    from_port: link_info.0.to_string(),
                    to_port: link_info.1.to_string(),
                }),
            );

            // Notify listeners that graph changed (triggers commit via GraphChangeListener)
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
            );

            Ok(())
        })
    }

    // =========================================================================
    // Sync Methods (convenience wrappers using block_on)
    // =========================================================================

    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
        self.tokio_runtime.block_on(self.add_processor_async(spec))
    }

    fn remove_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        self.tokio_runtime
            .block_on(self.remove_processor_async(processor_id.clone()))
    }

    fn connect(&self, from: OutputLinkPortRef, to: InputLinkPortRef) -> Result<LinkUniqueId> {
        self.tokio_runtime.block_on(self.connect_async(from, to))
    }

    fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()> {
        self.tokio_runtime
            .block_on(self.disconnect_async(link_id.clone()))
    }
}
