// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    InsertedNodeOnTheWire, PinLinkMarksOnTheWire,
};

/// What the answer of a graph transaction names beside the operations it applied: the `as` of
/// each node added, why a `connect` inserted each node it added on its link, and each input
/// value set that a link overrides.
pub(super) struct WhatAnAnswerNamesBesideItsOperations<'named> {
    pub(super) what_the_answer_names_of_each_added_node:
        &'named HashMap<Uuid, WhatTheAnswerNamesOfAnAddedNode>,
    pub(super) overridden_by_links: Vec<GraphTransactionInputOverriddenByALink>,
}

impl StreamGraphTransactionExecutor<'_> {
    /// The answer of `applied_before`, the transaction of the project `project` an action sent
    /// again under its key first applied: each edit of a loaded stream under its stream id, each
    /// input value set that a link overrode, and each node a `connect` inserted on its link, as
    /// its first answer named them.
    pub(super) fn the_answer_of_a_graph_transaction_applied_before(
        &self,
        project: Uuid,
        applied_before: AppliedProjectTransaction,
    ) -> GraphTransactionToolResult {
        let stream_id_of_each_stream_asset =
            self.the_stream_id_of_each_loaded_stream_asset(project);
        let mut applied_live: Vec<(String, StreamGraphOperation)> = Vec::new();
        for operation in applied_before.transaction.operations {
            let ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
                stream,
                graph_operation,
            }) = operation
            else {
                continue;
            };
            let stream_id = stream_id_of_each_stream_asset
                .get(&stream)
                .cloned()
                .unwrap_or_else(|| stream.to_string());
            applied_live.push((stream_id, graph_operation));
        }
        let what_the_answer_names_of_each_added_node = applied_before
            .transaction
            .nodes_inserted_on_links
            .iter()
            .map(|inserted| {
                (
                    inserted.node,
                    WhatTheAnswerNamesOfAnAddedNode {
                        node_in_its_document: inserted.node,
                        label: None,
                        inserted_because: Some(inserted.inserted_because),
                    },
                )
            })
            .collect();
        self.the_answer_of_a_graph_transaction(
            GraphEditMode::Commit,
            Some(applied_before.transaction.transaction.to_string()),
            Some(applied_before.commit),
            &|stream_id| {
                self.operations_on_the_loaded_streams
                    .runtime_operations_of_the_stream_a_call_names(stream_id)
                    .ok()
            },
            applied_live,
            WhatAnAnswerNamesBesideItsOperations {
                what_the_answer_names_of_each_added_node: &what_the_answer_names_of_each_added_node,
                overridden_by_links: applied_before.transaction.inputs_overridden_by_links,
            },
        )
    }

    pub(super) fn the_answer_of_a_graph_transaction(
        &self,
        mode: GraphEditMode,
        transaction: Option<String>,
        commit: Option<String>,
        stream_operations_of: &dyn Fn(&str) -> Option<Arc<dyn RuntimeOperations>>,
        applied_live: Vec<(String, StreamGraphOperation)>,
        WhatAnAnswerNamesBesideItsOperations {
            what_the_answer_names_of_each_added_node,
            overridden_by_links,
        }: WhatAnAnswerNamesBesideItsOperations<'_>,
    ) -> GraphTransactionToolResult {
        let mut added_nodes = Vec::new();
        let mut added_groups = Vec::new();
        let mut added_notes = Vec::new();
        let mut connected_links = Vec::new();
        let mut graph_render_of_each_stream: HashMap<String, Option<Value>> = HashMap::new();
        let mut operations = Vec::with_capacity(applied_live.len());
        for (stream_id, applied) in applied_live {
            match &applied {
                StreamGraphOperation::AddNode { node, .. } => {
                    added_nodes.push(GraphTransactionAddedNode {
                        stream: stream_id.clone(),
                        node: node.id.to_string(),
                        name: node.name.clone(),
                        label: what_the_answer_names_of_each_added_node
                            .get(&node.id)
                            .and_then(|named| named.label.clone()),
                        inserted: what_the_answer_names_of_each_added_node
                            .get(&node.id)
                            .and_then(|named| named.inserted_because)
                            .map(|inserted_because| InsertedNodeOnTheWire {
                                node_type: node.node_type.clone(),
                                inserted_because,
                            }),
                    });
                }
                StreamGraphOperation::FoldIntoFragment { fold } => {
                    if let Some(fragment) = fold.fragment_node() {
                        added_nodes.push(GraphTransactionAddedNode {
                            stream: stream_id.clone(),
                            node: fragment.to_string(),
                            name: fold.fragment_name().to_owned(),
                            label: what_the_answer_names_of_each_added_node
                                .get(&fragment)
                                .and_then(|named| named.label.clone()),
                            inserted: None,
                        });
                    }
                }
                StreamGraphOperation::AddNote { note } => {
                    added_notes.push(GraphTransactionAddedNote {
                        stream: stream_id.clone(),
                        note: note.id.to_string(),
                    });
                }
                StreamGraphOperation::AddGroup { group, .. } => {
                    added_groups.push(GraphTransactionAddedGroup {
                        stream: stream_id.clone(),
                        group: group.id.to_string(),
                        title: group.fields.title.clone(),
                    });
                }
                _ => {}
            }
            let links_the_operation_connects = match &applied {
                StreamGraphOperation::Connect { link } => std::slice::from_ref(link),
                StreamGraphOperation::AddNode { links, .. } => links.as_slice(),
                _ => &[],
            };
            for link in links_the_operation_connects {
                let graph_render = graph_render_of_each_stream
                    .entry(stream_id.clone())
                    .or_insert_with(|| {
                        let stream_operations = stream_operations_of(&stream_id)?;
                        match self
                            .async_runtime
                            .block_on(stream_operations.to_json_async())
                        {
                            Ok(graph_render) => Some(graph_render),
                            Err(unreadable) => {
                                tracing::warn!(
                                    stream = %stream_id,
                                    "a link was connected and its stream's graph could not \
                                     be read to say what state it is in: {unreadable}"
                                );
                                None
                            }
                        }
                    });
                let HowTheGraphReadsALink {
                    state,
                    error_reason,
                    marks,
                } = graph_render
                    .as_ref()
                    .map(|graph_render| how_the_graph_reads_the_link(graph_render, link))
                    .unwrap_or_default();
                connected_links.push(GraphTransactionConnectedLink {
                    stream: stream_id.clone(),
                    source: link_end_on_the_wire(&link.source),
                    target: link_end_on_the_wire(&link.target),
                    state,
                    error_reason,
                    marks,
                });
            }
            let mut operation = serde_json::to_value(&applied).unwrap_or_default();
            if let Value::Object(operation) = &mut operation {
                operation.insert("stream".to_owned(), Value::String(stream_id));
            }
            operations.push(operation);
        }
        GraphTransactionToolResult {
            mode: mode.on_the_wire().to_owned(),
            transaction,
            commit,
            operations,
            added_nodes,
            connected_links,
            added_groups,
            added_notes,
            nodes_rebuilt: Vec::new(),
            overridden_by_links,
        }
    }
}

fn link_end_on_the_wire(end: &StreamGraphDocumentLinkEnd) -> GraphLinkEndOnTheWire {
    GraphLinkEndOnTheWire {
        node: end.node.to_string(),
        port: end.port.clone(),
    }
}

/// What the `graph` render of a link says of it: its `state`, its `error_reason`, and what it
/// converts and warns of.
#[derive(Default)]
struct HowTheGraphReadsALink {
    state: Option<String>,
    error_reason: Option<String>,
    marks: PinLinkMarksOnTheWire,
}

/// What the `graph` render `graph_render` says of `link`, whose ends it names by node name.
fn how_the_graph_reads_the_link(
    graph_render: &Value,
    link: &StreamGraphDocumentLink,
) -> HowTheGraphReadsALink {
    let node_name_of = |node: Uuid| {
        graph_render["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|rendered| rendered["id"].as_str() == Some(node.to_string().as_str()))
            .and_then(|rendered| rendered["name"].as_str())
            .map(str::to_owned)
    };
    let (Some(source_name), Some(target_name)) = (
        node_name_of(link.source.node),
        node_name_of(link.target.node),
    ) else {
        return HowTheGraphReadsALink::default();
    };
    let end_is = |end: &Value, name: &str, port: &str| {
        end["node"].as_str() == Some(name) && end["port"].as_str() == Some(port)
    };
    let Some(rendered_link) =
        graph_render["links"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|rendered| {
                end_is(&rendered["source"], &source_name, &link.source.port)
                    && end_is(&rendered["target"], &target_name, &link.target.port)
            })
    else {
        return HowTheGraphReadsALink::default();
    };
    let text_at = |key: &str| rendered_link[key].as_str().map(str::to_owned);
    let marks = PinLinkMarksOnTheWire::deserialize(rendered_link).unwrap_or_else(|not_read| {
        tracing::warn!(
            %not_read,
            "a connected link's pin marks could not be read from the render"
        );
        PinLinkMarksOnTheWire::default()
    });
    HowTheGraphReadsALink {
        state: text_at("state"),
        error_reason: text_at("error_reason"),
        marks,
    }
}
