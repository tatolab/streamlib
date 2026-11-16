// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::execution::ThreadPriority;
use crate::core::graph::GraphNode;
use crate::core::nodes::NodeTypesOneStreamResolves;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SchedulingStrategy {
    /// Dedicated OS thread with configurable priority.
    DedicatedThread { priority: ThreadPriority },
}

impl SchedulingStrategy {
    /// Get a human-readable description.
    pub(crate) fn description(&self) -> String {
        match self {
            SchedulingStrategy::DedicatedThread { priority } => {
                format!("dedicated thread ({})", priority.description())
            }
        }
    }
}

pub(crate) fn scheduling_strategy_for_node(
    node_types: &NodeTypesOneStreamResolves,
    node: &GraphNode,
) -> SchedulingStrategy {
    let priority = node_types
        .descriptor(&node.node_type)
        .map(|d| d.scheduling.priority)
        .unwrap_or(ThreadPriority::Normal);

    SchedulingStrategy::DedicatedThread { priority }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::descriptors::{
        NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeScheduling,
    };
    use crate::core::error::Error;
    use crate::core::nodes::NODE_REGISTRY;

    fn class_import_path(short: &str) -> NodeClassImportPath {
        NodeClassImportPath::new(format!("{}::{short}", module_path!())).unwrap()
    }

    #[test]
    fn strategy_reads_priority_from_registered_descriptor() {
        let path = class_import_path("Widgetron");
        let descriptor = NodeDescriptor::new(
            NodeClassShortName::new("Widgetron").unwrap(),
            path.clone(),
            "fixture",
        )
        .with_scheduling(NodeScheduling {
            priority: ThreadPriority::RealTime,
        });
        NODE_REGISTRY
            .register_descriptor_only(descriptor)
            .expect("fixture descriptor registers cleanly");

        let node = GraphNode::new(path, None, "fixture-node", None, vec![], vec![]);
        match scheduling_strategy_for_node(&NodeTypesOneStreamResolves::new(), &node) {
            SchedulingStrategy::DedicatedThread { priority } => {
                assert_eq!(priority, ThreadPriority::RealTime);
            }
        }
    }

    #[test]
    fn strategy_reads_priority_from_a_type_described_in_the_stream() {
        let path = class_import_path("Gizmotron");
        let descriptor = NodeDescriptor::new(
            NodeClassShortName::new("Gizmotron").unwrap(),
            path.clone(),
            "fixture",
        )
        .with_scheduling(NodeScheduling {
            priority: ThreadPriority::RealTime,
        });
        let node_types = NodeTypesOneStreamResolves::new();
        node_types
            .register_a_type_described_in_this_streams_node_interpreter(
                descriptor,
                crate::core::nodes::HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(
                    Box::new(|_node| Err(Error::NotSupported("never constructed".into()))),
                ),
            )
            .expect("fixture descriptor registers cleanly");

        let node = GraphNode::new(path, None, "fixture-node", None, vec![], vec![]);
        match scheduling_strategy_for_node(&node_types, &node) {
            SchedulingStrategy::DedicatedThread { priority } => {
                assert_eq!(priority, ThreadPriority::RealTime);
            }
        }
    }

    #[test]
    fn strategy_falls_back_to_normal_when_descriptor_missing() {
        let node = GraphNode::new(
            class_import_path("UnregisteredFixtureNode"),
            None,
            "ghost-node",
            None,
            vec![],
            vec![],
        );
        match scheduling_strategy_for_node(&NodeTypesOneStreamResolves::new(), &node) {
            SchedulingStrategy::DedicatedThread { priority } => {
                assert_eq!(priority, ThreadPriority::Normal);
            }
        }
    }
}
