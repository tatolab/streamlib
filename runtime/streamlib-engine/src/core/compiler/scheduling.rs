// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor scheduling logic for the compiler.
//!
//! Resolves a [`SchedulingStrategy`] for a processor by reading the
//! [`ProcessorScheduling`] block off the registered [`ProcessorDescriptor`].
//! The block is declared in the `#[processor]` attribute; processors that
//! don't declare one fall through to [`ThreadPriority::Normal`].

use crate::core::execution::ThreadPriority;
use crate::core::graph::ProcessorNode;
use crate::core::processors::NodeTypesOneStreamResolves;

/// How a processor should be scheduled at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulingStrategy {
    /// Dedicated OS thread with configurable priority.
    DedicatedThread { priority: ThreadPriority },
}

impl SchedulingStrategy {
    /// Get a human-readable description.
    pub fn description(&self) -> String {
        match self {
            SchedulingStrategy::DedicatedThread { priority } => {
                format!("dedicated thread ({})", priority.description())
            }
        }
    }
}

/// Resolve the [`SchedulingStrategy`] for a processor. Reads the
/// `priority` off its registered [`ProcessorDescriptor`] (defaults to
/// [`ThreadPriority::Normal`] when the processor isn't registered or has
/// no `scheduling:` block declared).
pub(crate) fn scheduling_strategy_for_processor(
    node_types: &NodeTypesOneStreamResolves,
    node: &ProcessorNode,
) -> SchedulingStrategy {
    let priority = node_types
        .descriptor(&node.processor_type)
        .map(|d| d.scheduling.priority)
        .unwrap_or(ThreadPriority::Normal);

    SchedulingStrategy::DedicatedThread { priority }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::descriptors::{
        ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor, ProcessorScheduling,
    };
    use crate::core::error::Error;
    use crate::core::processors::PROCESSOR_REGISTRY;

    /// Build a class import path that is **deliberately neutral** — no segment
    /// of it, module included, is one of the substrings the pre-#722 heuristic
    /// matched on (`Camera`, `Display`, `Audio`, `Microphone`, `Speaker`,
    /// `Encoder`, `Decoder`, `H264`, `H265`, `Compositor`). That way mentally
    /// reverting `scheduling_strategy_for_processor` back to the old
    /// substring-match heuristic causes these tests to fail. The whole path is
    /// what has to stay neutral now: the key is the whole string, and this
    /// module's own path is part of it.
    fn class_import_path(short: &str) -> ProcessorClassImportPath {
        ProcessorClassImportPath::new(format!("{}::{short}", module_path!())).unwrap()
    }

    #[test]
    fn strategy_reads_priority_from_registered_descriptor() {
        let path = class_import_path("Widgetron");
        let descriptor = ProcessorDescriptor::new(
            ProcessorClassShortName::new("Widgetron").unwrap(),
            path.clone(),
            "fixture",
        )
        .with_scheduling(ProcessorScheduling {
            priority: ThreadPriority::RealTime,
        });
        PROCESSOR_REGISTRY
            .register_descriptor_only(descriptor)
            .expect("fixture descriptor registers cleanly");

        let node = ProcessorNode::new(path, "fixture-node", None, vec![], vec![]);
        match scheduling_strategy_for_processor(&NodeTypesOneStreamResolves::new(), &node) {
            SchedulingStrategy::DedicatedThread { priority } => {
                assert_eq!(priority, ThreadPriority::RealTime);
            }
        }
    }

    #[test]
    fn strategy_reads_priority_from_a_type_described_in_the_stream() {
        let path = class_import_path("Gizmotron");
        let descriptor = ProcessorDescriptor::new(
            ProcessorClassShortName::new("Gizmotron").unwrap(),
            path.clone(),
            "fixture",
        )
        .with_scheduling(ProcessorScheduling {
            priority: ThreadPriority::RealTime,
        });
        let node_types = NodeTypesOneStreamResolves::new();
        node_types
            .register_a_type_described_in_this_streams_processor_interpreter(
                descriptor,
                Box::new(|_node| Err(Error::NotSupported("never constructed".into()))),
            )
            .expect("fixture descriptor registers cleanly");

        let node = ProcessorNode::new(path, "fixture-node", None, vec![], vec![]);
        match scheduling_strategy_for_processor(&node_types, &node) {
            SchedulingStrategy::DedicatedThread { priority } => {
                assert_eq!(priority, ThreadPriority::RealTime);
            }
        }
    }

    #[test]
    fn strategy_falls_back_to_normal_when_descriptor_missing() {
        let node = ProcessorNode::new(
            class_import_path("UnregisteredFixtureProcessor"),
            "ghost-node",
            None,
            vec![],
            vec![],
        );
        match scheduling_strategy_for_processor(&NodeTypesOneStreamResolves::new(), &node) {
            SchedulingStrategy::DedicatedThread { priority } => {
                assert_eq!(priority, ThreadPriority::Normal);
            }
        }
    }
}
