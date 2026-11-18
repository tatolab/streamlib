// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashSet;
use streamlib_project_repository::{
    GRAPH_INTERFACE_FAMILY_CONFIG_KEY, STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocument,
    StreamGraphDocumentLink, StreamGraphOperation, is_a_graph_interface_node_type,
};

use uuid::Uuid;

use crate::core::descriptors::NodeClassImportPath;
use crate::core::graph::legacy_config_of_a_node::{
    ConfigAndPinValues, LegacyConfigOfANodeType, node_spec_holding_the_input_values,
};
use crate::core::graph::{
    LinkUniqueId, NodeRunId, WhyTheNodeLeaves, cast_exposed_name_to_url_safe,
    the_default_name_a_new_node_takes, the_verdict_of_two_typed_ends,
};
use crate::core::pubsub::RuntimeEvent;
use crate::core::runtime::LoadedStreamInThisRuntime;
use crate::core::runtime::streams_loaded_in_this_runtime::live_graph_reconciled_to_a_graph_document::LinksAReconcileMakes;
use crate::core::runtime::operations::{RuntimeOperations, WhatALiveConnectMakesOfALink};
use crate::core::runtime::operations_runtime::{
    ConfigReferencesOfTheAddedNode, describe_the_type_an_add_names_when_it_must,
    the_verdict_connect_takes_on_a_link,
};
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};

impl LoadedStreamInThisRuntime {
    /// Apply `graph_operation` to the live graph, each port it names under its current name,
    /// answering it as applied.
    ///
    /// The live graph holds each link once, under its ports' current names, while a document
    /// may hold one link under several names a port had: a link it already holds is not
    /// connected again, and a disconnect naming a port by a name it had leaves the graph as it
    /// is when it holds no such link. A request to connect a link its document already holds is
    /// refused before it reaches here, so only an undo or a redo of an edit meets a link held.
    pub(crate) async fn apply_a_stream_graph_operation_to_the_live_graph(
        &self,
        graph_operation: StreamGraphOperation,
    ) -> Result<StreamGraphOperation> {
        let link_as_given = match &graph_operation {
            StreamGraphOperation::Disconnect { link } => Some(link.clone()),
            _ => None,
        };
        let graph_operation = self.the_operation_under_the_current_port_names(graph_operation);
        let disconnects_a_port_by_a_name_it_had = match (&graph_operation, &link_as_given) {
            (StreamGraphOperation::Disconnect { link }, Some(as_given)) => link != as_given,
            _ => false,
        };
        if self.the_operation_touches_a_node_no_node_runs_as(&graph_operation) {
            return self
                .apply_a_stream_graph_operation_by_reconciling_the_live_graph(graph_operation)
                .await;
        }
        match graph_operation {
            StreamGraphOperation::AddNode { node, links } => {
                let node_type = NodeClassImportPath::new(&node.node_type).map_err(|refusal| {
                    Error::GraphError(format!("node {} `type`: {refusal}", node.id))
                })?;
                describe_the_type_an_add_names_when_it_must(
                    &self.node_interpreter_launch_record,
                    &node_type,
                    self.lend_directory(),
                    ConfigReferencesOfTheAddedNode::CheckedAgainstTheGraphItJoins,
                )
                .await?;
                let (mut node, links) =
                    self.the_added_node_under_the_current_port_names(node, links);
                let mut spec = node_spec_holding_the_input_values(
                    self.node_types_this_stream_resolves(),
                    node_type,
                    &node.input_values,
                    node.studio.to_map(),
                )
                .with_exposed_config_fields(node.exposed_config_fields.clone())
                .with_node_uuid(node.id);
                if !node.name.is_empty() {
                    spec = spec.with_display_name(node.name.clone());
                }
                let a_node_held_the_uuid_before_the_add =
                    self.the_node_of_the_node(node.id).is_ok();
                let added = match self.add_node_async(spec).await {
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
                let node_run_id = self.the_node_of_the_node(node.id)?;
                self.remove_node_async(node_run_id).await?;
                Ok(StreamGraphOperation::RemoveNode { node, links })
            }
            StreamGraphOperation::Connect { link } => {
                if self.the_link_held_between(&link)?.is_none() {
                    self.connect_the_link(&link).await?;
                }
                Ok(StreamGraphOperation::Connect { link })
            }
            StreamGraphOperation::Disconnect { link } => {
                match self.the_link_held_between(&link)? {
                    Some(link_id) => self.disconnect_async(link_id).await?,
                    None if disconnects_a_port_by_a_name_it_had => {}
                    None => {
                        return Err(Error::GraphError(format!(
                            "stream `{}` holds no link {}/{} -> {}/{}",
                            self.stream_id(),
                            link.source.node,
                            link.source.port,
                            link.target.node,
                            link.target.port
                        )));
                    }
                }
                Ok(StreamGraphOperation::Disconnect { link })
            }
            StreamGraphOperation::SetNodeConfig { node, from, to } => {
                self.set_the_config_and_pin_values_of_the_node(node, &from, &to)
                    .await?;
                Ok(StreamGraphOperation::SetNodeConfig { node, from, to })
            }
            StreamGraphOperation::SetPinValue {
                node,
                input,
                from,
                to,
            } => {
                self.set_pin_value(&self.the_node_of_the_node(node)?, &input, to.clone())?;
                Ok(StreamGraphOperation::SetPinValue {
                    node,
                    input,
                    from,
                    to,
                })
            }
            StreamGraphOperation::SetNodeInputValue {
                node,
                input,
                from,
                to,
            } => {
                self.set_one_input_value_of_the_node(node, &input, to.clone())
                    .await?;
                Ok(StreamGraphOperation::SetNodeInputValue {
                    node,
                    input,
                    from,
                    to,
                })
            }
            StreamGraphOperation::RenameNode { node, from, to } => {
                let renamed = self.rename_node(&self.the_node_of_the_node(node)?, &to)?;
                Ok(StreamGraphOperation::RenameNode {
                    node,
                    from,
                    to: renamed,
                })
            }
            StreamGraphOperation::SetStudioFields { node, from, to } => {
                self.set_studio_fields(&self.the_node_of_the_node(node)?, to.to_map())?;
                Ok(StreamGraphOperation::SetStudioFields { node, from, to })
            }
            StreamGraphOperation::RefreshFragment { .. }
            | StreamGraphOperation::AddGroup { .. }
            | StreamGraphOperation::SetGroupFields { .. }
            | StreamGraphOperation::RemoveGroup { .. }
            | StreamGraphOperation::FoldIntoFragment { .. }
            | StreamGraphOperation::UnfoldFragment { .. }
            | StreamGraphOperation::CarryTheLinksOntoThePortsOfAPlacedStream { .. }
            | StreamGraphOperation::AddNote { .. }
            | StreamGraphOperation::SetNoteFields { .. }
            | StreamGraphOperation::RemoveNote { .. } => {
                self.apply_a_stream_graph_operation_by_reconciling_the_live_graph(graph_operation)
                    .await
            }
            StreamGraphOperation::SetExposedConfigFields { node, from, to } => {
                self.set_exposed_config_fields(&self.the_node_of_the_node(node)?, to.clone())?;
                Ok(StreamGraphOperation::SetExposedConfigFields { node, from, to })
            }
        }
    }

    fn the_operation_touches_a_node_no_node_runs_as(
        &self,
        graph_operation: &StreamGraphOperation,
    ) -> bool {
        let no_node_runs_as = |node: Uuid| self.the_node_of_the_node(node).is_err();
        match graph_operation {
            StreamGraphOperation::AddNode { node, links } => {
                node.fragment.is_some()
                    || node.node_type == STREAM_FRAGMENT_NODE_TYPE
                    || is_a_graph_interface_node_naming_no_family(
                        &node.node_type,
                        &node.input_values,
                    )
                    || links
                        .iter()
                        .flat_map(|link| [link.source.node, link.target.node])
                        .any(|end| end != node.id && no_node_runs_as(end))
            }
            StreamGraphOperation::Connect { link } | StreamGraphOperation::Disconnect { link } => {
                no_node_runs_as(link.source.node) || no_node_runs_as(link.target.node)
            }
            StreamGraphOperation::RefreshFragment { .. }
            | StreamGraphOperation::AddGroup { .. }
            | StreamGraphOperation::SetGroupFields { .. }
            | StreamGraphOperation::RemoveGroup { .. }
            | StreamGraphOperation::FoldIntoFragment { .. }
            | StreamGraphOperation::UnfoldFragment { .. }
            | StreamGraphOperation::CarryTheLinksOntoThePortsOfAPlacedStream { .. }
            | StreamGraphOperation::AddNote { .. }
            | StreamGraphOperation::SetNoteFields { .. }
            | StreamGraphOperation::RemoveNote { .. } => true,
            other => other.node().is_some_and(no_node_runs_as),
        }
    }

    /// Reconcile the live graph to itself as drawn, so each stream fragment places the stream it
    /// references as that stream's document, or its import's, is on disk now.
    pub(crate) async fn reconcile_each_stream_fragment_to_the_stream_it_references(
        &self,
    ) -> Result<()> {
        let graph_as_drawn = self.to_json()?;
        self.reconcile_the_live_graph_to_the_graph_document(
            graph_as_drawn,
            LinksAReconcileMakes::EachAsASavedGraphsLink,
        )
        .await
    }

    /// Apply `graph_operation` to this stream's graph as drawn and reconcile the live graph to
    /// the result, the live graph reconciled back to what it was drawn as when that is refused.
    async fn apply_a_stream_graph_operation_by_reconciling_the_live_graph(
        &self,
        mut graph_operation: StreamGraphOperation,
    ) -> Result<StreamGraphOperation> {
        let graph_as_drawn = self.to_json()?;
        let mut graph_document =
            StreamGraphDocument::from_value(graph_as_drawn.clone()).map_err(Error::GraphError)?;
        if let StreamGraphOperation::AddNode { node, .. } = &mut graph_operation {
            if node.name.is_empty() {
                if node.fragment.is_some() || node.node_type == STREAM_FRAGMENT_NODE_TYPE {
                    return Err(Error::GraphError(format!(
                        "node {} is added with no name, and a fragment is placed under one",
                        node.id
                    )));
                }
                let names_at_the_level: HashSet<String> = graph_document
                    .nodes()
                    .into_iter()
                    .map(|held| held.name)
                    .collect();
                node.name = self.the_name_a_node_of_the_type_takes_at_a_level(
                    &node.node_type,
                    &names_at_the_level,
                )?;
            }
            node.name = cast_exposed_name_to_url_safe(&node.name)?.into_owned();
        }
        graph_document
            .apply(&graph_operation)
            .map_err(Error::GraphError)?;
        if let Err(refusal) = self
            .reconcile_the_live_graph_to_the_graph_document(
                graph_document.to_value(),
                LinksAReconcileMakes::ThoseTheLiveGraphKeptThoughConnectRefusesThem,
            )
            .await
        {
            if let Err(not_restored) = self
                .reconcile_the_live_graph_to_the_graph_document(
                    graph_as_drawn,
                    LinksAReconcileMakes::EachAsASavedGraphsLink,
                )
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

    /// The name a node of type `node_type` added with none takes at a level whose nodes are
    /// named `names_at_the_level`, as this stream resolves the type.
    pub(crate) fn the_name_a_node_of_the_type_takes_at_a_level(
        &self,
        node_type: &str,
        names_at_the_level: &HashSet<String>,
    ) -> Result<String> {
        the_default_name_a_new_node_takes(
            self.node_types_this_stream_resolves(),
            &NodeClassImportPath::new(node_type).map_err(|refusal| {
                Error::GraphError(format!("node type `{node_type}`: {refusal}"))
            })?,
            |candidate| names_at_the_level.contains(candidate),
        )
    }

    /// Take out of the live graph the node of UUID `node` a refused add left in it, if it left
    /// one, so a refused operation leaves the live graph as it was. An add of a type nothing
    /// resolves places its node with nothing to compile, so that node is dropped as it stands;
    /// one the compile could not build is removed as any node is.
    async fn take_out_the_node_a_refused_add_left(&self, node: Uuid, refusal: &Error) {
        let Ok(node_run_id) = self.the_node_of_the_node(node) else {
            return;
        };
        if matches!(refusal, Error::UnknownNodeType { .. }) {
            self.compiler.scope(|graph, _tx| {
                if !graph.drop_the_node(&node_run_id, WhyTheNodeLeaves::Removed) {
                    tracing::warn!("`{node_run_id}`, which a refused add left, was not dropped");
                }
            });
            self.publish_on_this_streams_topic(RuntimeEvent::RuntimeDidRemoveNode { node_run_id });
            self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
            return;
        }
        if let Err(not_removed) = self.remove_node_async(node_run_id).await {
            tracing::error!(
                %node,
                %not_removed,
                "could not take out a node whose add was refused"
            );
        }
    }

    /// Set what a `set_node_config` from `from` to `to` sets on the running node `node`: the
    /// config its type takes whole out of `to`, and the value on each of its inputs `to` names,
    /// each input `from` names and `to` does not cleared. A `to` that is no object is the config.
    async fn set_the_config_and_pin_values_of_the_node(
        &self,
        node: Uuid,
        from: &serde_json::Value,
        to: &serde_json::Value,
    ) -> Result<()> {
        let node_run_id = self.the_node_of_the_node(node)?;
        let serde_json::Value::Object(to_object) = to else {
            return self
                .update_node_config_compiled_right_away(&node_run_id, to)
                .await;
        };
        let node_type = self.compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .v(&node_run_id)
                .first()
                .map(|running| running.node_type.clone())
                .ok_or_else(|| Error::NodeNotFound(node_run_id.to_string()))
        })?;
        let legacy_config =
            LegacyConfigOfANodeType::of(self.node_types_this_stream_resolves(), &node_type);
        let ConfigAndPinValues { config, pin_values } = legacy_config.split(to_object);
        let pin_values_cleared: Vec<String> = from
            .as_object()
            .map(|from_object| legacy_config.split(from_object).pin_values)
            .unwrap_or_default()
            .into_iter()
            .map(|(input, _)| input)
            .filter(|input| !pin_values.contains_key(input))
            .collect();
        self.update_node_config_compiled_right_away(&node_run_id, &config)
            .await?;
        for input in pin_values_cleared {
            self.set_pin_value(&node_run_id, &input, None)?;
        }
        for (input, value) in pin_values {
            self.set_pin_value(&node_run_id, &input, Some(value))?;
        }
        Ok(())
    }

    /// Set the input value `input` of `node` to `value`, `None` taking it off: a setting its
    /// type holds in its config is set in the config the node holds once each config update
    /// already logged for it is applied, any other on its input.
    async fn set_one_input_value_of_the_node(
        &self,
        node: Uuid,
        input: &str,
        value: Option<serde_json::Value>,
    ) -> Result<()> {
        let node_run_id = self.the_node_of_the_node(node)?;
        let node_type = self.compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .v(&node_run_id)
                .first()
                .map(|running| running.node_type.clone())
                .ok_or_else(|| Error::NodeNotFound(node_run_id.to_string()))
        })?;
        let legacy_config =
            LegacyConfigOfANodeType::of(self.node_types_this_stream_resolves(), &node_type);
        if !legacy_config.is_a_config_field(input) {
            return self.set_pin_value(&node_run_id, input, value);
        }
        self.update_node_config_built_from_the_one_it_holds(&node_run_id, |config_held| {
            let mut config = match config_held.unwrap_or_default() {
                serde_json::Value::Object(config) => config,
                serde_json::Value::Null => serde_json::Map::new(),
                other => {
                    return Err(Error::Configuration(format!(
                        "node {node} holds the config {other}, which is no object, so its \
                         setting `{input}` cannot be set"
                    )));
                }
            };
            match value {
                Some(value) => config.insert(input.to_owned(), value),
                None => config.remove(input),
            };
            Ok(serde_json::Value::Object(config))
        })
        .await
    }

    fn the_node_of_the_node(&self, node: Uuid) -> Result<NodeRunId> {
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
        let (from, to) = self.the_ends_of_the_link(link)?;
        self.connect_async(from, to).await
    }

    /// What a connect of `link`, each node by its UUID, would make of it, judged as
    /// [`Self::apply_a_stream_graph_operation_to_the_live_graph`] judges a connect and made
    /// nowhere: a link reaching a node no node runs as is refused only as the graph as drawn
    /// refuses it, since the live graph judges the rest of it only as it is reconciled.
    pub(crate) fn what_a_connect_makes_of_the_link(
        &self,
        link: &StreamGraphDocumentLink,
    ) -> Result<WhatALiveConnectMakesOfALink> {
        let connect =
            self.the_operation_under_the_current_port_names(StreamGraphOperation::Connect {
                link: link.clone(),
            });
        if self.the_operation_touches_a_node_no_node_runs_as(&connect) {
            StreamGraphDocument::from_value(self.to_json()?)
                .map_err(Error::GraphError)?
                .apply(&connect)
                .map_err(Error::GraphError)?;
            return Ok(WhatALiveConnectMakesOfALink::JudgesItOnlyAsItIsMade);
        }
        let StreamGraphOperation::Connect { link } = &connect else {
            return Err(Error::GraphError(
                "a connect read under its ports' current names is no longer a connect".into(),
            ));
        };
        let (from, to) = self.the_ends_of_the_link(link)?;
        the_verdict_connect_takes_on_a_link(&self.compiler, from, to)
            .map(|verdict| WhatALiveConnectMakesOfALink::Makes(verdict.map(Box::new)))
    }

    /// The verdict of `link`, a link the live graph holds, each node by its UUID and each port
    /// under its current name, by its two typed ends; `None` when either end is no typed port of
    /// a node that runs.
    pub(crate) fn the_verdict_of_a_held_link(
        &self,
        link: &StreamGraphDocumentLink,
    ) -> Option<crate::core::descriptors::PinLinkVerdict> {
        let StreamGraphOperation::Connect { link } = self
            .the_operation_under_the_current_port_names(StreamGraphOperation::Connect {
                link: link.clone(),
            })
        else {
            return None;
        };
        let (from, to) = self.the_ends_of_the_link(&link).ok()?;
        self.compiler
            .scope_reading(|graph| the_verdict_of_two_typed_ends(graph, &from, &to))
    }

    /// The output and the input `link` names, each node by its UUID.
    fn the_ends_of_the_link(
        &self,
        link: &StreamGraphDocumentLink,
    ) -> Result<(OutputLinkPortRef, InputLinkPortRef)> {
        Ok((
            OutputLinkPortRef::new(
                self.the_node_of_the_node(link.source.node)?,
                &link.source.port,
            ),
            InputLinkPortRef::new(
                self.the_node_of_the_node(link.target.node)?,
                &link.target.port,
            ),
        ))
    }

    /// The live link `link` names by its ends; `None` when the live graph holds none, and
    /// refused when either end names a node it does not hold or a name that does not cast.
    fn the_link_held_between(
        &self,
        link: &StreamGraphDocumentLink,
    ) -> Result<Option<LinkUniqueId>> {
        let source = self.the_node_of_the_node(link.source.node)?;
        let target = self.the_node_of_the_node(link.target.node)?;
        let source_port = cast_exposed_name_to_url_safe(&link.source.port)?;
        let target_port = cast_exposed_name_to_url_safe(&link.target.port)?;
        Ok(self.compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .v(&source)
                .out_e()
                .iter()
                .find(|held| {
                    held.source.port_name() == source_port
                        && *held.target.node_run_id() == target
                        && held.target.port_name() == target_port
                })
                .map(|held| held.id.clone())
        }))
    }
}

/// Whether a node of `node_type` holding `config` is a `GraphInput` or `GraphOutput` whose config
/// names no family, so the family it carries is resolved with the graph's links.
fn is_a_graph_interface_node_naming_no_family(
    node_type: &str,
    input_values: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    is_a_graph_interface_node_type(node_type)
        && input_values
            .get(GRAPH_INTERFACE_FAMILY_CONFIG_KEY)
            .is_none_or(serde_json::Value::is_null)
}
