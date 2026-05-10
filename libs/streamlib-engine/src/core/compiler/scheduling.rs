// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor scheduling logic for the compiler.
//!
//! Resolves a [`SchedulingStrategy`] for a processor by reading the
//! [`ProcessorScheduling`] block off the registered [`ProcessorDescriptor`].
//! The block is sourced from the processor's `streamlib.yaml`; processors
//! that don't declare one fall through to [`ThreadPriority::Normal`] with
//! a `processor-{id}` thread name.

use crate::core::descriptors::ProcessorScheduling;
use crate::core::execution::ThreadPriority;
use crate::core::graph::ProcessorNode;
use crate::core::processors::PROCESSOR_REGISTRY;

/// How a processor should be scheduled at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulingStrategy {
    /// Dedicated OS thread with configurable priority.
    DedicatedThread {
        priority: ThreadPriority,
        /// Final thread name applied to the spawned OS thread (visible in
        /// `/proc/<pid>/task/*/comm`, `htop`, `tracing` per-thread spans).
        name: String,
    },
}

impl SchedulingStrategy {
    /// Get a human-readable description.
    pub fn description(&self) -> String {
        match self {
            SchedulingStrategy::DedicatedThread { priority, name } => {
                format!("dedicated thread '{}' ({})", name, priority.description())
            }
        }
    }
}

/// Build the OS thread name for a processor from its declared scheduling
/// block. Precedence: explicit `thread_name` → `kind`-prefixed default
/// (`{kind}-{id}`) → fallback `processor-{id}`.
///
/// `pthread_setname_np` truncates at 15 characters on Linux; callers should
/// keep ids and overrides short.
fn resolve_thread_name(scheduling: &ProcessorScheduling, node_id: &str) -> String {
    if let Some(name) = scheduling.thread_name.as_deref() {
        return name.to_string();
    }
    match scheduling.kind {
        Some(kind) => format!("{}-{}", kind.thread_name_prefix(), node_id),
        None => format!("processor-{}", node_id),
    }
}

/// Resolve the [`SchedulingStrategy`] for a processor by reading the
/// `scheduling:` block off its registered [`ProcessorDescriptor`]. When the
/// processor isn't registered (test fixtures, partially-built graphs) the
/// strategy falls back to [`ProcessorScheduling::default`] (`Normal` +
/// `processor-{id}`).
pub(crate) fn scheduling_strategy_for_processor(node: &ProcessorNode) -> SchedulingStrategy {
    let scheduling = PROCESSOR_REGISTRY
        .descriptor(&node.processor_type)
        .map(|d| d.scheduling.clone())
        .unwrap_or_default();

    let name = resolve_thread_name(&scheduling, node.id.as_str());

    SchedulingStrategy::DedicatedThread {
        priority: scheduling.priority,
        name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::descriptors::{
        Org, Package, ProcessorDescriptor, ProcessorSchedulingKind, SchemaIdent, SemVer, TypeName,
    };
    use crate::core::graph::ProcessorNode;

    #[test]
    fn resolve_thread_name_uses_explicit_override_when_present() {
        let scheduling = ProcessorScheduling {
            priority: ThreadPriority::High,
            kind: Some(ProcessorSchedulingKind::Camera),
            thread_name: Some("custom-thread".into()),
        };
        assert_eq!(resolve_thread_name(&scheduling, "node-7"), "custom-thread");
    }

    #[test]
    fn resolve_thread_name_uses_kind_prefix_when_no_override() {
        let scheduling = ProcessorScheduling {
            priority: ThreadPriority::High,
            kind: Some(ProcessorSchedulingKind::Audio),
            thread_name: None,
        };
        assert_eq!(resolve_thread_name(&scheduling, "node-7"), "audio-node-7");
    }

    #[test]
    fn resolve_thread_name_falls_back_to_processor_when_no_kind() {
        let scheduling = ProcessorScheduling::default();
        assert_eq!(
            resolve_thread_name(&scheduling, "node-7"),
            "processor-node-7"
        );
    }

    #[test]
    fn kind_prefix_round_trip() {
        assert_eq!(ProcessorSchedulingKind::Camera.thread_name_prefix(), "camera");
        assert_eq!(ProcessorSchedulingKind::Display.thread_name_prefix(), "display");
        assert_eq!(ProcessorSchedulingKind::Audio.thread_name_prefix(), "audio");
        assert_eq!(ProcessorSchedulingKind::Video.thread_name_prefix(), "video");
        assert_eq!(
            ProcessorSchedulingKind::Compositor.thread_name_prefix(),
            "compositor"
        );
    }

    /// Build an ident whose short name is **deliberately neutral** —
    /// none of the substrings the pre-#722 heuristic matched on
    /// (`Camera`, `Display`, `Audio`, `Microphone`, `Speaker`, `Encoder`,
    /// `Decoder`, `H264`, `H265`, `Compositor`). That way mentally
    /// reverting `scheduling_strategy_for_processor` back to the old
    /// substring-match heuristic causes these tests to fail — which is
    /// what the regression-locking rule requires.
    fn ident(short: &str) -> SchemaIdent {
        SchemaIdent::new(
            Org::new("scheduling-test").unwrap(),
            Package::new("fixture").unwrap(),
            TypeName::new(short).unwrap(),
            SemVer::new(1, 0, 0),
        )
    }

    #[test]
    fn strategy_reads_priority_and_kind_from_registered_descriptor() {
        // `Widgetron` contains none of the heuristic's substrings, so the
        // old code would have returned `Normal` + `processor-{id}` here.
        // The test asserts the new code returns `RealTime` + `audio-{id}`
        // sourced from the registered descriptor.
        let id = ident("Widgetron");
        let descriptor = ProcessorDescriptor::new(id.clone(), "fixture")
            .with_scheduling(ProcessorScheduling {
                priority: ThreadPriority::RealTime,
                kind: Some(ProcessorSchedulingKind::Audio),
                thread_name: None,
            });
        PROCESSOR_REGISTRY
            .register_descriptor_only(descriptor)
            .expect("fixture descriptor registers cleanly");

        let node = ProcessorNode::new(id, "fixture-node", None, vec![], vec![]);
        let expected_name = format!("audio-{}", node.id.as_str());
        let strategy = scheduling_strategy_for_processor(&node);
        match strategy {
            SchedulingStrategy::DedicatedThread { priority, name } => {
                assert_eq!(priority, ThreadPriority::RealTime);
                assert_eq!(name, expected_name);
            }
        }
    }

    #[test]
    fn strategy_falls_back_to_normal_when_descriptor_missing() {
        // Use an ident that intentionally isn't registered.
        let id = ident("UnregisteredFixtureProcessor");
        let node = ProcessorNode::new(id, "ghost-node", None, vec![], vec![]);
        let expected_name = format!("processor-{}", node.id.as_str());
        let strategy = scheduling_strategy_for_processor(&node);
        match strategy {
            SchedulingStrategy::DedicatedThread { priority, name } => {
                assert_eq!(priority, ThreadPriority::Normal);
                assert_eq!(name, expected_name);
            }
        }
    }

    /// Smoke test that the macro → manifest → registry path produces a
    /// descriptor whose `scheduling` block matches what
    /// `libs/streamlib-engine/streamlib.yaml` declares for an in-tree
    /// processor. Mentally reverting the codegen emission in
    /// `streamlib-macros/src/codegen.rs::generate_descriptor_from_schema`
    /// or the engine yaml's `scheduling:` block makes this fail.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_camera_descriptor_carries_declared_scheduling_block() {
        use crate::linux::processors::LinuxCameraProcessor;
        let descriptor = PROCESSOR_REGISTRY
            .descriptor(&LinuxCameraProcessor::schema_ident())
            .expect("Linux Camera processor must be registered via inventory at test start");
        assert_eq!(descriptor.scheduling.priority, ThreadPriority::High);
        assert_eq!(
            descriptor.scheduling.kind,
            Some(ProcessorSchedulingKind::Camera),
            "engine yaml declares kind: camera for Camera processor"
        );
        assert!(
            descriptor.scheduling.thread_name.is_none(),
            "engine yaml does not override thread_name; default `{{kind}}-{{id}}` applies"
        );
    }

    #[test]
    fn strategy_uses_explicit_thread_name_override_when_present() {
        let id = ident("DescriptorDrivenCustomThread");
        let descriptor = ProcessorDescriptor::new(id.clone(), "fixture")
            .with_scheduling(ProcessorScheduling {
                priority: ThreadPriority::High,
                kind: Some(ProcessorSchedulingKind::Camera),
                thread_name: Some("crt".into()),
            });
        PROCESSOR_REGISTRY
            .register_descriptor_only(descriptor)
            .expect("fixture descriptor registers cleanly");

        let node = ProcessorNode::new(id, "main-cam", None, vec![], vec![]);
        let strategy = scheduling_strategy_for_processor(&node);
        match strategy {
            SchedulingStrategy::DedicatedThread { priority, name } => {
                assert_eq!(priority, ThreadPriority::High);
                assert_eq!(name, "crt", "explicit thread_name overrides kind prefix");
            }
        }
    }
}
