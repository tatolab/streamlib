// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor scheduling logic for the compiler.
//!
//! Resolves a [`SchedulingStrategy`] for a processor by reading the
//! [`ProcessorScheduling`] block off the registered [`ProcessorDescriptor`].
//! The block is sourced from the processor's `streamlib.yaml`; processors
//! that don't declare one fall through to [`ThreadPriority::Normal`].

use crate::core::execution::ThreadPriority;
use crate::core::graph::ProcessorNode;
use crate::core::processors::PROCESSOR_REGISTRY;

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
pub(crate) fn scheduling_strategy_for_processor(node: &ProcessorNode) -> SchedulingStrategy {
    let priority = PROCESSOR_REGISTRY
        .descriptor(&node.processor_type)
        .map(|d| d.scheduling.priority)
        .unwrap_or(ThreadPriority::Normal);

    SchedulingStrategy::DedicatedThread { priority }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::descriptors::{
        Org, Package, ProcessorDescriptor, ProcessorScheduling, SchemaIdent, SemVer, TypeName,
    };

    /// Build an ident whose short name is **deliberately neutral** —
    /// none of the substrings the pre-#722 heuristic matched on
    /// (`Camera`, `Display`, `Audio`, `Microphone`, `Speaker`, `Encoder`,
    /// `Decoder`, `H264`, `H265`, `Compositor`). That way mentally
    /// reverting `scheduling_strategy_for_processor` back to the old
    /// substring-match heuristic causes these tests to fail.
    fn ident(short: &str) -> SchemaIdent {
        SchemaIdent::new(
            Org::new("scheduling-test").unwrap(),
            Package::new("fixture").unwrap(),
            TypeName::new(short).unwrap(),
            SemVer::new(1, 0, 0),
        )
    }

    #[test]
    fn strategy_reads_priority_from_registered_descriptor() {
        let id = ident("Widgetron");
        let descriptor = ProcessorDescriptor::new(id.clone(), "fixture")
            .with_scheduling(ProcessorScheduling {
                priority: ThreadPriority::RealTime,
            });
        PROCESSOR_REGISTRY
            .register_descriptor_only(descriptor)
            .expect("fixture descriptor registers cleanly");

        let node = ProcessorNode::new(id, "fixture-node", None, vec![], vec![]);
        match scheduling_strategy_for_processor(&node) {
            SchedulingStrategy::DedicatedThread { priority } => {
                assert_eq!(priority, ThreadPriority::RealTime);
            }
        }
    }

    #[test]
    fn strategy_falls_back_to_normal_when_descriptor_missing() {
        let id = ident("UnregisteredFixtureProcessor");
        let node = ProcessorNode::new(id, "ghost-node", None, vec![], vec![]);
        match scheduling_strategy_for_processor(&node) {
            SchedulingStrategy::DedicatedThread { priority } => {
                assert_eq!(priority, ThreadPriority::Normal);
            }
        }
    }

    /// Smoke test that the macro → manifest → registry path produces a
    /// descriptor whose `scheduling.priority` matches what
    /// `libs/streamlib-engine/streamlib.yaml` declares for an in-tree
    /// processor. Mentally reverting the codegen emission in
    /// `streamlib-macros/src/codegen.rs::generate_descriptor_from_schema`
    /// or the engine yaml's `scheduling:` block makes this fail.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_display_descriptor_carries_declared_priority() {
        use crate::linux::processors::LinuxDisplayProcessor;
        let descriptor = PROCESSOR_REGISTRY
            .descriptor(&LinuxDisplayProcessor::schema_ident())
            .expect("Linux Display processor must be registered via inventory at test start");
        assert_eq!(descriptor.scheduling.priority, ThreadPriority::High);
    }
}
