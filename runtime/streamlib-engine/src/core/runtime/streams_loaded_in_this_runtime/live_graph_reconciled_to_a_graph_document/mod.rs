// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::{Map, Value};
use streamlib_project_repository::STREAM_DOCUMENT_ASSET_KEY;
use uuid::Uuid;

use crate::core::compiler::compiler_ops::function_node_pool::{
    is_the_function_node_pool_type, the_links_carried_through_the_function_node_pool_of,
};
use crate::core::descriptors::{NodeClassImportPath, PinType};
use crate::core::graph::graph_snapshot::{
    GraphSnapshot, GraphSnapshotNode, GraphSnapshotWithGraphOnlyNodesResolved,
    LinkSetAsideByTheGraphsValidation,
};
use crate::core::graph::legacy_config_of_a_node::{
    ConfigAndPinValues, LegacyConfigOfANodeType, node_spec_holding_the_input_values,
};
use crate::core::graph::{
    ALinkConnected, Graph, GraphEdgeWithComponents, GraphNodeWithComponents, LinkUniqueId,
    NodeRunId, PendingDeletionComponent, cast_exposed_name_to_url_safe,
    the_pin_types_at_the_ends_of, the_pin_types_refuse_the_link,
};
use crate::core::logging::carrying_this_threads_project_log_route_entry;
use crate::core::pubsub::RuntimeEvent;
use crate::core::runtime::LoadedStreamInThisRuntime;
use crate::core::runtime::operations_runtime::{
    ALinkOfASavedGraphMade, ConfigReferencesOfTheAddedNode, add_node_impl,
    connect_a_link_of_a_saved_graph_unless_the_engine_cannot_wire_it, connect_impl,
    describe_the_type_an_add_names_when_it_must, disconnect_impl, remove_node_impl,
};
use crate::core::runtime::streams_loaded_in_this_runtime::loaded_stream::{
    SavedValuesOfAKeptNode, set_the_saved_values_of_each_kept_node_and_hand_them_over,
};
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};

/// One link of a running graph, each end its node's UUID and its port's cast name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::core::runtime) struct RunningLinkBetweenTwoNodesByUuid {
    pub(in crate::core::runtime) source_node: Uuid,
    pub(in crate::core::runtime) source_port: String,
    pub(in crate::core::runtime) target_node: Uuid,
    pub(in crate::core::runtime) target_port: String,
}

/// One link the live graph holds as it was given, as a reconcile compares it to the document's,
/// beside the links it is wired as, which a disconnect of it takes out together.
pub(in crate::core::runtime) struct LiveLinkAsGiven {
    pub(in crate::core::runtime) link_as_given: RunningLinkBetweenTwoNodesByUuid,
    pub(in crate::core::runtime) wired_as: Vec<LinkUniqueId>,
}

/// Which links a reconcile makes as a saved graph's, keeping one its pin types alone refuse when
/// its ports' families take it, and which it judges as `connect` judges a link made now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::core::runtime) enum LinksAReconcileMakes {
    /// Each link, the document being a saved graph: read from disk, or as the stream drew it.
    EachAsASavedGraphsLink,
    /// A link the live graph held when the reconcile began though its pin types refused it, a
    /// saved graph's link the reconcile remakes, as a saved graph's while its two ends' pin types
    /// are the ones it had then; any other as one an edit makes now.
    ThoseTheLiveGraphKeptThoughTheirPinTypesRefusedThem,
}

impl LinksAReconcileMakes {
    /// How a reconcile making `link`, whose two ends' pin types are now `pin_types_now`, judges
    /// it, `kept_though_their_pin_types_refused_them` holding the pin types of each link the live
    /// graph held so when it began.
    fn for_a_link(
        self,
        link: &RunningLinkBetweenTwoNodesByUuid,
        pin_types_now: Option<&(PinType, PinType)>,
        kept_though_their_pin_types_refused_them: &HashMap<
            RunningLinkBetweenTwoNodesByUuid,
            (PinType, PinType),
        >,
    ) -> ALinkConnected {
        match self {
            Self::EachAsASavedGraphsLink => ALinkConnected::OfASavedGraph,
            Self::ThoseTheLiveGraphKeptThoughTheirPinTypesRefusedThem
                if pin_types_now.is_some()
                    && kept_though_their_pin_types_refused_them.get(link) == pin_types_now =>
            {
                ALinkConnected::OfASavedGraph
            }
            Self::ThoseTheLiveGraphKeptThoughTheirPinTypesRefusedThem => ALinkConnected::MadeNow,
        }
    }
}

/// One node the live graph holds, as a reconcile compares it to the document's.
pub(in crate::core::runtime) struct LiveNodeAsTheGraphHoldsIt {
    pub(in crate::core::runtime) node_run_id: NodeRunId,
    pub(in crate::core::runtime) node_uuid: Uuid,
    pub(in crate::core::runtime) display_name: String,
    pub(in crate::core::runtime) node_type: NodeClassImportPath,
    pub(in crate::core::runtime) config: Value,
    pub(in crate::core::runtime) pins: Map<String, Value>,
    pub(in crate::core::runtime) studio: Map<String, Value>,
    pub(in crate::core::runtime) exposed_config_fields: Vec<String>,
}

/// A graph document a reconcile accepted: resolved and validated, each node's index in the graph
/// the runtime runs by its UUID, and its links by node UUID.
struct GraphDocumentAcceptedForAReconcile {
    resolution: GraphSnapshotWithGraphOnlyNodesResolved,
    document_node_index_by_uuid: HashMap<Uuid, usize>,
    document_links: Vec<RunningLinkBetweenTwoNodesByUuid>,
}

/// The name a kept node holds between the two passes of a reconcile's renames while another
/// kept node takes its name, unique by its UUID, so two nodes that swap names never meet.
fn a_name_no_document_node_holds_while_renames_swap(node_uuid: Uuid) -> String {
    format!("renaming-{node_uuid}")
}

fn cast_name(name: &str) -> Result<String> {
    Ok(cast_exposed_name_to_url_safe(name)?.into_owned())
}

impl LoadedStreamInThisRuntime {
    /// Bring this stream's live graph to the graph `graph_document` holds, each node matched by
    /// its UUID: a node the document no longer holds, or holds as another type, taken out; a
    /// kept node renamed, reconfigured and repinned to the document's; each new node added;
    /// and the links made to be the document's. The document is resolved and validated whole,
    /// as a load is, before the live graph is touched. A kept node not handed the values the
    /// document sets on it fails the reconcile once every node and link is made. Each link it
    /// makes as a saved graph's, as `links_it_makes` says, is kept when its pin types alone
    /// refuse it and its ports' families take it, and is set aside, not made, when the engine
    /// cannot wire it; so is a link the stream held set aside before, which no edit refuses.
    pub(in crate::core::runtime) async fn reconcile_the_live_graph_to_the_graph_document(
        &self,
        mut graph_document: Value,
        links_it_makes: LinksAReconcileMakes,
    ) -> Result<()> {
        if let Value::Object(document) = &mut graph_document {
            let stream_asset = self.stream_location().stream_asset;
            document
                .entry(STREAM_DOCUMENT_ASSET_KEY)
                .or_insert_with(|| Value::String(stream_asset.to_string()));
        }
        let graph_document =
            streamlib_project_repository::graph_document_with_each_stream_fragment_reference_resolved(
                self.project_directory(),
                &graph_document,
            )
            .map_err(Error::GraphError)?;
        let graph_whose_every_node_holds_a_uuid =
            GraphSnapshot::from_graph_document(graph_document)?
                .with_a_uuid_minted_for_each_node_holding_none();
        let node_types_the_document_names =
            graph_whose_every_node_holds_a_uuid.node_types_its_running_nodes_name_at_every_level();
        let described_node_types_before_the_document = self
            .node_types_this_stream_resolves()
            .the_described_node_types_as_they_stand_now(node_types_the_document_names.iter())?;
        let accepted = match self
            .the_graph_document_described_resolved_and_validated(
                graph_whose_every_node_holds_a_uuid,
                node_types_the_document_names,
            )
            .await
        {
            Ok(accepted) => accepted,
            Err(refusal) => {
                self.node_types_this_stream_resolves()
                    .put_back_the_described_node_types_as_they_stood(
                        described_node_types_before_the_document,
                    );
                return Err(refusal);
            }
        };
        let GraphDocumentAcceptedForAReconcile {
            resolution,
            document_node_index_by_uuid,
            document_links,
        } = accepted;
        let links_set_aside_before_the_document = self.the_links_set_aside_by_node_uuid();
        let graph = &resolution.graph_the_runtime_runs;
        let document_node_by_uuid: HashMap<Uuid, &GraphSnapshotNode> = document_node_index_by_uuid
            .into_iter()
            .map(|(node_uuid, node_index)| (node_uuid, &graph.nodes[node_index]))
            .collect();
        let document_link_set: HashSet<&RunningLinkBetweenTwoNodesByUuid> =
            document_links.iter().collect();
        let kept_though_their_pin_types_refused_them = match links_it_makes {
            LinksAReconcileMakes::EachAsASavedGraphsLink => HashMap::new(),
            LinksAReconcileMakes::ThoseTheLiveGraphKeptThoughTheirPinTypesRefusedThem => {
                self.the_live_links_their_pin_types_refuse()
            }
        };

        for live_node in self.the_live_nodes() {
            let kept = document_node_by_uuid
                .get(&live_node.node_uuid)
                .is_some_and(|document_node| document_node.node_type == live_node.node_type);
            if !kept {
                remove_node_impl(self.live_graph_change(), live_node.node_run_id).await?;
            }
        }
        for live_link in self.the_live_links_by_node_uuid() {
            if document_link_set.contains(&live_link.link_as_given) {
                continue;
            }
            for link_id in live_link.wired_as {
                disconnect_impl(self.live_graph_change(), link_id).await?;
            }
        }

        let node_names_the_graph_only_nodes_hold =
            resolution.graph_only_nodes_as_drawn.node_names_cast();
        self.compiler.scope(|this_streams_graph, _tx| {
            this_streams_graph
                .set_node_names_the_graph_only_nodes_hold(node_names_the_graph_only_nodes_hold);
        });
        *self.graph_only_nodes_as_drawn.lock() = resolution.graph_only_nodes_as_drawn;

        let kept_live_nodes = self.the_live_nodes();
        let mut kept_live_nodes_to_rename = Vec::new();
        let mut kept_nodes_whose_saved_values_change = Vec::new();
        for live_node in &kept_live_nodes {
            let document_node = document_node_by_uuid[&live_node.node_uuid];
            if live_node.display_name != cast_name(&document_node.name)? {
                kept_live_nodes_to_rename.push((live_node, document_node));
            }
        }
        self.compiler
            .scope(|this_streams_graph, _tx| -> Result<()> {
                let names_the_renamed_nodes_take = kept_live_nodes_to_rename
                    .iter()
                    .map(|(_, document_node)| cast_name(&document_node.name))
                    .collect::<Result<HashSet<String>>>()?;
                for (live_node, _) in &kept_live_nodes_to_rename {
                    if !names_the_renamed_nodes_take.contains(&live_node.display_name) {
                        continue;
                    }
                    this_streams_graph.traversal_mut().rename_v(
                        &live_node.node_run_id,
                        &a_name_no_document_node_holds_while_renames_swap(live_node.node_uuid),
                    )?;
                }
                for (live_node, document_node) in &kept_live_nodes_to_rename {
                    this_streams_graph
                        .traversal_mut()
                        .rename_v(&live_node.node_run_id, &document_node.name)?;
                }
                Ok(())
            })?;
        let node_types = self.node_types_this_stream_resolves();
        let kept_node_uuids: HashSet<Uuid> = kept_live_nodes
            .iter()
            .map(|live_node| live_node.node_uuid)
            .collect();
        for document_node in &graph.nodes {
            let Some(node_uuid) = document_node.id.filter(|id| !kept_node_uuids.contains(id))
            else {
                continue;
            };
            describe_the_type_an_add_names_when_it_must(
                &self.node_interpreter_launch_record,
                &document_node.node_type,
                self.lend_directory(),
                ConfigReferencesOfTheAddedNode::ValidatedWithTheWholeGraphBeforeTheAdd,
            )
            .await?;
            let spec = node_spec_holding_the_input_values(
                node_types,
                document_node.node_type.clone(),
                &document_node.input_values,
                document_node.studio.clone(),
            )
            .with_display_name(document_node.name.clone())
            .with_exposed_config_fields(document_node.exposed_config_fields.clone())
            .with_node_uuid(node_uuid);
            add_node_impl(
                self.live_graph_change(),
                Arc::clone(&self.node_interpreter_launch_record),
                self.lend_directory(),
                spec,
                ConfigReferencesOfTheAddedNode::ValidatedWithTheWholeGraphBeforeTheAdd,
            )
            .await?;
        }
        for live_node in &kept_live_nodes {
            let document_node = document_node_by_uuid[&live_node.node_uuid];
            let ConfigAndPinValues {
                config: document_config,
                pin_values: document_pins,
            } = LegacyConfigOfANodeType::of(node_types, &document_node.node_type)
                .split(&document_node.input_values);
            let ports_of_a_view = node_types
                .is_a_view_type_of_this_streams_project(&live_node.node_type)
                .then(|| node_types.port_info(&live_node.node_type))
                .flatten();
            let document_pins = match ports_of_a_view {
                Some(ports) => {
                    crate::core::nodes::the_saved_values_without_any_on_an_output_a_panel_writes(
                        node_types,
                        &live_node.node_type,
                        &ports,
                        &live_node.display_name,
                        document_pins,
                    )
                }
                None => document_pins,
            };
            if live_node.config != document_config {
                self.update_node_config_compiled_right_away(
                    &live_node.node_run_id,
                    &document_config,
                )
                .await?;
            }
            if live_node.exposed_config_fields != document_node.exposed_config_fields {
                self.set_exposed_config_fields(
                    &live_node.node_run_id,
                    document_node.exposed_config_fields.clone(),
                )?;
            }
            if live_node.pins != document_pins || live_node.studio != document_node.studio {
                kept_nodes_whose_saved_values_change.push(SavedValuesOfAKeptNode {
                    node_run_id: live_node.node_run_id.clone(),
                    pins: document_pins,
                    studio: document_node.studio.clone(),
                });
            }
        }
        let saved_values_handed_over = self.with_no_other_input_value_change(|| {
            set_the_saved_values_of_each_kept_node_and_hand_them_over(
                self.compiler.graph_lock(),
                kept_nodes_whose_saved_values_change,
            )
        });

        let node_run_id_of_each_node_uuid: HashMap<Uuid, NodeRunId> = self
            .the_live_nodes()
            .into_iter()
            .map(|live_node| (live_node.node_uuid, live_node.node_run_id))
            .collect();
        let live_links = self.the_live_links_as_given();
        let node_of = |node_uuid: &Uuid| {
            node_run_id_of_each_node_uuid
                .get(node_uuid)
                .cloned()
                .ok_or_else(|| {
                    Error::GraphError(format!("the live graph holds no node {node_uuid}"))
                })
        };
        for (document_link, link_as_drawn) in document_links.iter().zip(&graph.links) {
            if live_links.contains(document_link) {
                continue;
            }
            let from = OutputLinkPortRef::new(
                node_of(&document_link.source_node)?,
                &document_link.source_port,
            );
            let to = InputLinkPortRef::new(
                node_of(&document_link.target_node)?,
                &document_link.target_port,
            );
            let pin_types_now = self
                .compiler
                .scope_reading(|graph| the_pin_types_at_the_ends_of(graph, &from, &to));
            let a_link_connected = links_it_makes.for_a_link(
                document_link,
                pin_types_now.as_ref(),
                &kept_though_their_pin_types_refused_them,
            );
            let a_saved_graphs_link = a_link_connected == ALinkConnected::OfASavedGraph
                || links_set_aside_before_the_document.contains(document_link);
            if !a_saved_graphs_link {
                connect_impl(self.live_graph_change(), from, to, a_link_connected).await?;
                continue;
            }
            if let ALinkOfASavedGraphMade::SetAside(why_the_engine_cannot_wire_it) =
                connect_a_link_of_a_saved_graph_unless_the_engine_cannot_wire_it(
                    self.live_graph_change(),
                    from,
                    to,
                    a_link_connected,
                )
                .await?
            {
                self.graph_only_nodes_as_drawn
                    .lock()
                    .links_its_validation_set_aside
                    .push(LinkSetAsideByTheGraphsValidation {
                        link: link_as_drawn.clone(),
                        why_its_validation_refused_it: why_the_engine_cannot_wire_it.to_string(),
                    });
            }
        }
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        saved_values_handed_over
    }

    /// Describe each type `graph_whose_every_node_holds_a_uuid` names into this stream, then
    /// resolve its graph-only nodes and validate it, as a load does, touching no live node.
    async fn the_graph_document_described_resolved_and_validated(
        &self,
        graph_whose_every_node_holds_a_uuid: GraphSnapshot,
        node_types_the_document_names: Vec<NodeClassImportPath>,
    ) -> Result<GraphDocumentAcceptedForAReconcile> {
        if let Some(why_none_of_its_node_code_is_read) = self
            .node_types_this_stream_resolves()
            .why_every_node_type_is_held_as_a_placeholder()
        {
            self.node_types_this_stream_resolves()
                .hold_a_placeholder_of_each_node_type_not_described(
                    &graph_whose_every_node_holds_a_uuid,
                    &why_none_of_its_node_code_is_read,
                );
        } else {
            let launch_record = Arc::clone(&self.node_interpreter_launch_record);
            let lend_directory = self.lend_directory();
            tokio::task::spawn_blocking(carrying_this_threads_project_log_route_entry(move || {
                launch_record.describe_and_register_every_type_a_load_names(
                    node_types_the_document_names.iter(),
                    lend_directory.as_deref(),
                )
            }))
            .await
            .map_err(|join_failure| {
                Error::Runtime(format!(
                    "the describe of the graph's node types did not finish: {join_failure}"
                ))
            })??;
        }
        let resolution = graph_whose_every_node_holds_a_uuid
            .with_graph_only_nodes_resolved(self.node_types_this_stream_resolves())?
            .validated_setting_aside_each_link_it_refuses(self.node_types_this_stream_resolves())?;
        let graph = &resolution.graph_the_runtime_runs;

        let mut document_node_index_by_uuid: HashMap<Uuid, usize> = HashMap::new();
        let mut document_node_uuid_by_name_cast: HashMap<String, Uuid> = HashMap::new();
        for (node_index, node) in graph.nodes.iter().enumerate() {
            let node_uuid = node
                .id
                .ok_or_else(|| Error::GraphError(format!("node `{}` holds no UUID", node.name)))?;
            document_node_index_by_uuid.insert(node_uuid, node_index);
            document_node_uuid_by_name_cast.insert(cast_name(&node.name)?, node_uuid);
        }
        let document_node_uuid_named = |node_name: &str| -> Result<Uuid> {
            document_node_uuid_by_name_cast
                .get(&cast_name(node_name)?)
                .copied()
                .ok_or_else(|| Error::GraphError(format!("the graph holds no node `{node_name}`")))
        };
        let document_links = graph
            .links
            .iter()
            .map(|link| {
                Ok(RunningLinkBetweenTwoNodesByUuid {
                    source_node: document_node_uuid_named(&link.source.node)?,
                    source_port: cast_name(&link.source.port)?,
                    target_node: document_node_uuid_named(&link.target.node)?,
                    target_port: cast_name(&link.target.port)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let links_set_aside_before_the_document = self.the_links_set_aside_by_node_uuid();
        for set_aside in &resolution
            .graph_only_nodes_as_drawn
            .links_its_validation_set_aside
        {
            let set_aside_by_node_uuid = RunningLinkBetweenTwoNodesByUuid {
                source_node: document_node_uuid_named(&set_aside.link.source.node)?,
                source_port: cast_name(&set_aside.link.source.port)?,
                target_node: document_node_uuid_named(&set_aside.link.target.node)?,
                target_port: cast_name(&set_aside.link.target.port)?,
            };
            if !links_set_aside_before_the_document.contains(&set_aside_by_node_uuid) {
                return Err(Error::GraphError(format!(
                    "the graph is refused: {}",
                    set_aside.described()
                )));
            }
        }
        Ok(GraphDocumentAcceptedForAReconcile {
            resolution,
            document_node_index_by_uuid,
            document_links,
        })
    }

    /// Each node the live graph holds as it was given, as a reconcile compares it.
    pub(in crate::core::runtime) fn the_live_nodes(&self) -> Vec<LiveNodeAsTheGraphHoldsIt> {
        self.compiler
            .scope(|this_streams_graph, _tx| the_live_nodes_as_given_of(this_streams_graph))
    }

    /// Each link this stream's graph holds set aside, by its validation or as one the engine
    /// cannot wire, each end its node's UUID; one naming a node the graph no longer holds is
    /// left out.
    fn the_links_set_aside_by_node_uuid(&self) -> HashSet<RunningLinkBetweenTwoNodesByUuid> {
        let node_uuid_of_each_name: HashMap<String, Uuid> = self
            .the_live_nodes()
            .into_iter()
            .map(|live_node| (live_node.display_name, live_node.node_uuid))
            .collect();
        let node_uuid_named = |node_name: &str| {
            cast_name(node_name)
                .ok()
                .and_then(|cast| node_uuid_of_each_name.get(&cast).copied())
        };
        self.graph_only_nodes_as_drawn
            .lock()
            .links_its_validation_set_aside
            .iter()
            .filter_map(|set_aside| {
                Some(RunningLinkBetweenTwoNodesByUuid {
                    source_node: node_uuid_named(&set_aside.link.source.node)?,
                    source_port: cast_name(&set_aside.link.source.port).ok()?,
                    target_node: node_uuid_named(&set_aside.link.target.node)?,
                    target_port: cast_name(&set_aside.link.target.port).ok()?,
                })
            })
            .collect()
    }

    /// Each link the live graph holds as it was given, by node UUID.
    fn the_live_links_as_given(&self) -> HashSet<RunningLinkBetweenTwoNodesByUuid> {
        self.the_live_links_by_node_uuid()
            .into_iter()
            .map(|live_link| live_link.link_as_given)
            .collect()
    }

    /// Each link the live graph holds whose two ends' pin types the verdict refuses, among the
    /// conversions this stream resolves, beside those pin types.
    fn the_live_links_their_pin_types_refuse(
        &self,
    ) -> HashMap<RunningLinkBetweenTwoNodesByUuid, (PinType, PinType)> {
        let live_links = self.the_live_links_as_given();
        let conversions = self.node_types_this_stream_resolves().pin_conversions();
        self.compiler.scope_reading(|graph| {
            let node_run_id_of_each_node_uuid: HashMap<Uuid, NodeRunId> = graph
                .traversal()
                .v(())
                .iter()
                .map(|node| (node.node_uuid, node.id.clone()))
                .collect();
            live_links
                .into_iter()
                .filter_map(|link| {
                    let from = OutputLinkPortRef::new(
                        node_run_id_of_each_node_uuid
                            .get(&link.source_node)?
                            .clone(),
                        &link.source_port,
                    );
                    let to = InputLinkPortRef::new(
                        node_run_id_of_each_node_uuid
                            .get(&link.target_node)?
                            .clone(),
                        &link.target_port,
                    );
                    if !the_pin_types_refuse_the_link(graph, &from, &to, &conversions) {
                        return None;
                    }
                    let pin_types = the_pin_types_at_the_ends_of(graph, &from, &to)?;
                    Some((link, pin_types))
                })
                .collect()
        })
    }

    /// Each link the live graph holds as it was given, by node UUID, beside the links it is
    /// wired as, as a reconcile compares it.
    pub(in crate::core::runtime) fn the_live_links_by_node_uuid(&self) -> Vec<LiveLinkAsGiven> {
        self.compiler.scope(|this_streams_graph, _tx| {
            the_live_links_as_given_by_node_uuid_of(this_streams_graph)
        })
    }
}

/// Each node `graph` holds that is not pending deletion, as a reconcile compares it.
fn the_live_nodes_of(graph: &Graph) -> Vec<LiveNodeAsTheGraphHoldsIt> {
    graph
        .traversal()
        .v(())
        .iter()
        .filter(|node| node.get::<PendingDeletionComponent>().is_none())
        .map(|node| LiveNodeAsTheGraphHoldsIt {
            node_run_id: node.id.clone(),
            node_uuid: node.node_uuid,
            display_name: node.display_name.clone(),
            node_type: node.node_type.clone(),
            config: node.config.clone().unwrap_or_default(),
            pins: node.pins.clone(),
            studio: node.studio.clone(),
            exposed_config_fields: node.exposed_config_fields.clone(),
        })
        .collect()
}

/// Each link `graph` holds that is not pending deletion, each end its node's UUID.
fn the_live_links_by_node_uuid_of(
    graph: &Graph,
) -> Vec<(LinkUniqueId, RunningLinkBetweenTwoNodesByUuid)> {
    let node_uuid_of_each_node: HashMap<NodeRunId, Uuid> = graph
        .traversal()
        .v(())
        .iter()
        .map(|node| (node.id.clone(), node.node_uuid))
        .collect();
    graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| link.get::<PendingDeletionComponent>().is_none())
        .filter_map(|link| {
            Some((
                link.id.clone(),
                RunningLinkBetweenTwoNodesByUuid {
                    source_node: *node_uuid_of_each_node.get(link.source.node_run_id())?,
                    source_port: link.source.port_name().to_owned(),
                    target_node: *node_uuid_of_each_node.get(link.target.node_run_id())?,
                    target_port: link.target.port_name().to_owned(),
                },
            ))
        })
        .collect()
}

/// Each node `graph` holds as it was given, as [`the_live_nodes_of`] reads them: the stream's
/// function node pool, a compile result, left out.
pub(in crate::core::runtime) fn the_live_nodes_as_given_of(
    graph: &Graph,
) -> Vec<LiveNodeAsTheGraphHoldsIt> {
    the_live_nodes_of(graph)
        .into_iter()
        .filter(|live_node| !is_the_function_node_pool_type(&live_node.node_type))
        .collect()
}

/// Each link `graph` holds as it was given, as [`the_live_links_by_node_uuid_of`] reads them,
/// beside the links it is wired as: a link carried through the function node pool once, from
/// its island to its reader, wired as its two links through the pool.
pub(in crate::core::runtime) fn the_live_links_as_given_by_node_uuid_of(
    graph: &Graph,
) -> Vec<LiveLinkAsGiven> {
    let live_links = the_live_links_by_node_uuid_of(graph);
    let live_link_of = |link_id: &LinkUniqueId| {
        live_links
            .iter()
            .find(|(live, _)| live == link_id)
            .map(|(_, live_link)| live_link)
    };
    let carried_links = the_links_carried_through_the_function_node_pool_of(graph);
    let wired_as_a_carried_link: HashSet<&LinkUniqueId> = carried_links
        .iter()
        .flat_map(|carried| {
            [
                &carried.into_the_function_node_pool.id,
                &carried.out_of_the_function_node_pool.id,
            ]
        })
        .collect();
    let carried_links_as_given = carried_links.iter().filter_map(|carried| {
        let into_the_pool = live_link_of(&carried.into_the_function_node_pool.id)?;
        let out_of_the_pool = live_link_of(&carried.out_of_the_function_node_pool.id)?;
        Some(LiveLinkAsGiven {
            link_as_given: RunningLinkBetweenTwoNodesByUuid {
                source_node: into_the_pool.source_node,
                source_port: into_the_pool.source_port.clone(),
                target_node: out_of_the_pool.target_node,
                target_port: out_of_the_pool.target_port.clone(),
            },
            wired_as: vec![
                carried.into_the_function_node_pool.id.clone(),
                carried.out_of_the_function_node_pool.id.clone(),
            ],
        })
    });
    live_links
        .iter()
        .filter(|(link_id, _)| !wired_as_a_carried_link.contains(link_id))
        .map(|(link_id, live_link)| LiveLinkAsGiven {
            link_as_given: live_link.clone(),
            wired_as: vec![link_id.clone()],
        })
        .chain(carried_links_as_given)
        .collect()
}

#[cfg(test)]
mod tests;
