// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use crate::core::delegates::{ProcessorDelegate, SchedulerDelegate, SchedulingStrategy};
use crate::core::error::{Result, StreamError};
use crate::core::execution::run_processor_loop;
use crate::core::graph::{
    Graph, GraphNodeWithComponents, LinkOutputToProcessorWriterAndReader,
    ProcessorInstanceComponent, ProcessorPauseGateComponent, ProcessorUniqueId,
    ShutdownChannelComponent, StateComponent, ThreadHandleComponent,
};
use crate::core::processors::ProcessorState;

pub(crate) fn start_processor(
    processor_delegate: &Arc<dyn ProcessorDelegate>,
    scheduler: &Arc<dyn SchedulerDelegate>,
    property_graph: &mut Graph,
    processor_id: impl AsRef<str>,
) -> Result<()> {
    let processor_id = processor_id.as_ref();
    // Check if already has a thread (already running)
    let has_thread = property_graph
        .traversal()
        .v(processor_id)
        .first()
        .map(|n| n.has::<ThreadHandleComponent>())
        .unwrap_or(false);

    if has_thread {
        return Ok(());
    }

    // Get the node to determine scheduling strategy
    let node = property_graph
        .traversal()
        .v(processor_id)
        .first()
        .ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
        })?;

    let strategy = scheduler.scheduling_strategy(node);
    tracing::info!(
        "[{}] Starting with strategy: {}",
        processor_id,
        strategy.description()
    );

    // Delegate callback: will_start
    processor_delegate.will_start(processor_id)?;

    match strategy {
        SchedulingStrategy::DedicatedThread { priority, name } => {
            spawn_dedicated_thread(property_graph, processor_id, priority, name)?;
        }
        SchedulingStrategy::MainThread => {
            spawn_dedicated_thread(
                property_graph,
                processor_id,
                crate::core::delegates::ThreadPriority::Normal,
                None,
            )?;
        }
        SchedulingStrategy::WorkStealingPool => {
            spawn_dedicated_thread(
                property_graph,
                processor_id,
                crate::core::delegates::ThreadPriority::Normal,
                None,
            )?;
        }
        SchedulingStrategy::Lightweight => {
            spawn_dedicated_thread(
                property_graph,
                processor_id,
                crate::core::delegates::ThreadPriority::Normal,
                None,
            )?;
        }
    }

    // Delegate callback: did_start
    processor_delegate.did_start(processor_id)?;

    Ok(())
}

fn spawn_dedicated_thread(
    property_graph: &mut Graph,
    processor_id: impl AsRef<str>,
    priority: crate::core::delegates::ThreadPriority,
    _name: Option<String>,
) -> Result<()> {
    let processor_id = processor_id.as_ref();
    // Get mutable node and extract all required data
    let node = property_graph
        .traversal_mut()
        .v(processor_id)
        .first_mut()
        .ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
        })?;

    let instance = node.get::<ProcessorInstanceComponent>().ok_or_else(|| {
        StreamError::Runtime(format!(
            "Processor '{}' has no ProcessorInstance",
            processor_id
        ))
    })?;
    let processor_arc = instance.0.clone();

    let state = node.get::<StateComponent>().ok_or_else(|| {
        StreamError::Runtime(format!(
            "Processor '{}' has no StateComponent",
            processor_id
        ))
    })?;
    let state_arc = state.0.clone();

    let channel = node.get_mut::<ShutdownChannelComponent>().ok_or_else(|| {
        StreamError::Runtime(format!(
            "Processor '{}' has no ShutdownChannel",
            processor_id
        ))
    })?;
    let shutdown_rx = channel.take_receiver().ok_or_else(|| {
        StreamError::Runtime(format!(
            "Processor '{}' shutdown receiver already taken",
            processor_id
        ))
    })?;

    let writer_and_reader = node
        .get_mut::<LinkOutputToProcessorWriterAndReader>()
        .ok_or_else(|| {
            StreamError::Runtime(format!(
                "Processor '{}' has no LinkOutputToProcessorWriterAndReader",
                processor_id
            ))
        })?;
    let message_reader = writer_and_reader.take_reader().ok_or_else(|| {
        StreamError::Runtime(format!(
            "Processor '{}' message reader already taken",
            processor_id
        ))
    })?;

    let pause_gate = node.get::<ProcessorPauseGateComponent>().ok_or_else(|| {
        StreamError::Runtime(format!(
            "Processor '{}' has no ProcessorPauseGate",
            processor_id
        ))
    })?;
    let pause_gate_inner = pause_gate.clone_inner();

    // Get execution config
    let exec_config = processor_arc.lock().execution_config();

    // Update state to Running
    *state_arc.lock() = ProcessorState::Running;

    // Spawn the thread
    let id_clone: ProcessorUniqueId = processor_id.into();
    let thread = std::thread::Builder::new()
        .name(format!("processor-{}", processor_id))
        .spawn(move || {
            // Apply thread priority (platform-specific)
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            if let Err(e) = crate::apple::thread_priority::apply_thread_priority(priority) {
                tracing::warn!(
                    "[{}] Failed to apply {:?} thread priority: {}",
                    id_clone,
                    priority,
                    e
                );
            }

            run_processor_loop(
                id_clone,
                processor_arc,
                shutdown_rx,
                message_reader,
                state_arc,
                pause_gate_inner,
                exec_config,
            );
        })
        .map_err(|e| StreamError::Runtime(format!("Failed to spawn thread: {}", e)))?;

    // Attach thread handle - need to get node again since we consumed the reference
    let node = property_graph
        .traversal_mut()
        .v(processor_id)
        .first_mut()
        .ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
        })?;
    node.insert(ThreadHandleComponent(thread));

    Ok(())
}
