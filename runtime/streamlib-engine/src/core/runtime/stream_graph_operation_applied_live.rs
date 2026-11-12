// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashSet;
use streamlib_project_repository::{
    STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocument, StreamGraphDocumentLink,
    StreamGraphDocumentLinkEnd, StreamGraphOperation,
};

use uuid::Uuid;

use super::LoadedStreamInThisRuntime;
use super::operations::RuntimeOperations;
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::graph::{
    LinkUniqueId, ProcessorUniqueId, cast_exposed_name_to_url_safe,
    the_default_name_a_new_node_takes,
};
use crate::core::processors::ProcessorSpec;
use crate::core::pubsub::RuntimeEvent;
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};

impl LoadedStreamInThisRuntime {
    /// Apply `graph_operation` to this stream's live graph, each node by its UUID, answering
    /// it as applied: an added node under the name it took. An operation on a node no
    /// processor runs as, a fragment's above all, or on a labelled group or a canvas note, is applied to the
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
                .with_pins_and_studio_fields(node.pins.clone(), node.studio.to_map())
                .with_exposed_config_fields(node.exposed_config_fields.clone())
                .with_node_uuid(node.id);
                if !node.name.is_empty() {
                    spec = spec.with_display_name(node.name.clone());
                }
                let a_node_held_the_uuid_before_the_add =
                    self.the_processor_of_the_node(node.id).is_ok();
                let added = match self.add_processor_async(spec).await {
                    Ok(added) => added,
                    Err(refusal) => {
                        if !a_node_held_the_uuid_before_the_add {
                            self.take_out_the_node_a_refused_add_left(node.id, &refusal)
                                .await;
                        }
                        return Err(refusal);
                    }
                };
                node.name = added.name;
                for link in &links {
                    if let Err(refusal) = self.connect_the_link(link).await {
                        self.take_out_the_node_a_refused_add_left(node.id, &refusal)
                            .await;
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
                self.update_processor_config_compiled_right_away(
                    &self.the_processor_of_the_node(node)?,
                    &to,
                )
                .await?;
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
                self.set_studio_fields(&self.the_processor_of_the_node(node)?, to.to_map())?;
                Ok(StreamGraphOperation::SetStudioFields { node, from, to })
            }
            StreamGraphOperation::RefreshFragment { .. }
            | StreamGraphOperation::AddGroup { .. }
            | StreamGraphOperation::SetGroupFields { .. }
            | StreamGraphOperation::RemoveGroup { .. }
            | StreamGraphOperation::FoldIntoFragment { .. }
            | StreamGraphOperation::UnfoldFragment { .. }
            | StreamGraphOperation::EditInsideFragment { .. }
            | StreamGraphOperation::AddNote { .. }
            | StreamGraphOperation::SetNoteFields { .. }
            | StreamGraphOperation::RemoveNote { .. } => {
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
            | StreamGraphOperation::RemoveGroup { .. }
            | StreamGraphOperation::FoldIntoFragment { .. }
            | StreamGraphOperation::UnfoldFragment { .. }
            | StreamGraphOperation::EditInsideFragment { .. }
            | StreamGraphOperation::AddNote { .. }
            | StreamGraphOperation::SetNoteFields { .. }
            | StreamGraphOperation::RemoveNote { .. } => true,
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
        self.name_a_node_added_inside_a_fragment(&graph_document, &mut graph_operation)?;
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

    /// Name the node an edit inside a fragment adds, as an add to the stream's own level names
    /// it, against the names of the fragment's level.
    fn name_a_node_added_inside_a_fragment(
        &self,
        graph_document: &StreamGraphDocument,
        graph_operation: &mut StreamGraphOperation,
    ) -> Result<()> {
        let (level, edit) = graph_operation.the_edit_at_its_own_level_mut();
        let StreamGraphOperation::AddNode { node, .. } = edit else {
            return Ok(());
        };
        if level.is_empty() {
            return Ok(());
        }
        if !node.name.is_empty() {
            node.name = cast_exposed_name_to_url_safe(&node.name)?.into_owned();
            return Ok(());
        }
        let names_at_the_level: HashSet<String> = graph_document
            .the_level_inside(&level)
            .map_err(Error::GraphError)?
            .nodes()
            .into_iter()
            .map(|held| held.name)
            .collect();
        node.name = the_default_name_a_new_node_takes(
            self.node_types_this_stream_resolves(),
            &ProcessorClassImportPath::new(&node.node_type).map_err(|refusal| {
                Error::GraphError(format!("node {} `type`: {refusal}", node.id))
            })?,
            |candidate| names_at_the_level.contains(candidate),
        )?;
        Ok(())
    }

    /// Take out of the live graph the node of UUID `node` a refused add left in it, if it left
    /// one, so a refused operation leaves the live graph as it was. An add of a type nothing
    /// resolves places its node with nothing to compile, so that node is dropped as it stands;
    /// one the compile could not build is removed as any node is.
    async fn take_out_the_node_a_refused_add_left(&self, node: Uuid, refusal: &Error) {
        let Ok(processor_id) = self.the_processor_of_the_node(node) else {
            return;
        };
        if matches!(refusal, Error::UnknownProcessorType { .. }) {
            self.compiler.scope(|graph, _tx| {
                graph.traversal_mut().v(&processor_id).drop();
            });
            self.publish_on_this_streams_topic(RuntimeEvent::RuntimeDidRemoveProcessor {
                processor_id,
            });
            self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
            return;
        }
        if let Err(not_removed) = self.remove_processor_async(processor_id).await {
            tracing::error!(
                %node,
                %not_removed,
                "could not take out a node whose add was refused"
            );
        }
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
