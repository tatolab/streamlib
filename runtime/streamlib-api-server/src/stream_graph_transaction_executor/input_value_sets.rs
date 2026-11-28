// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The value sets of a graph transaction: each value checked by the engine before any edit of
//! the transaction is applied and again as it is applied.

use streamlib::sdk::descriptors::NodeClassImportPath;

use super::*;

/// Why an edit inside a placed stream, or placing one, is refused live.
pub(super) const DOCUMENTS_FIRST: &str =
    "it is made on the project's graph documents, with `mode` `commit`";

/// What a transaction has staged so far, which each of its actions reads and adds to: the
/// loaded streams it edits, the graph of each stream it edits that is not running, the streams
/// taking their edits through their staged graph, the project it is committed to, and the
/// node each `as` names, the `as` of each node added, and why a `connect` inserted each node it
/// added on its link.
pub(super) struct WhatTheTransactionHasStaged<'transaction> {
    pub(super) streams: BTreeMap<String, StreamATransactionEdits>,
    pub(super) staged_graph_of_each_stream_not_running: BTreeMap<Uuid, StreamGraphDocument>,
    pub(super) streams_taking_their_edits_through_their_staged_graph: BTreeSet<String>,
    pub(super) project_repository: Option<&'transaction ProjectRepository>,
    pub(super) node_by_label: HashMap<String, Uuid>,
    pub(super) what_the_answer_names_of_each_added_node:
        HashMap<Uuid, WhatTheAnswerNamesOfAnAddedNode>,
    /// What became of each saved value the transaction retypes, worked out before any lock.
    pub(super) saved_value_retypes_worked_out_before_the_edit:
        &'transaction SavedValueRetypesWorkedOutBeforeTheEdit,
}

/// A node, by the stream id, level and node name a transaction's action gives it.
type NodeAnActionNames = (String, Vec<Uuid>, String);

impl StreamGraphTransactionExecutor<'_> {
    /// Refuse, before any edit is applied, a value set on an input that the input does not take:
    /// each value of a `set_node_input_value`, on a node the graph documents hold at any level
    /// or one an earlier `add_node` names by its `as`, the node holding each value the
    /// transaction set on it before. A value on a node this cannot find is checked as it is
    /// applied.
    pub(super) fn refuse_a_value_an_input_does_not_take_before_any_is_applied(
        &self,
        requests_and_the_stream_id_each_names: &[(StreamGraphAction, String)],
        staged: &WhatTheTransactionHasStaged<'_>,
    ) -> Result<(), GraphTransactionRefusal> {
        let mut node_as_the_transaction_leaves_it: HashMap<
            NodeAnActionNames,
            StreamGraphDocumentNode,
        > = HashMap::new();
        for (request_index, (request, stream_id)) in
            requests_and_the_stream_id_each_names.iter().enumerate()
        {
            let refused = |refusal: GraphRequestRefusal| {
                GraphTransactionRefusal::of_the_operation(
                    request_index,
                    what_a_graph_action_names(request),
                    refusal,
                )
            };
            let (level, node) = match request {
                StreamGraphAction::AddNode {
                    level,
                    node_type,
                    name,
                    label: Some(label),
                    ..
                } => {
                    node_as_the_transaction_leaves_it.insert(
                        (stream_id.clone(), level.clone(), label.clone()),
                        StreamGraphDocumentNode {
                            id: Uuid::nil(),
                            name: name.clone().unwrap_or_else(|| label.clone()),
                            node_type: node_type.clone(),
                            input_values: Map::new(),
                            exposed_config_fields: Vec::new(),
                            studio: Box::default(),
                            fragment: None,
                        },
                    );
                    continue;
                }
                StreamGraphAction::SetNodeInputValue { level, node, .. } => (level, node),
                _ => continue,
            };
            let Some(stream) = staged.streams.get(stream_id) else {
                continue;
            };
            let named = (stream_id.clone(), level.clone(), node.clone());
            let Some(mut set_on) = node_as_the_transaction_leaves_it
                .get(&named)
                .cloned()
                .or_else(|| self.the_node_the_documents_hold(stream_id, level, node, staged))
                .filter(its_input_values_are_checked)
            else {
                continue;
            };
            let sets = match request {
                StreamGraphAction::SetNodeInputValue {
                    input,
                    member,
                    value,
                    ..
                } => {
                    let value = value.as_ref().filter(|value| !value.is_null());
                    vec![
                        the_value_set_checked(
                            stream.stream_operations.as_ref(),
                            &set_on,
                            input,
                            member.as_deref(),
                            value,
                        )
                        .map_err(refused)?,
                    ]
                }
                _ => continue,
            };
            for (input, value) in &sets {
                set_on.input_values =
                    input_values_after_the_set(&set_on.input_values, input, None, value.as_ref());
            }
            node_as_the_transaction_leaves_it.insert(named, set_on);
        }
        Ok(())
    }

    /// The node `node`, a UUID, at `level` of the loaded stream `stream_id`, as the graph
    /// documents hold it before the transaction; `None` when they hold none.
    fn the_node_the_documents_hold(
        &self,
        stream_id: &str,
        level: &[Uuid],
        node: &str,
        staged: &WhatTheTransactionHasStaged<'_>,
    ) -> Option<StreamGraphDocumentNode> {
        let node = Uuid::parse_str(node).ok()?;
        self.the_level_a_request_reaches(
            stream_id,
            level,
            &staged.streams,
            &staged.staged_graph_of_each_stream_not_running,
            staged.project_repository,
        )
        .ok()?
        .the_document_as_drawn()
        .ok()?
        .node(node)
        .ok()
    }

    /// Apply `request` as [`Self::apply_one_request`] does, as each operation it stands for: a
    /// `disconnect` closes the gap it leaves in a pin group; a `set_node_input_value` choosing
    /// another type for a `GraphInput` carries its saved value into that type.
    pub(super) fn apply_one_action(
        &self,
        request: StreamGraphAction,
        stream_id: &str,
        staged: &mut WhatTheTransactionHasStaged<'_>,
    ) -> Result<Vec<OneGraphRequestApplied>, GraphRequestRefusal> {
        match &request {
            StreamGraphAction::Disconnect { .. } => {
                self.apply_a_disconnect_closing_the_gap_it_leaves(request, stream_id, staged)
            }
            StreamGraphAction::SetNodeInputValue { .. } => {
                self.apply_a_set_carrying_a_retyped_saved_value(request, stream_id, staged)
            }
            _ => self
                .apply_one_request(request, stream_id, staged)
                .map(|applied| vec![applied]),
        }
    }
}

/// Whether the engine checks the input values of `node`: every node but a stream fragment and
/// a node of a type the graph resolves itself, whose input values the runtime reads itself.
pub(super) fn its_input_values_are_checked(node: &StreamGraphDocumentNode) -> bool {
    node.fragment.is_none()
        && node.node_type != STREAM_FRAGMENT_NODE_TYPE
        && !is_a_node_type_of_the_graph_itself(&node.node_type)
}

/// The link of `level` that feeds input `input` of `node`, which overrides a value set on it.
pub(super) fn the_link_overriding_the_input(
    level: &StreamGraphDocument,
    stream_id: &str,
    node: Uuid,
    input: &str,
) -> Option<GraphTransactionInputOverriddenByALink> {
    level
        .links_touching(node)
        .into_iter()
        .find(|link| link.target.node == node && link.target.port == input)
        .map(|link| GraphTransactionInputOverriddenByALink {
            stream: stream_id.to_owned(),
            node: node.to_string(),
            input: input.to_owned(),
            from_node: link.source.node.to_string(),
            from_output: link.source.port,
        })
}

/// What node `node` of a running stream holds on its input `input`: the input value it saves
/// and the value set live over it; so a set made on the running node is reversed to the saved
/// value with the live one set live again.
pub(super) struct ValueARunningInputHolds {
    pub(super) saved: Option<Value>,
    pub(super) live: Option<Value>,
}

/// What node `node` of `rendered_graph`, a running stream's `graph` render, holds on its input
/// `input`, saved and live; neither when the render holds no such node.
pub(super) fn the_value_a_running_node_holds_on_the_input(
    rendered_graph: &Value,
    node: Uuid,
    input: &str,
) -> ValueARunningInputHolds {
    let Some(rendered_node) = rendered_graph["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|rendered| rendered["id"] == node.to_string())
    else {
        return ValueARunningInputHolds {
            saved: None,
            live: None,
        };
    };
    let not_null = |value: &Value| (!value.is_null()).then(|| value.clone());
    ValueARunningInputHolds {
        saved: rendered_node["input_values"].get(input).and_then(not_null),
        live: rendered_node["ports"]["inputs"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|port| port["name"] == input && port["value_comes_from"] == "live")
            .and_then(|port| not_null(&port["value"])),
    }
}

/// The set of `value` on input `input` of `set_on`, or on its member `member` when one is named,
/// checked, as the set of the whole value the input holds after it.
fn the_value_set_checked(
    stream_operations: &dyn RuntimeOperations,
    set_on: &StreamGraphDocumentNode,
    input: &str,
    member: Option<&str>,
    value: Option<&Value>,
) -> Result<(String, Option<Value>), GraphRequestRefusal> {
    let input_values_after_the_set =
        input_values_after_the_set(&set_on.input_values, input, member, value);
    refuse_a_value_the_input_does_not_take(
        stream_operations,
        set_on,
        &input_values_after_the_set,
        input,
        member,
        value,
        Some("/value"),
    )?;
    Ok((
        input.to_owned(),
        input_values_after_the_set.get(input).cloned(),
    ))
}

/// The type of `node`, refused when it names none.
fn the_node_type_of(node: &StreamGraphDocumentNode) -> Result<NodeClassImportPath, String> {
    NodeClassImportPath::new(&node.node_type)
        .map_err(|refusal| format!("node {} `type`: {refusal}", node.id))
}

/// Refuse `value`, set on input `input` of `set_on`, or on its member `member` when one is
/// named, with `None` taking it off, when the input does not take it, as the stream
/// `stream_operations` runs resolves the node's type; the value is at `refused_params_pointer`
/// in the action's params, and in none when the runtime made the set itself, as an undo or redo
/// does.
pub(super) fn refuse_a_value_the_input_does_not_take(
    stream_operations: &dyn RuntimeOperations,
    set_on: &StreamGraphDocumentNode,
    input_values_after_the_set: &Map<String, Value>,
    input: &str,
    member: Option<&str>,
    value: Option<&Value>,
    refused_params_pointer: Option<&str>,
) -> Result<(), GraphRequestRefusal> {
    stream_operations
        .refuse_an_input_value_its_input_does_not_take(
            &the_node_type_of(set_on)?,
            &set_on.name,
            input_values_after_the_set,
            input,
            member,
            value,
        )
        .map_err(|refusal| {
            GraphRequestRefusal::of_an_input_value(
                refusal,
                refused_params_pointer.map(str::to_owned),
            )
        })
}
