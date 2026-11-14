// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::{Map, Value};
use streamlib_project_repository::STREAM_DOCUMENT_ASSET_KEY;
use uuid::Uuid;

use super::LoadedStreamInThisRuntime;
use super::operations_runtime::{
    ConfigReferencesOfTheAddedNode, add_processor_impl, connect_impl, disconnect_impl,
    remove_processor_impl,
};
use crate::core::compiler::compiler_ops::function_node_pool::{
    is_the_function_node_pool_type, the_links_carried_through_the_function_node_pool_of,
};
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, LinkUniqueId,
    PendingDeletionComponent, ProcessorUniqueId, cast_exposed_name_to_url_safe,
};
use crate::core::graph_snapshot::{
    GraphSnapshot, GraphSnapshotNode, GraphSnapshotWithGraphOnlyNodesResolved,
};
use crate::core::logging::carrying_this_threads_project_log_route_entry;
use crate::core::processors::ProcessorSpec;
use crate::core::pubsub::RuntimeEvent;
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};

/// One link of a running graph, each end its node's UUID and its port's cast name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct RunningLinkBetweenTwoNodesByUuid {
    pub(super) source_node: Uuid,
    pub(super) source_port: String,
    pub(super) target_node: Uuid,
    pub(super) target_port: String,
}

/// One link the live graph holds as it was given, as a reconcile compares it to the document's,
/// beside the links it is wired as, which a disconnect of it takes out together.
pub(super) struct LiveLinkAsGiven {
    pub(super) link_as_given: RunningLinkBetweenTwoNodesByUuid,
    pub(super) wired_as: Vec<LinkUniqueId>,
}

/// One node the live graph holds, as a reconcile compares it to the document's.
pub(super) struct LiveNodeAsTheGraphHoldsIt {
    pub(super) processor_id: ProcessorUniqueId,
    pub(super) node_uuid: Uuid,
    pub(super) display_name: String,
    pub(super) processor_type: ProcessorClassImportPath,
    pub(super) config: Value,
    pub(super) pins: Map<String, Value>,
    pub(super) studio: Map<String, Value>,
    pub(super) exposed_config_fields: Vec<String>,
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
    /// as a load is, before the live graph is touched.
    pub(super) async fn reconcile_the_live_graph_to_the_graph_document(
        &self,
        mut graph_document: Value,
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
        let graph = &resolution.graph_the_runtime_runs;
        let document_node_by_uuid: HashMap<Uuid, &GraphSnapshotNode> = document_node_index_by_uuid
            .into_iter()
            .map(|(node_uuid, node_index)| (node_uuid, &graph.nodes[node_index]))
            .collect();
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
                        &live_node.processor_id,
                        &a_name_no_document_node_holds_while_renames_swap(live_node.node_uuid),
                    )?;
                }
                for (live_node, document_node) in &kept_live_nodes_to_rename {
                    this_streams_graph
                        .traversal_mut()
                        .rename_v(&live_node.processor_id, &document_node.name)?;
                }
                Ok(())
            })?;
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
                self.update_processor_config_compiled_right_away(
                    &live_node.processor_id,
                    &document_node.config,
                )
                .await?;
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
            .map(|live_link| live_link.link_as_given)
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

    /// Describe each type `graph_whose_every_node_holds_a_uuid` names into this stream, then
    /// resolve its graph-only nodes and validate it, as a load does, touching no live node.
    async fn the_graph_document_described_resolved_and_validated(
        &self,
        graph_whose_every_node_holds_a_uuid: GraphSnapshot,
        node_types_the_document_names: Vec<ProcessorClassImportPath>,
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
    pub(super) fn the_live_nodes(&self) -> Vec<LiveNodeAsTheGraphHoldsIt> {
        self.compiler
            .scope(|this_streams_graph, _tx| the_live_nodes_as_given_of(this_streams_graph))
    }

    /// Each link this stream's graph holds set aside by its validation, each end its node's
    /// UUID; one naming a node the graph no longer holds is left out.
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

    /// Each link the live graph holds as it was given, by node UUID, beside the links it is
    /// wired as, as a reconcile compares it.
    pub(super) fn the_live_links_by_node_uuid(&self) -> Vec<LiveLinkAsGiven> {
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
}

/// Each link `graph` holds that is not pending deletion, each end its node's UUID.
fn the_live_links_by_node_uuid_of(
    graph: &Graph,
) -> Vec<(LinkUniqueId, RunningLinkBetweenTwoNodesByUuid)> {
    let node_uuid_of_each_processor: HashMap<ProcessorUniqueId, Uuid> = graph
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
                    source_node: *node_uuid_of_each_processor.get(link.source.processor_id())?,
                    source_port: link.source.port_name().to_owned(),
                    target_node: *node_uuid_of_each_processor.get(link.target.processor_id())?,
                    target_port: link.target.port_name().to_owned(),
                },
            ))
        })
        .collect()
}

/// Each node `graph` holds as it was given, as [`the_live_nodes_of`] reads them: the stream's
/// function node pool, a compile result, left out.
pub(super) fn the_live_nodes_as_given_of(graph: &Graph) -> Vec<LiveNodeAsTheGraphHoldsIt> {
    the_live_nodes_of(graph)
        .into_iter()
        .filter(|live_node| !is_the_function_node_pool_type(&live_node.processor_type))
        .collect()
}

/// Each link `graph` holds as it was given, as [`the_live_links_by_node_uuid_of`] reads them,
/// beside the links it is wired as: a link carried through the function node pool once, from
/// its island to its reader, wired as its two links through the pool.
pub(super) fn the_live_links_as_given_by_node_uuid_of(graph: &Graph) -> Vec<LiveLinkAsGiven> {
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
mod tests {
    use crate::core::runtime::a_project_on_disk_for_a_test::{
        options_loading_a_stream_of_the_test_project_at,
        options_loading_a_stream_of_the_test_project_in,
    };
    use serde_json::json;
    use serial_test::serial;

    use super::*;
    use crate::core::compiler::PendingOperation;
    use crate::core::runtime::Runner;
    use crate::core::test_support::{
        MockInputOnlyProcessor, MockOutputOnlyProcessor, MockProcessor,
        ensure_test_mocks_registered,
    };

    const NODE_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const NODE_B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

    /// A graph document of `a_name` (node A, an output-only mock) linked from `out1` into
    /// `in1` of `b_name` (node B, of `b_type`).
    fn a_document_of_a_linked_into_b(
        a_name: &str,
        b_name: &str,
        b_type: ProcessorClassImportPath,
    ) -> Value {
        json!({
            "nodes": [
                {
                    "id": NODE_A,
                    "name": a_name,
                    "type": MockOutputOnlyProcessor::processor_class_import_path().as_str(),
                    "config": {},
                },
                { "id": NODE_B, "name": b_name, "type": b_type.as_str(), "config": {} },
            ],
            "links": [{
                "source": { "node": a_name, "port": "out1" },
                "target": { "node": b_name, "port": "in1" },
            }],
        })
    }

    /// Each live node as `(UUID, name, type)`, and each live link by node UUID, sorted.
    fn the_live_graph_by_node_uuid(
        stream: &LoadedStreamInThisRuntime,
    ) -> (Vec<(String, String, String)>, Vec<String>) {
        let mut nodes: Vec<_> = stream
            .the_live_nodes()
            .into_iter()
            .map(|node| {
                (
                    node.node_uuid.to_string(),
                    node.display_name,
                    node.processor_type.as_str().to_owned(),
                )
            })
            .collect();
        nodes.sort();
        let mut links: Vec<_> = stream
            .the_live_links_by_node_uuid()
            .into_iter()
            .map(
                |LiveLinkAsGiven {
                     link_as_given: link,
                     ..
                 }| {
                    format!(
                        "{}/{} -> {}/{}",
                        link.source_node, link.source_port, link.target_node, link.target_port
                    )
                },
            )
            .collect();
        links.sort();
        (nodes, links)
    }

    fn the_graph_node_count(stream: &LoadedStreamInThisRuntime) -> usize {
        stream
            .compiler
            .scope_reading(|graph| graph.traversal().v(()).iter().count())
    }

    fn reconcile(stream: &LoadedStreamInThisRuntime, graph_document: Value) -> Result<()> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime")
            .block_on(stream.reconcile_the_live_graph_to_the_graph_document(graph_document))
    }

    fn a_stream_loaded_from(
        runner: &Runner,
        project_directory: &tempfile::TempDir,
        graph_document: Value,
    ) -> Arc<LoadedStreamInThisRuntime> {
        runner
            .load_stream_from_graph_snapshot(
                &GraphSnapshot::from_graph_document(graph_document).expect("the document reads"),
                options_loading_a_stream_of_the_test_project_at(project_directory.path(), "main"),
            )
            .expect("the stream loads")
    }

    #[test]
    fn a_reconcile_compares_a_link_carried_through_the_pool_as_the_link_it_was_given() {
        let (graph, lfo, reader, lfo_into_the_reader) =
            crate::core::compiler::compiler_ops::function_node_pool::tests::a_graph_carrying_an_lfo_into_its_reader_through_the_pool();
        let node_uuid_of = |processor_id: &ProcessorUniqueId| {
            graph.traversal().v(processor_id).first().unwrap().node_uuid
        };

        let live_nodes = the_live_nodes_as_given_of(&graph);
        let live_links = the_live_links_as_given_by_node_uuid_of(&graph);

        assert_eq!(
            live_nodes
                .iter()
                .map(|live_node| live_node.node_uuid)
                .collect::<HashSet<_>>(),
            HashSet::from([node_uuid_of(&lfo), node_uuid_of(&reader)]),
            "the pool is no node a document holds"
        );
        assert_eq!(live_links.len(), 1);
        let LiveLinkAsGiven {
            link_as_given,
            wired_as,
        } = &live_links[0];
        assert_eq!(
            link_as_given,
            &RunningLinkBetweenTwoNodesByUuid {
                source_node: node_uuid_of(&lfo),
                source_port: "result".to_string(),
                target_node: node_uuid_of(&reader),
                target_port: "in1".to_string(),
            }
        );
        assert_eq!(
            wired_as.iter().cloned().collect::<HashSet<_>>(),
            crate::core::compiler::compiler_ops::function_node_pool::the_links_a_link_is_wired_as(
                &graph,
                &lfo_into_the_reader
            )
            .into_iter()
            .collect::<HashSet<_>>(),
            "a disconnect takes out both links it is wired as"
        );
    }

    #[test]
    #[serial]
    fn two_kept_nodes_that_swap_names_are_renamed_and_keep_their_link() {
        ensure_test_mocks_registered();
        let project_directory = tempfile::tempdir().expect("a project directory");
        let runner = Runner::new().expect("Runner::new");
        let stream = a_stream_loaded_from(
            &runner,
            &project_directory,
            a_document_of_a_linked_into_b(
                "left",
                "right",
                MockProcessor::processor_class_import_path(),
            ),
        );

        let swapped = a_document_of_a_linked_into_b(
            "right",
            "left",
            MockProcessor::processor_class_import_path(),
        );
        reconcile(&stream, swapped).expect("the swap applies");

        assert_eq!(
            the_live_graph_by_node_uuid(&stream),
            (
                vec![
                    (
                        NODE_A.to_owned(),
                        "right".to_owned(),
                        MockOutputOnlyProcessor::processor_class_import_path()
                            .as_str()
                            .to_owned()
                    ),
                    (
                        NODE_B.to_owned(),
                        "left".to_owned(),
                        MockProcessor::processor_class_import_path()
                            .as_str()
                            .to_owned()
                    ),
                ],
                vec![format!("{NODE_A}/out1 -> {NODE_B}/in1")],
            )
        );
    }

    #[test]
    #[serial]
    fn a_node_whose_type_changes_on_a_stream_not_started_is_replaced_under_its_uuid() {
        ensure_test_mocks_registered();
        let project_directory = tempfile::tempdir().expect("a project directory");
        let runner = Runner::new().expect("Runner::new");
        let stream = a_stream_loaded_from(
            &runner,
            &project_directory,
            a_document_of_a_linked_into_b("a", "b", MockProcessor::processor_class_import_path()),
        );
        let replaced_processor = stream
            .the_live_nodes()
            .into_iter()
            .find(|node| node.node_uuid.to_string() == NODE_B)
            .expect("node B is live")
            .processor_id;

        reconcile(
            &stream,
            a_document_of_a_linked_into_b(
                "a",
                "b",
                MockInputOnlyProcessor::processor_class_import_path(),
            ),
        )
        .expect("the type change applies");

        assert_eq!(
            the_live_graph_by_node_uuid(&stream),
            (
                vec![
                    (
                        NODE_A.to_owned(),
                        "a".to_owned(),
                        MockOutputOnlyProcessor::processor_class_import_path()
                            .as_str()
                            .to_owned()
                    ),
                    (
                        NODE_B.to_owned(),
                        "b".to_owned(),
                        MockInputOnlyProcessor::processor_class_import_path()
                            .as_str()
                            .to_owned()
                    ),
                ],
                vec![format!("{NODE_A}/out1 -> {NODE_B}/in1")],
            )
        );
        assert_eq!(
            the_graph_node_count(&stream),
            2,
            "the replaced node is out of the graph"
        );
        assert!(
            !stream
                .compiler
                .logged_pending_operations()
                .iter()
                .any(|operation| matches!(
                    operation,
                    PendingOperation::AddProcessor(id) | PendingOperation::RemoveProcessor(id)
                        if *id == replaced_processor
                )),
            "the start builds nothing of the replaced node"
        );
    }

    /// A stand-in venv interpreter in `project_directory` that answers every describe with
    /// what `describe_answer` holds when it runs, so a test can change the answer between
    /// describes.
    fn an_interpreter_answering_each_describe_from(
        project_directory: &tempfile::TempDir,
        describe_answer: &std::path::Path,
    ) -> crate::core::runtime::StreamEnvironment {
        let interpreter = project_directory.path().join("stub-python");
        crate::core::test_support::write_an_executable_script_from_a_child_process(
            &interpreter,
            &format!("#!/bin/sh\ncat '{}'\n", describe_answer.display()),
        );
        crate::core::runtime::StreamEnvironment {
            project_directory: project_directory.path().to_path_buf(),
            interpreter,
        }
    }

    fn a_describe_answer_of(described_node_types: &[(&ProcessorClassImportPath, &str)]) -> String {
        json!({
            "described_node_types": described_node_types
                .iter()
                .map(|(node_type, input_port)| json!({
                    "import_path": node_type.as_str(),
                    "short_name": "Blur",
                    "description": "blurs",
                    "kind": "process",
                    "execution": {"mode": "reactive"},
                    "scheduling_priority": null,
                    "paced_by_real_time": false,
                    "config_schema": {"type": "object"},
                    "input_ports": [{"name": input_port, "description": "", "family": {"family": "records"}}],
                    "output_ports": [],
                }))
                .collect::<Vec<_>>(),
            "refused_node_types": [],
        })
        .to_string()
    }

    /// A document refused after its describe leaves every node type the stream resolves as it
    /// was: a type it re-described keeps its earlier ports, and a type it newly described is
    /// not resolved at all.
    #[test]
    #[serial]
    fn a_refused_document_leaves_the_node_types_the_stream_resolves_as_they_were() {
        ensure_test_mocks_registered();
        let kept_type = ProcessorClassImportPath::new("my_app.kept_across_a_refused_reload:Blur")
            .expect("the test path names a class");
        let newly_named_type =
            ProcessorClassImportPath::new("my_app.named_by_a_refused_reload:Blur")
                .expect("the test path names a class");
        let project_directory = tempfile::tempdir().expect("a project directory");
        let describe_answer = project_directory.path().join("describe-answer.json");
        std::fs::write(
            &describe_answer,
            a_describe_answer_of(&[(&kept_type, "video")]),
        )
        .expect("the first describe answer is written");
        let stream_environment =
            an_interpreter_answering_each_describe_from(&project_directory, &describe_answer);
        let runner = Runner::new().expect("Runner::new");
        runner
            .set_processor_interpreter_lend_directory("/opt/tatolab/lib/tatolab/lend".into())
            .expect("the runner's first lend directory");
        let stream = runner
            .load_stream_from_graph_snapshot(
                &GraphSnapshot::from_graph_document(json!({
                    "nodes": [{ "id": NODE_A, "name": "kept", "type": kept_type.as_str() }],
                }))
                .expect("the document reads"),
                options_loading_a_stream_of_the_test_project_in(stream_environment, "main"),
            )
            .expect("the stream loads");

        std::fs::write(
            &describe_answer,
            a_describe_answer_of(&[(&kept_type, "frames"), (&newly_named_type, "audio")]),
        )
        .expect("the later describe answer is written");
        let refusal = reconcile(
            &stream,
            json!({
                "nodes": [
                    { "id": NODE_A, "name": "kept", "type": kept_type.as_str() },
                    { "id": NODE_B, "name": "added", "type": newly_named_type.as_str() },
                    {
                        "name": "source",
                        "type": MockOutputOnlyProcessor::processor_class_import_path().as_str(),
                    },
                ],
                "links": [{
                    "source": { "node": "source", "port": "out1" },
                    "target": { "node": "added", "port": "no_such_port" },
                }],
            }),
        )
        .expect_err("a link into a port the type does not declare is refused");

        let node_types = stream.node_types_this_stream_resolves();
        let kept_input_names: Vec<String> = node_types
            .descriptor(&kept_type)
            .expect("the stream still resolves the kept type")
            .inputs
            .into_iter()
            .map(|port| port.name)
            .collect();
        assert_eq!(kept_input_names, ["video"], "refused with: {refusal}");
        assert!(
            node_types.descriptor(&newly_named_type).is_none(),
            "the refused document's new type stayed resolved; refused with: {refusal}"
        );
        assert_eq!(
            the_live_graph_by_node_uuid(&stream).0,
            vec![(
                NODE_A.to_owned(),
                "kept".to_owned(),
                kept_type.as_str().to_owned()
            )]
        );
    }
}
