// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::{Map, Value};
use uuid::Uuid;

use super::LoadedStreamInThisRuntime;
use super::operations_runtime::{
    ConfigReferencesOfTheAddedNode, add_processor_impl, connect_impl, disconnect_impl,
    remove_processor_impl,
};
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::graph::{
    GraphEdgeWithComponents, GraphNodeWithComponents, LinkUniqueId, PendingDeletionComponent,
    ProcessorUniqueId, cast_exposed_name_to_url_safe,
};
use crate::core::graph_snapshot::{GraphSnapshot, GraphSnapshotNode};
use crate::core::logging::carrying_this_threads_project_log_route_entry;
use crate::core::processors::ProcessorSpec;
use crate::core::pubsub::RuntimeEvent;
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};

/// One link of a running graph, each end its node's UUID and its port's cast name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RunningLinkBetweenTwoNodesByUuid {
    source_node: Uuid,
    source_port: String,
    target_node: Uuid,
    target_port: String,
}

/// One node the live graph holds, as a reconcile compares it to the document's.
struct LiveNodeAsTheGraphHoldsIt {
    processor_id: ProcessorUniqueId,
    node_uuid: Uuid,
    display_name: String,
    processor_type: ProcessorClassImportPath,
    config: Value,
    pins: Map<String, Value>,
    studio: Map<String, Value>,
    exposed_config_fields: Vec<String>,
}

fn cast_name(name: &str) -> Result<String> {
    Ok(cast_exposed_name_to_url_safe(name)?.into_owned())
}

impl LoadedStreamInThisRuntime {
    /// Bring this stream's live graph to the graph `graph_document` holds, each node matched by
    /// its UUID: a node the document no longer holds, or holds as another type, taken out; a
    /// kept node renamed, reconfigured and repinned to the document's; each new node added;
    /// and the links made to be the document's. The document is resolved and validated whole,
    /// as a load is, before the live graph is touched.
    pub(super) async fn reconcile_the_live_graph_to_the_graph_document(
        &self,
        graph_document: Value,
    ) -> Result<()> {
        let resolution = GraphSnapshot::from_graph_document(graph_document)?
            .with_a_uuid_minted_for_each_node_holding_none()
            .with_graph_only_nodes_resolved()?;
        let node_types_the_document_names: Vec<ProcessorClassImportPath> = resolution
            .graph_the_runtime_runs
            .nodes
            .iter()
            .map(|node| node.processor_type.clone())
            .collect();
        let launch_record = Arc::clone(&self.processor_interpreter_launch_record);
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
        resolution.validate(self.node_types_this_stream_resolves())?;
        let graph = &resolution.graph_the_runtime_runs;

        let mut document_node_by_uuid: HashMap<Uuid, &GraphSnapshotNode> = HashMap::new();
        let mut document_node_uuid_by_name_cast: HashMap<String, Uuid> = HashMap::new();
        for node in &graph.nodes {
            let node_uuid = node
                .id
                .ok_or_else(|| Error::GraphError(format!("node `{}` holds no UUID", node.name)))?;
            document_node_by_uuid.insert(node_uuid, node);
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
        let document_link_set: HashSet<&RunningLinkBetweenTwoNodesByUuid> =
            document_links.iter().collect();

        for live_node in self.the_live_nodes() {
            let kept =
                document_node_by_uuid
                    .get(&live_node.node_uuid)
                    .is_some_and(|document_node| {
                        document_node.processor_type == live_node.processor_type
                    });
            if !kept {
                remove_processor_impl(self.live_graph_change(), live_node.processor_id).await?;
            }
        }
        for (link_id, live_link) in self.the_live_links_by_node_uuid() {
            if !document_link_set.contains(&live_link) {
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
        for live_node in &kept_live_nodes {
            let document_node = document_node_by_uuid[&live_node.node_uuid];
            if live_node.display_name != cast_name(&document_node.name)? {
                self.compiler.scope(|this_streams_graph, _tx| {
                    this_streams_graph
                        .traversal_mut()
                        .rename_v(&live_node.processor_id, &document_node.name)
                })?;
            }
        }
        let kept_node_uuids: HashSet<Uuid> = kept_live_nodes
            .iter()
            .map(|live_node| live_node.node_uuid)
            .collect();
        for document_node in &graph.nodes {
            let Some(node_uuid) = document_node.id.filter(|id| !kept_node_uuids.contains(id))
            else {
                continue;
            };
            let spec = ProcessorSpec::new(
                document_node.processor_type.clone(),
                document_node.config.clone(),
            )
            .with_display_name(document_node.name.clone())
            .with_pins_and_studio_fields(document_node.pins.clone(), document_node.studio.clone())
            .with_exposed_config_fields(document_node.exposed_config_fields.clone())
            .with_node_uuid(node_uuid);
            add_processor_impl(
                self.live_graph_change(),
                Arc::clone(&self.processor_interpreter_launch_record),
                self.lend_directory(),
                spec,
                ConfigReferencesOfTheAddedNode::ValidatedWithTheWholeGraphBeforeTheAdd,
            )
            .await?;
        }
        for live_node in &kept_live_nodes {
            let document_node = document_node_by_uuid[&live_node.node_uuid];
            if live_node.config != document_node.config {
                self.update_processor_config(&live_node.processor_id, &document_node.config)?;
            }
            if live_node.exposed_config_fields != document_node.exposed_config_fields {
                self.set_exposed_config_fields(
                    &live_node.processor_id,
                    document_node.exposed_config_fields.clone(),
                )?;
            }
            if live_node.pins != document_node.pins || live_node.studio != document_node.studio {
                self.compiler.scope(|this_streams_graph, _tx| {
                    if let Some(node) = this_streams_graph
                        .traversal_mut()
                        .v(&live_node.processor_id)
                        .first_mut()
                    {
                        node.pins = document_node.pins.clone();
                        node.studio = document_node.studio.clone();
                    }
                });
            }
        }

        let processor_of_each_node_uuid: HashMap<Uuid, ProcessorUniqueId> = self
            .the_live_nodes()
            .into_iter()
            .map(|live_node| (live_node.node_uuid, live_node.processor_id))
            .collect();
        let live_links: HashSet<RunningLinkBetweenTwoNodesByUuid> = self
            .the_live_links_by_node_uuid()
            .into_iter()
            .map(|(_, live_link)| live_link)
            .collect();
        let processor_of = |node_uuid: &Uuid| {
            processor_of_each_node_uuid
                .get(node_uuid)
                .cloned()
                .ok_or_else(|| {
                    Error::GraphError(format!("the live graph holds no node {node_uuid}"))
                })
        };
        for document_link in &document_links {
            if live_links.contains(document_link) {
                continue;
            }
            connect_impl(
                self.live_graph_change(),
                OutputLinkPortRef::new(
                    processor_of(&document_link.source_node)?,
                    &document_link.source_port,
                ),
                InputLinkPortRef::new(
                    processor_of(&document_link.target_node)?,
                    &document_link.target_port,
                ),
            )
            .await?;
        }
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        Ok(())
    }

    fn the_live_nodes(&self) -> Vec<LiveNodeAsTheGraphHoldsIt> {
        self.compiler.scope(|this_streams_graph, _tx| {
            this_streams_graph
                .traversal()
                .v(())
                .iter()
                .filter(|node| node.get::<PendingDeletionComponent>().is_none())
                .map(|node| LiveNodeAsTheGraphHoldsIt {
                    processor_id: node.id.clone(),
                    node_uuid: node.node_uuid,
                    display_name: node.display_name.clone(),
                    processor_type: node.processor_type.clone(),
                    config: node.config.clone().unwrap_or_default(),
                    pins: node.pins.clone(),
                    studio: node.studio.clone(),
                    exposed_config_fields: node.exposed_config_fields.clone(),
                })
                .collect()
        })
    }

    fn the_live_links_by_node_uuid(&self) -> Vec<(LinkUniqueId, RunningLinkBetweenTwoNodesByUuid)> {
        self.compiler.scope(|this_streams_graph, _tx| {
            let node_uuid_of_each_processor: HashMap<ProcessorUniqueId, Uuid> = this_streams_graph
                .traversal()
                .v(())
                .iter()
                .map(|node| (node.id.clone(), node.node_uuid))
                .collect();
            this_streams_graph
                .traversal()
                .e(())
                .iter()
                .filter(|link| link.get::<PendingDeletionComponent>().is_none())
                .filter_map(|link| {
                    Some((
                        link.id.clone(),
                        RunningLinkBetweenTwoNodesByUuid {
                            source_node: *node_uuid_of_each_processor
                                .get(link.source.processor_id())?,
                            source_port: link.source.port_name().to_owned(),
                            target_node: *node_uuid_of_each_processor
                                .get(link.target.processor_id())?,
                            target_port: link.target.port_name().to_owned(),
                        },
                    ))
                })
                .collect()
        })
    }
}
