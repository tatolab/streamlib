// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use streamlib_project_repository::{
    STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocument, StreamGraphDocumentLink,
    StreamGraphDocumentLinkEnd, StreamGraphOperation,
};
use uuid::Uuid;

use super::LoadedStreamInThisRuntime;
use super::operations::RuntimeOperations;
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::graph::{LinkUniqueId, ProcessorUniqueId, cast_exposed_name_to_url_safe};
use crate::core::processors::ProcessorSpec;
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};

impl LoadedStreamInThisRuntime {
    /// Apply `graph_operation` to this stream's live graph, each node by its UUID, answering
    /// it as applied: an added node under the name it took. An operation on a node no
    /// processor runs as, a fragment's above all, or on a labelled group, is applied to the
    /// graph as drawn and the live graph reconciled to it.
    pub(crate) async fn apply_a_stream_graph_operation_to_the_live_graph(
        &self,
        graph_operation: StreamGraphOperation,
    ) -> Result<StreamGraphOperation> {
        if self.the_operation_touches_a_node_no_processor_runs_as(&graph_operation) {
            return self
                .apply_a_stream_graph_operation_by_reconciling_the_live_graph(graph_operation)
                .await;
        }
        match graph_operation {
            StreamGraphOperation::AddNode { mut node, links } => {
                let mut spec = ProcessorSpec::new(
                    ProcessorClassImportPath::new(&node.node_type).map_err(|refusal| {
                        Error::GraphError(format!("node {} `type`: {refusal}", node.id))
                    })?,
                    node.config.clone(),
                )
                .with_pins_and_studio_fields(node.pins.clone(), node.studio.clone())
                .with_exposed_config_fields(node.exposed_config_fields.clone())
                .with_node_uuid(node.id);
                if !node.name.is_empty() {
                    spec = spec.with_display_name(node.name.clone());
                }
                let added = self.add_processor_async(spec).await?;
                node.name = added.name;
                for link in &links {
                    if let Err(refusal) = self.connect_the_link(link).await {
                        if let Err(not_removed) = self
                            .remove_processor_async(added.processor_id.clone())
                            .await
                        {
                            tracing::error!(
                                node = %node.id,
                                %not_removed,
                                "could not take out a node whose links were refused"
                            );
                        }
                        return Err(refusal);
                    }
                }
                Ok(StreamGraphOperation::AddNode { node, links })
            }
            StreamGraphOperation::RemoveNode { node, links } => {
                let processor_id = self.the_processor_of_the_node(node.id)?;
                self.remove_processor_async(processor_id).await?;
                Ok(StreamGraphOperation::RemoveNode { node, links })
            }
            StreamGraphOperation::Connect { link } => {
                self.connect_the_link(&link).await?;
                Ok(StreamGraphOperation::Connect { link })
            }
            StreamGraphOperation::Disconnect { link } => {
                let link_id = self.the_link_between(&link)?;
                self.disconnect_async(link_id).await?;
                Ok(StreamGraphOperation::Disconnect { link })
            }
            StreamGraphOperation::SetNodeConfig { node, from, to } => {
                self.update_processor_config(&self.the_processor_of_the_node(node)?, &to)?;
                Ok(StreamGraphOperation::SetNodeConfig { node, from, to })
            }
            StreamGraphOperation::SetPinValue {
                node,
                input,
                from,
                to,
            } => {
                self.set_pin_value(&self.the_processor_of_the_node(node)?, &input, to.clone())?;
                Ok(StreamGraphOperation::SetPinValue {
                    node,
                    input,
                    from,
                    to,
                })
            }
            StreamGraphOperation::RenameNode { node, from, to } => {
                let renamed = self.rename_node(&self.the_processor_of_the_node(node)?, &to)?;
                Ok(StreamGraphOperation::RenameNode {
                    node,
                    from,
                    to: renamed,
                })
            }
            StreamGraphOperation::SetStudioFields { node, from, to } => {
                self.set_studio_fields(&self.the_processor_of_the_node(node)?, to.clone())?;
                Ok(StreamGraphOperation::SetStudioFields { node, from, to })
            }
            StreamGraphOperation::RefreshFragment { .. }
            | StreamGraphOperation::AddGroup { .. }
            | StreamGraphOperation::SetGroupFields { .. }
            | StreamGraphOperation::RemoveGroup { .. } => {
                self.apply_a_stream_graph_operation_by_reconciling_the_live_graph(graph_operation)
                    .await
            }
            StreamGraphOperation::SetExposedConfigFields { node, from, to } => {
                self.set_exposed_config_fields(&self.the_processor_of_the_node(node)?, to.clone())?;
                Ok(StreamGraphOperation::SetExposedConfigFields { node, from, to })
            }
        }
    }

    fn the_operation_touches_a_node_no_processor_runs_as(
        &self,
        graph_operation: &StreamGraphOperation,
    ) -> bool {
        let no_processor_runs_as = |node: Uuid| self.the_processor_of_the_node(node).is_err();
        match graph_operation {
            StreamGraphOperation::AddNode { node, links } => {
                node.fragment.is_some()
                    || node.node_type == STREAM_FRAGMENT_NODE_TYPE
                    || links
                        .iter()
                        .flat_map(|link| [link.source.node, link.target.node])
                        .any(|end| end != node.id && no_processor_runs_as(end))
            }
            StreamGraphOperation::Connect { link } | StreamGraphOperation::Disconnect { link } => {
                no_processor_runs_as(link.source.node) || no_processor_runs_as(link.target.node)
            }
            StreamGraphOperation::RefreshFragment { .. }
            | StreamGraphOperation::AddGroup { .. }
            | StreamGraphOperation::SetGroupFields { .. }
            | StreamGraphOperation::RemoveGroup { .. } => true,
            other => other.node().is_some_and(no_processor_runs_as),
        }
    }

    /// Reconcile the live graph to itself as drawn, so each stream fragment places the stream it
    /// references as that stream's document, or its import's, is on disk now.
    pub(crate) async fn reconcile_each_stream_fragment_to_the_stream_it_references(
        &self,
    ) -> Result<()> {
        let graph_as_drawn = self.to_json()?;
        self.reconcile_the_live_graph_to_the_graph_document(graph_as_drawn)
            .await
    }

    /// Apply `graph_operation` to this stream's graph as drawn and reconcile the live graph to
    /// the result, the live graph reconciled back to what it was drawn as when that is refused.
    async fn apply_a_stream_graph_operation_by_reconciling_the_live_graph(
        &self,
        mut graph_operation: StreamGraphOperation,
    ) -> Result<StreamGraphOperation> {
        if let StreamGraphOperation::AddNode { node, .. } = &mut graph_operation {
            if node.name.is_empty() {
                return Err(Error::GraphError(format!(
                    "node {} is added with no name, and a fragment is placed under one",
                    node.id
                )));
            }
            node.name = cast_exposed_name_to_url_safe(&node.name)?.into_owned();
        }
        let graph_as_drawn = self.to_json()?;
        let mut graph_document =
            StreamGraphDocument::from_value(graph_as_drawn.clone()).map_err(Error::GraphError)?;
        graph_document
            .apply(&graph_operation)
            .map_err(Error::GraphError)?;
        if let Err(refusal) = self
            .reconcile_the_live_graph_to_the_graph_document(graph_document.to_value())
            .await
        {
            if let Err(not_restored) = self
                .reconcile_the_live_graph_to_the_graph_document(graph_as_drawn)
                .await
            {
                tracing::error!(
                    stream = %self.stream_id(),
                    %not_restored,
                    "could not bring the live graph back to what it was drawn as after a \
                     refused change"
                );
            }
            return Err(refusal);
        }
        Ok(graph_operation)
    }

    /// The processor the node whose UUID is `node` runs as, refused naming the nodes the
    /// stream holds when none has it.
    fn the_processor_of_the_node(&self, node: Uuid) -> Result<ProcessorUniqueId> {
        self.compiler.scope(|graph, _tx| {
            let nodes: Vec<_> = graph.traversal().v(()).iter().collect();
            nodes
                .iter()
                .find(|held| held.node_uuid == node)
                .map(|held| held.id.clone())
                .ok_or_else(|| {
                    let held: Vec<String> = nodes
                        .iter()
                        .map(|held| format!("{} `{}`", held.node_uuid, held.display_name))
                        .collect();
                    Error::GraphError(format!(
                        "stream `{}` holds no node {node}. It holds: {}",
                        self.stream_id(),
                        if held.is_empty() {
                            "no node".to_string()
                        } else {
                            held.join(", ")
                        }
                    ))
                })
        })
    }

    async fn connect_the_link(&self, link: &StreamGraphDocumentLink) -> Result<LinkUniqueId> {
        let from = OutputLinkPortRef::new(
            self.the_processor_of_the_node(link.source.node)?,
            &link.source.port,
        );
        let to = InputLinkPortRef::new(
            self.the_processor_of_the_node(link.target.node)?,
            &link.target.port,
        );
        self.connect_async(from, to).await
    }

    /// The live link `link` names by its ends.
    fn the_link_between(&self, link: &StreamGraphDocumentLink) -> Result<LinkUniqueId> {
        let source = self.the_processor_of_the_node(link.source.node)?;
        let target = self.the_processor_of_the_node(link.target.node)?;
        let end = |end: &StreamGraphDocumentLinkEnd| format!("{}/{}", end.node, end.port);
        let source_port = cast_exposed_name_to_url_safe(&link.source.port)?;
        let target_port = cast_exposed_name_to_url_safe(&link.target.port)?;
        self.compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .v(&source)
                .out_e()
                .iter()
                .find(|held| {
                    held.source.port_name() == source_port
                        && *held.target.processor_id() == target
                        && held.target.port_name() == target_port
                })
                .map(|held| held.id.clone())
                .ok_or_else(|| {
                    Error::GraphError(format!(
                        "stream `{}` holds no link {} -> {}",
                        self.stream_id(),
                        end(&link.source),
                        end(&link.target)
                    ))
                })
        })
    }
}
