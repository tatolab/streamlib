// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl StreamGraphTransactionExecutor<'_> {
    /// The answer of `applied_before`, the transaction of the project `project` an action sent
    /// again under its key first applied, each edit of a loaded stream under its stream id.
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
            &HashMap::new(),
            Vec::new(),
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "each argument is a separate piece of the transaction the caller already holds"
    )]
    pub(super) fn the_answer_of_a_graph_transaction(
        &self,
        mode: GraphEditMode,
        transaction: Option<String>,
        commit: Option<String>,
        stream_operations_of: &dyn Fn(&str) -> Option<Arc<dyn RuntimeOperations>>,
        applied_live: Vec<(String, StreamGraphOperation)>,
        labels_of_added_nodes: &HashMap<Uuid, String>,
        overridden_by_links: Vec<GraphTransactionInputOverriddenByALink>,
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
                        label: labels_of_added_nodes.get(&node.id).cloned(),
                    });
                }
                StreamGraphOperation::FoldIntoFragment { fold } => {
                    if let Some(fragment) = fold.fragment_node() {
                        added_nodes.push(GraphTransactionAddedNode {
                            stream: stream_id.clone(),
                            node: fragment.to_string(),
                            name: fold.fragment_name().to_owned(),
                            label: labels_of_added_nodes.get(&fragment).cloned(),
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
                let (state, error_reason) = graph_render
                    .as_ref()
                    .map(|graph_render| how_the_graph_reads_the_link(graph_render, link))
                    .unwrap_or_default();
                connected_links.push(GraphTransactionConnectedLink {
                    stream: stream_id.clone(),
                    source: link_end_on_the_wire(&link.source),
                    target: link_end_on_the_wire(&link.target),
                    state,
                    error_reason,
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

/// The `state` and `error_reason` the `graph` render `graph_render` gives `link`, whose ends it
/// names by node name.
fn how_the_graph_reads_the_link(
    graph_render: &Value,
    link: &StreamGraphDocumentLink,
) -> (Option<String>, Option<String>) {
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
        return (None, None);
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
        return (None, None);
    };
    let text_at = |key: &str| rendered_link[key].as_str().map(str::to_owned);
    (text_at("state"), text_at("error_reason"))
}
