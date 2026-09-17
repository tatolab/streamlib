// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use parking_lot::RwLock;

use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphNodeWithComponents, ProcessorInstanceComponent, ProcessorUniqueId,
};

/// What handing a processor a configuration came to, short of a refusal.
#[must_use]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ProcessorConfigUpdateOutcome {
    /// The processor took the configuration and its graph node records it.
    TakenAndRecordedOnTheNode,
    /// The processor left the graph before the update reached it.
    ProcessorNoLongerInTheGraph,
}

/// Hand a running processor `config_to_apply`, and record it on the processor's
/// graph node only once the processor has taken it.
///
/// A refusal leaves the node's configuration as it was and returns what the
/// processor refused with.
pub(crate) fn apply_processor_config_update(
    graph: &RwLock<Graph>,
    processor_id: &ProcessorUniqueId,
    config_to_apply: serde_json::Value,
) -> Result<ProcessorConfigUpdateOutcome> {
    let processor_instance = {
        let graph = graph.read();
        let Some(node) = graph.traversal().v(processor_id).first() else {
            tracing::warn!("[CONFIG] Processor {} not found in graph", processor_id);
            return Ok(ProcessorConfigUpdateOutcome::ProcessorNoLongerInTheGraph);
        };
        node.get::<ProcessorInstanceComponent>()
            .map(|instance| instance.0.clone())
            .ok_or_else(|| {
                Error::ProcessorNotFound(format!(
                    "Processor '{}' not found for config update",
                    processor_id
                ))
            })?
    };

    processor_instance
        .lock()
        .apply_config_json(&config_to_apply)?;

    if let Some(node) = graph.write().traversal_mut().v(processor_id).first_mut() {
        node.set_config(config_to_apply);
    }
    Ok(ProcessorConfigUpdateOutcome::TakenAndRecordedOnTheNode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph::ProcessorInstanceWithItsOutOfProcessLinkWiring;
    use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
    use crate::core::test_support::{MockOutputOnlyProcessor, ensure_test_mocks_registered};

    /// A graph holding one instantiated mock processor that declares no
    /// config, so it takes an empty configuration and refuses any key.
    fn graph_with_an_instantiated_processor_that_declares_no_config()
    -> (RwLock<Graph>, ProcessorUniqueId) {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        let node = graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                MockOutputOnlyProcessor::processor_class_import_path(),
                serde_json::Value::Null,
            ))
            .first_mut()
            .expect("the mock is registered");
        let processor_instance = PROCESSOR_REGISTRY
            .create(node)
            .expect("the mock constructs from its node");
        ProcessorInstanceWithItsOutOfProcessLinkWiring::from(processor_instance).attach_to(node);
        let processor_id = node.id.clone();
        (RwLock::new(graph), processor_id)
    }

    fn config_on_the_node(
        graph: &RwLock<Graph>,
        processor_id: &ProcessorUniqueId,
    ) -> Option<serde_json::Value> {
        graph
            .read()
            .traversal()
            .v(processor_id)
            .first()
            .expect("the node is in the graph")
            .config
            .clone()
    }

    /// Fail-without-fix: the config was written onto the node when the update
    /// was requested, so a processor refusing it left `graph` reporting a
    /// configuration it never took.
    #[test]
    fn a_configuration_the_processor_refuses_never_reaches_its_graph_node() {
        let (graph, processor_id) = graph_with_an_instantiated_processor_that_declares_no_config();

        let refusal =
            apply_processor_config_update(&graph, &processor_id, serde_json::json!({"gain": 3}))
                .expect_err("a processor declaring no config refuses a key");

        assert!(
            refusal.to_string().contains("gain"),
            "the refusal names the key that had nowhere to go: {refusal}"
        );
        assert_eq!(
            config_on_the_node(&graph, &processor_id),
            Some(serde_json::Value::Null)
        );
    }

    #[test]
    fn a_configuration_the_processor_takes_is_recorded_on_its_graph_node() {
        let (graph, processor_id) = graph_with_an_instantiated_processor_that_declares_no_config();

        let outcome = apply_processor_config_update(&graph, &processor_id, serde_json::json!({}))
            .expect("an empty configuration is taken");

        assert_eq!(
            outcome,
            ProcessorConfigUpdateOutcome::TakenAndRecordedOnTheNode
        );

        assert_eq!(
            config_on_the_node(&graph, &processor_id),
            Some(serde_json::json!({}))
        );
    }

    #[test]
    fn an_update_for_a_processor_no_longer_in_the_graph_is_passed_over_not_counted_as_taken() {
        let (graph, _processor_id) = graph_with_an_instantiated_processor_that_declares_no_config();

        let outcome = apply_processor_config_update(
            &graph,
            &"P-removed-before-the-commit".into(),
            serde_json::json!({}),
        )
        .expect("a processor that left the graph is not an error");

        assert_eq!(
            outcome,
            ProcessorConfigUpdateOutcome::ProcessorNoLongerInTheGraph
        );
    }
}
