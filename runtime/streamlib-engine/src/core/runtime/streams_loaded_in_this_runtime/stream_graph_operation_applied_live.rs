// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use streamlib_project_repository::{
    GRAPH_INTERFACE_FAMILY_CONFIG_KEY, STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocument,
    StreamGraphDocumentLink, StreamGraphDocumentLinkEnd, StreamGraphOperation,
    is_a_graph_interface_node_type,
};

use uuid::Uuid;

use crate::core::descriptors::{NodeClassImportPath, PortDescriptor};
use crate::core::graph::legacy_config_of_a_node::{
    ConfigAndPinValues, LegacyConfigOfANodeType, node_spec_holding_the_input_values,
};
use crate::core::graph::{
    ALinkConnected, LinkUniqueId, NodeRunId, WhyTheNodeLeaves, cast_exposed_name_to_url_safe,
    the_default_name_a_new_node_takes, the_verdict_on_a_link_between_typed_ends,
};
use crate::core::pubsub::RuntimeEvent;
use crate::core::runtime::LoadedStreamInThisRuntime;
use crate::core::runtime::streams_loaded_in_this_runtime::live_graph_reconciled_to_a_graph_document::{
    LinksAReconcileMakes, RunningLinkBetweenTwoNodesByUuid,
};
use crate::core::compiler::compiler_ops::node_interpreter_describe::a_node_interpreter_describes;
use crate::core::nodes::NodeTypesOneStreamResolves;
use crate::core::runtime::operations::{
    ANodeTypesPort, RuntimeOperations, WhatALiveConnectMakesOfALink,
};
use crate::core::runtime::operations_runtime::{
    ALinkEndTheGraphHolds, ANodeTypesPortToJudge, ConfigReferencesOfTheAddedNode, connect_impl,
    describe_the_type_an_add_names_when_it_must, the_verdict_connect_takes_on_a_link,
    the_verdicts_connect_takes_on_links_with_node_types_ports,
};
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, PortDirection, Result};

impl LoadedStreamInThisRuntime {
    /// Apply `graph_operation` to the live graph, each port it names under its current name,
    /// answering it as applied.
    ///
    /// The live graph holds each link once, under its ports' current names, while a document
    /// may hold one link under several names a port had: a link it already holds is not
    /// connected again, and a disconnect naming a port by a name it had leaves the graph as it
    /// is when it holds no such link. A request to connect a link its document already holds is
    /// refused before it reaches here, so only an undo or a redo of an edit meets a link held.
    /// A disconnect of a link held set aside, made nowhere, takes it off the graph as drawn.
    ///
    /// The operation is applied as graph edits held for one commit, joined to those the running
    /// task holds, so it is judged by the graph it leaves and taken back whole when refused.
    pub(crate) async fn apply_a_stream_graph_operation_to_the_live_graph(
        &self,
        graph_operation: StreamGraphOperation,
    ) -> Result<StreamGraphOperation> {
        self.in_one_commit(
            self.apply_a_stream_graph_operation_within_the_graph_edits_held(graph_operation),
        )
        .await
    }

    async fn apply_a_stream_graph_operation_within_the_graph_edits_held(
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
        if self.the_operation_is_drawn_before_the_live_graph_takes_it(&graph_operation) {
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
                if let Err(refusal) = self.connect_each_link_of_an_added_node(&links).await {
                    self.take_out_the_node_a_refused_add_left(node.id, &refusal)
                        .await;
                    return Err(refusal);
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
                    None if Self::the_link_by_node_uuid(&link)
                        .is_some_and(|link| self.forget_the_link_set_aside(&link)) => {}
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

    /// Whether `graph_operation` is applied to the graph as drawn first and the live graph
    /// reconciled to it: an edit that places or touches a node no node runs as, and one that
    /// places, takes out or changes a `GraphInput` or `GraphOutput`, whose interface the graph as
    /// drawn declares and `graph` renders from. A link to a running interface node is the live
    /// graph's own, and `graph` renders it from there.
    fn the_operation_is_drawn_before_the_live_graph_takes_it(
        &self,
        graph_operation: &StreamGraphOperation,
    ) -> bool {
        let no_node_runs_as = |node: Uuid| self.the_node_of_the_node(node).is_err();
        let held_only_as_drawn =
            |node: Uuid| no_node_runs_as(node) || self.the_node_is_a_graph_interface_node(node);
        match graph_operation {
            StreamGraphOperation::AddNode { node, links } => {
                node.fragment.is_some()
                    || node.node_type == STREAM_FRAGMENT_NODE_TYPE
                    || is_a_graph_interface_node_type(&node.node_type)
                    || links
                        .iter()
                        .flat_map(|link| [link.source.node, link.target.node])
                        .any(|end| end != node.id && no_node_runs_as(end))
            }
            StreamGraphOperation::Connect { link } | StreamGraphOperation::Disconnect { link } => {
                no_node_runs_as(link.source.node) || no_node_runs_as(link.target.node)
            }
            StreamGraphOperation::RemoveNode { node, .. } => {
                is_a_graph_interface_node_type(&node.node_type) || held_only_as_drawn(node.id)
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
            other => other.node().is_some_and(held_only_as_drawn),
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

    /// Whether the node `node` runs as a `GraphInput` or a `GraphOutput`, whose interface the
    /// graph as drawn declares, so an edit to it is drawn before the live graph takes it.
    fn the_node_is_a_graph_interface_node(&self, node: Uuid) -> bool {
        self.compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .v(())
                .iter()
                .find(|held| held.node_uuid == node)
                .is_some_and(|held| is_a_graph_interface_node_type(held.node_type.as_str()))
        })
    }

    fn the_node_of_the_node(&self, node: Uuid) -> Result<NodeRunId> {
        self.compiler.scope_reading(|graph| {
            let standing = || graph.traversal().v(()).iter().filter(|held| held.stands());
            if let Some(held) = standing().find(|held| held.node_uuid == node) {
                return Ok(held.id.clone());
            }
            let held: Vec<String> = standing()
                .map(|held| format!("{} `{}`", held.node_uuid, held.display_name))
                .collect();
            Err(Error::GraphError(format!(
                "stream `{}` holds no node {node}. It holds: {}",
                self.stream_id(),
                if held.is_empty() {
                    "no node".to_string()
                } else {
                    held.join(", ")
                }
            )))
        })
    }

    /// Connect each of `links`, those of a node an add made, the rule a reader that runs
    /// reactively sets left to the close of the graph edits held for one commit, so the order
    /// they are listed in never matters.
    async fn connect_each_link_of_an_added_node(
        &self,
        links: &[StreamGraphDocumentLink],
    ) -> Result<()> {
        for link in links {
            let (from, to) = self.the_ends_of_the_link(link)?;
            connect_impl(
                self.live_graph_change(),
                from,
                to,
                ALinkConnected::MadeNow,
                self.the_connect_an_edit_makes(),
            )
            .await?
            .connected()?;
        }
        Ok(())
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
        let link = self
            .the_link_under_the_current_port_names(link)
            .ok_or_else(|| {
                Error::GraphError(
                    "a connect read under its ports' current names is no longer a connect".into(),
                )
            })?;
        let connect = StreamGraphOperation::Connect { link: link.clone() };
        if self.the_operation_is_drawn_before_the_live_graph_takes_it(&connect) {
            StreamGraphDocument::from_value(self.to_json()?)
                .map_err(Error::GraphError)?
                .apply(&connect)
                .map_err(Error::GraphError)?;
            return Ok(WhatALiveConnectMakesOfALink::JudgesItOnlyAsItIsMade);
        }
        let (from, to) = self.the_ends_of_the_link(&link)?;
        the_verdict_connect_takes_on_a_link(&self.compiler, from, to)
            .map(|verdict| WhatALiveConnectMakesOfALink::Makes(verdict.map(Box::new)))
    }

    /// The verdict of `link`, between two nodes the live graph runs, held or not, each node by
    /// its UUID and each port under its current name, by its two typed ends, a convert node's
    /// output judged by the type it gives under the settings it holds; `None` when either end is
    /// no typed port of a node that runs.
    pub(crate) fn the_verdict_of_a_link_between_two_running_nodes(
        &self,
        link: &StreamGraphDocumentLink,
    ) -> Option<crate::core::descriptors::PinLinkVerdict> {
        let link = self.the_link_under_the_current_port_names(link)?;
        let (from, to) = self.the_ends_of_the_link(&link).ok()?;
        self.compiler.scope_reading(|graph| {
            let conversions = graph.node_types_this_stream_resolves().pin_conversions();
            the_verdict_on_a_link_between_typed_ends(graph, &from, &to, &conversions)
        })
    }

    /// The verdict of a link from the output `from` names to the input `to` names, judged by the
    /// ports this stream resolves each node type to declare, among the conversions it resolves,
    /// before either node runs; `None` when either port is untyped or not declared.
    pub(crate) fn the_verdict_of_a_link_between_two_node_types(
        &self,
        from: crate::core::runtime::PortOfANodeOfAType<'_>,
        to: crate::core::runtime::PortOfANodeOfAType<'_>,
    ) -> Option<crate::core::descriptors::PinLinkVerdict> {
        let node_types = self.node_types_this_stream_resolves();
        let ports_of = |node_type: &str| {
            node_types
                .port_info(&crate::core::descriptors::NodeClassImportPath::new(node_type).ok()?)
        };
        let (output_ports, input_ports) = (ports_of(from.node_type)?, ports_of(to.node_type)?);
        crate::core::graph::the_verdict_of_a_link_between_declared_ports(
            (from.node_name, output_ports.output_named(from.port)?),
            (to.node_name, input_ports.input_named(to.port)?),
            &node_types.pin_conversions(),
        )
    }

    /// `link`, each node by its UUID, with each of its ports under its current name; `None` when
    /// read so it is no longer a link.
    fn the_link_under_the_current_port_names(
        &self,
        link: &StreamGraphDocumentLink,
    ) -> Option<StreamGraphDocumentLink> {
        match self.the_operation_under_the_current_port_names(StreamGraphOperation::Connect {
            link: link.clone(),
        }) {
            StreamGraphOperation::Connect { link } => Some(link),
            _ => None,
        }
    }

    /// What a connect of each link between `held`, an end of the live graph by its node's UUID
    /// and of `held_direction`, and one of `node_types_ports` would make of it, in their order,
    /// were a node of the type placed: judged as [`Self::what_a_connect_makes_of_the_link`]
    /// judges a link, and made nowhere, the graph's lock taken once for them all. Nothing is
    /// described or registered: a type the stream has not described yet is judged only once a
    /// node of it is placed, as is a port of a type whose nodes run as no node of their own, a
    /// fragment or a graph interface naming no family, and each link with an end at a node the
    /// graph as drawn holds and no node runs as.
    pub(crate) fn what_a_connect_makes_of_links_with_node_types_ports(
        &self,
        held: &StreamGraphDocumentLinkEnd,
        held_direction: PortDirection,
        node_types_ports: &[ANodeTypesPort<'_>],
    ) -> Result<Vec<Result<WhatALiveConnectMakesOfALink>>> {
        let node_run_id = match self.the_node_of_the_node(held.node) {
            Ok(node_run_id) => node_run_id,
            Err(refusal) => {
                let drawn =
                    StreamGraphDocument::from_value(self.to_json()?).map_err(Error::GraphError)?;
                if !drawn.nodes().iter().any(|node| node.id == held.node) {
                    return Err(refusal);
                }
                return Ok(node_types_ports
                    .iter()
                    .map(|_| Ok(WhatALiveConnectMakesOfALink::JudgesItOnlyAsItIsMade))
                    .collect());
            }
        };
        let held = match held_direction {
            PortDirection::Output => {
                ALinkEndTheGraphHolds::Output(OutputLinkPortRef::new(node_run_id, &held.port))
            }
            PortDirection::Input => {
                ALinkEndTheGraphHolds::Input(InputLinkPortRef::new(node_run_id, &held.port))
            }
        };
        let node_types = self.node_types_this_stream_resolves();
        let mut answers = Vec::with_capacity(node_types_ports.len());
        let mut to_judge = Vec::new();
        let mut ports_of_each_type = HashMap::new();
        for port in node_types_ports {
            match how_a_node_types_port_is_judged(
                node_types,
                &mut ports_of_each_type,
                port,
                held_direction.opposite(),
            ) {
                HowANodeTypesPortIsJudged::Answered(answer) => answers.push(Some(answer)),
                HowANodeTypesPortIsJudged::OnAPlacedNode(port) => {
                    to_judge.push(*port);
                    answers.push(None);
                }
            }
        }
        if to_judge.is_empty() {
            return Ok(answers.into_iter().flatten().collect());
        }
        let mut verdicts = the_verdicts_connect_takes_on_links_with_node_types_ports(
            &self.compiler,
            held,
            to_judge,
        )?
        .into_iter();
        let mut in_their_order = Vec::with_capacity(answers.len());
        for answer in answers {
            in_their_order.push(match answer {
                Some(answer) => answer,
                None => verdicts
                    .next()
                    .ok_or_else(|| {
                        Error::GraphError("a node type's port was judged no verdict".into())
                    })?
                    .map(|verdict| WhatALiveConnectMakesOfALink::Makes(verdict.map(Box::new))),
            });
        }
        Ok(in_their_order)
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

    /// `link` with each end its node's UUID and each port name cast; `None` when a name does
    /// not cast.
    fn the_link_by_node_uuid(
        link: &StreamGraphDocumentLink,
    ) -> Option<RunningLinkBetweenTwoNodesByUuid> {
        RunningLinkBetweenTwoNodesByUuid::of_drawn_link(
            link.source.node,
            &link.source.port,
            link.target.node,
            &link.target.port,
        )
        .ok()
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
                    graph.stands(held)
                        && held.source.port_name() == source_port
                        && *held.target.node_run_id() == target
                        && held.target.port_name() == target_port
                })
                .map(|held| held.id.clone())
        }))
    }
}

/// How a link with a port of a node type is judged: answered before any node is placed, or on a
/// node of the type placed for it.
enum HowANodeTypesPortIsJudged {
    /// Answered as it is: a type whose nodes run as no node of their own, one the stream has not
    /// described yet, and one no node interpreter describes and the stream does not resolve.
    Answered(Result<WhatALiveConnectMakesOfALink>),
    /// Judged on a node of the type placed for it.
    OnAPlacedNode(Box<ANodeTypesPortToJudge>),
}

/// How a link with `port`, of `direction`, is judged in a stream resolving `node_types`; the
/// ports of `direction` each type declares are read from its descriptor once, into
/// `ports_of_each_type`, and shared by every port of the type judged.
fn how_a_node_types_port_is_judged(
    node_types: &NodeTypesOneStreamResolves,
    ports_of_each_type: &mut HashMap<NodeClassImportPath, Arc<[PortDescriptor]>>,
    port: &ANodeTypesPort<'_>,
    direction: PortDirection,
) -> HowANodeTypesPortIsJudged {
    let node_type = match NodeClassImportPath::new(port.node_type) {
        Ok(node_type) => node_type,
        Err(refusal) => {
            return HowANodeTypesPortIsJudged::Answered(Err(Error::GraphError(format!(
                "node type `{}`: {refusal}",
                port.node_type
            ))));
        }
    };
    if node_type.as_str() == STREAM_FRAGMENT_NODE_TYPE
        || is_a_graph_interface_node_naming_no_family(node_type.as_str(), &serde_json::Map::new())
    {
        return HowANodeTypesPortIsJudged::Answered(Ok(
            WhatALiveConnectMakesOfALink::JudgesItOnlyAsItIsMade,
        ));
    }
    let ports_of_its_direction = match ports_of_each_type.get(&node_type) {
        Some(ports) => Arc::clone(ports),
        None => {
            let Some(descriptor) = node_types.descriptor(&node_type) else {
                return HowANodeTypesPortIsJudged::Answered(
                    if a_node_interpreter_describes(&node_type) {
                        Ok(WhatALiveConnectMakesOfALink::JudgesItOnlyOnceItsNodeTypeIsDescribed)
                    } else {
                        Err(Error::UnknownNodeType { ident: node_type })
                    },
                );
            };
            let ports: Arc<[PortDescriptor]> = match direction {
                PortDirection::Input => descriptor.inputs,
                PortDirection::Output => descriptor.outputs,
            }
            .into();
            ports_of_each_type.insert(node_type.clone(), Arc::clone(&ports));
            ports
        }
    };
    HowANodeTypesPortIsJudged::OnAPlacedNode(Box::new(ANodeTypesPortToJudge::of(
        node_types,
        node_type,
        ports_of_its_direction,
        port.port,
    )))
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
