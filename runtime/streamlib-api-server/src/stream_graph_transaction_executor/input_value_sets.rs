// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The value sets of a graph transaction: each value checked by the engine before any edit of
//! the transaction is applied and again as it is applied, a `set_node_config` applied as the
//! set of each input value it changes, and a `set_exposed_config_fields` on a node type that
//! takes no config recorded as nothing.

use streamlib::sdk::descriptors::NodeClassImportPath;

use super::*;

/// Why an edit inside a placed stream, or placing one, is refused live.
pub(super) const DOCUMENTS_FIRST: &str =
    "it is made on the project's graph documents, with `mode` `commit`";

/// What a transaction has staged so far, which each of its actions reads and adds to: the
/// loaded streams it edits, the graph of each stream it edits that is not running, the streams
/// taking their edits once the documents hold them, the project it is committed to, and the
/// node each `as` names, the `as` of each node added, and why a `connect` inserted each node it
/// added on its link.
pub(super) struct WhatTheTransactionHasStaged<'repository> {
    pub(super) streams: BTreeMap<String, StreamATransactionEdits>,
    pub(super) staged_graph_of_each_stream_not_running: BTreeMap<Uuid, StreamGraphDocument>,
    pub(super) streams_taking_their_edits_once_the_documents_hold_them: BTreeSet<String>,
    pub(super) project_repository: Option<&'repository ProjectRepository>,
    pub(super) node_by_label: HashMap<String, Uuid>,
    pub(super) what_the_answer_names_of_each_added_node:
        HashMap<Uuid, WhatTheAnswerNamesOfAnAddedNode>,
}

/// Whether [`StreamGraphTransactionExecutor::apply_one_request`] checks a value it sets on an
/// input, or the caller checked it already against every value its action sets.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum InputValueCheck {
    MadeOnApply,
    MadeBefore,
}

/// A node, by the stream id, level and node name a transaction's action gives it.
type NodeAnActionNames = (String, Vec<Uuid>, String);

impl StreamGraphTransactionExecutor<'_> {
    /// Refuse, before any edit is applied, a value set on an input that the input does not take:
    /// each value of a `set_node_input_value`, `set_pin_value` or `set_node_config`, on a node
    /// the graph documents hold at any level or one an earlier `add_node` names by its `as`, the
    /// node holding each value the transaction set on it before. A value on a node this cannot
    /// find is checked as it is applied.
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
                    config,
                    pins,
                    label,
                    ..
                } => {
                    if let Some(stream) = staged.streams.get(stream_id)
                        && let Ok(added_type) = NodeClassImportPath::new(node_type)
                    {
                        let added_name = name.as_deref().or(label.as_deref()).unwrap_or(node_type);
                        for (input, value) in pins.iter().flatten() {
                            stream
                                .stream_operations
                                .refuse_a_saved_value_on_the_input_of_an_output_a_panel_writes(
                                    &added_type,
                                    added_name,
                                    input,
                                    Some(value).filter(|value| !value.is_null()),
                                )
                                .map_err(|refusal| {
                                    refused(GraphRequestRefusal::of_an_input_value(
                                        refusal,
                                        Some(format!("/pins/{input}")),
                                    ))
                                })?;
                        }
                    }
                    if let Some(label) = label {
                        let mut input_values = Map::new();
                        fold_into_input_values(
                            &mut input_values,
                            pins.clone().unwrap_or_default(),
                            config.clone().unwrap_or_default(),
                        );
                        node_as_the_transaction_leaves_it.insert(
                            (stream_id.clone(), level.clone(), label.clone()),
                            StreamGraphDocumentNode {
                                id: Uuid::nil(),
                                name: name.clone().unwrap_or_else(|| label.clone()),
                                node_type: node_type.clone(),
                                input_values,
                                exposed_config_fields: Vec::new(),
                                studio: Box::default(),
                                fragment: None,
                            },
                        );
                    }
                    continue;
                }
                StreamGraphAction::SetNodeInputValue { level, node, .. }
                | StreamGraphAction::SetPinValue { level, node, .. }
                | StreamGraphAction::SetNodeConfig { level, node, .. } => (level, node),
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
                StreamGraphAction::SetNodeConfig { config, .. } => {
                    the_input_value_sets_a_config_stands_for(
                        stream.stream_operations.as_ref(),
                        &set_on,
                        config,
                        None,
                    )
                    .map_err(refused)?
                }
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
                StreamGraphAction::SetPinValue { input, value, .. } => {
                    let value = value.as_ref().filter(|value| !value.is_null());
                    vec![
                        the_value_set_checked(
                            stream.stream_operations.as_ref(),
                            &set_on,
                            input,
                            None,
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

    /// The node `node`, by the transaction's `as` or its UUID, at `level` of the loaded stream
    /// `stream_id` as the transaction has staged it, when the engine checks its input values.
    fn the_node_an_action_sets_values_on(
        &self,
        stream_id: &str,
        level: &[Uuid],
        node: &str,
        staged: &WhatTheTransactionHasStaged<'_>,
    ) -> Option<StreamGraphDocumentNode> {
        let node = staged
            .node_by_label
            .get(node)
            .copied()
            .or_else(|| Uuid::parse_str(node).ok())?;
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
        .filter(its_input_values_are_checked)
    }

    /// Apply `request` as [`Self::apply_one_request`] does, as each operation it stands for: a
    /// `set_node_config` sets each input value it names and, on a node type that
    /// takes a config, takes off each config field it does not name; a `set_exposed_config_fields`
    /// on a node type that takes no config records nothing; a `disconnect` closes the gap it
    /// leaves in a pin group.
    pub(super) fn apply_one_action(
        &self,
        request: StreamGraphAction,
        stream_id: &str,
        staged: &mut WhatTheTransactionHasStaged<'_>,
    ) -> Result<Vec<OneGraphRequestApplied>, GraphRequestRefusal> {
        let stream_operations = staged
            .streams
            .get(stream_id)
            .map(|stream| Arc::clone(&stream.stream_operations))
            .ok_or_else(|| format!("stream `{stream_id}` is not loaded"))?;
        match &request {
            StreamGraphAction::SetNodeConfig {
                stream,
                level,
                node,
                config,
            } => {
                if let Some(configured) =
                    self.the_node_an_action_sets_values_on(stream_id, level, node, staged)
                {
                    let config_the_running_node_holds = if level.is_empty() {
                        let rendered_graph = self
                            .async_runtime
                            .block_on(stream_operations.to_json_async())?;
                        the_config_a_running_node_holds(&rendered_graph, configured.id)
                    } else {
                        None
                    };
                    let sets = the_input_value_sets_a_config_stands_for(
                        stream_operations.as_ref(),
                        &configured,
                        config,
                        config_the_running_node_holds.as_ref(),
                    )?;
                    return self.apply_each_input_value_set(
                        stream_id,
                        (*stream, level, node),
                        sets,
                        staged,
                    );
                }
            }
            StreamGraphAction::SetExposedConfigFields {
                level,
                node,
                exposed_config_fields,
                ..
            } => {
                if let Some(exposed_on) =
                    self.the_node_an_action_sets_values_on(stream_id, level, node, staged)
                    && !stream_operations.an_exposure_of_config_fields_is_recorded_on(
                        &the_node_type_of(&exposed_on)?,
                        exposed_config_fields,
                    )?
                {
                    return Ok(Vec::new());
                }
            }
            StreamGraphAction::Disconnect { .. } => {
                return self
                    .apply_a_disconnect_closing_the_gap_it_leaves(request, stream_id, staged);
            }
            _ => {}
        }
        self.apply_one_request(request, stream_id, staged, InputValueCheck::MadeOnApply)
            .map(|applied| vec![applied])
    }

    /// Apply each of `sets`, already checked, on the node `(stream, level, node)` names in the
    /// loaded stream `stream_id`, in order; one refused reverses those made live before it.
    fn apply_each_input_value_set(
        &self,
        stream_id: &str,
        (stream, level, node): (Uuid, &[Uuid], &str),
        sets: Vec<(String, Option<Value>)>,
        staged: &mut WhatTheTransactionHasStaged<'_>,
    ) -> Result<Vec<OneGraphRequestApplied>, GraphRequestRefusal> {
        let mut applied_by_the_sets: Vec<OneGraphRequestApplied> = Vec::with_capacity(sets.len());
        for (input, value) in sets {
            let set = StreamGraphAction::SetNodeInputValue {
                member: None,
                stream,
                level: level.to_vec(),
                node: node.to_owned(),
                input: input.clone(),
                value,
            };
            match self.apply_one_request(set, stream_id, staged, InputValueCheck::MadeBefore) {
                Ok(applied) => applied_by_the_sets.push(applied),
                Err(refusal) => {
                    let applied_live = applied_by_the_sets
                        .into_iter()
                        .filter_map(|applied| Some((stream_id.to_owned(), applied.applied_live?)))
                        .collect();
                    self.reverse_what_was_applied_live(&staged.streams, applied_live);
                    return Err(refusal.of_the_field(&input));
                }
            }
        }
        Ok(applied_by_the_sets)
    }
}

/// Whether the engine checks the input values of `node`: every node but a stream fragment and
/// a graph interface node, whose input values the runtime reads itself.
pub(super) fn its_input_values_are_checked(node: &StreamGraphDocumentNode) -> bool {
    node.fragment.is_none()
        && node.node_type != STREAM_FRAGMENT_NODE_TYPE
        && !is_a_graph_interface_node_type(&node.node_type)
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

/// What node `node` of a running stream holds on its input `input`: its saved value, its pin
/// value else its config field, and the value set live over it; so a set made on the running
/// node is reversed to the saved value with the live one set live again.
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
        saved: rendered_node["pins"]
            .get(input)
            .or_else(|| rendered_node["config"].get(input))
            .and_then(not_null),
        live: rendered_node["ports"]["inputs"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|port| port["name"] == input && port["value_comes_from"] == "live")
            .and_then(|port| not_null(&port["value"])),
    }
}

/// The config node `node` of `rendered_graph`, a running stream's `graph` render, holds; `None`
/// when the render holds no such node or it holds no config.
fn the_config_a_running_node_holds(
    rendered_graph: &Value,
    node: Uuid,
) -> Option<Map<String, Value>> {
    rendered_graph["nodes"]
        .as_array()?
        .iter()
        .find(|rendered| rendered["id"] == node.to_string())?["config"]
        .as_object()
        .cloned()
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

/// The input values `config`, sent to `set_node_config`, sets on `configured`, checked: each it
/// names, even one the graph document already holds, since the running node may hold another
/// set live, and, on a node type that takes a config, each config field it does not name taken
/// off, among them each `config_the_running_node_holds` holds live alone, so the config is
/// replaced whole.
fn the_input_value_sets_a_config_stands_for(
    stream_operations: &dyn RuntimeOperations,
    configured: &StreamGraphDocumentNode,
    config: &Map<String, Value>,
    config_the_running_node_holds: Option<&Map<String, Value>>,
) -> Result<Vec<(String, Option<Value>)>, GraphRequestRefusal> {
    let node_type = the_node_type_of(configured)?;
    let mut sets: Vec<(String, Option<Value>)> = config
        .iter()
        .map(|(input, value)| {
            (
                input.clone(),
                Some(value.clone()).filter(|value| !value.is_null()),
            )
        })
        .collect();
    if stream_operations.node_type_takes_a_config(&node_type) {
        let config_held = match stream_operations.the_config_a_node_holding_the_input_values_takes(
            &configured.node_type,
            &configured.input_values,
        )? {
            Value::Object(config_held) => config_held,
            Value::Null => Map::new(),
            other => {
                return Err(format!(
                    "node {} `{}` holds the config {other}, which is no object",
                    configured.id, configured.name
                )
                .into());
            }
        };
        let mut taken_off: Vec<&String> = config_held
            .keys()
            .filter(|input| {
                !config.contains_key(*input) && configured.input_values.contains_key(*input)
            })
            .collect();
        for held_live in config_the_running_node_holds
            .into_iter()
            .flat_map(Map::keys)
        {
            if !config.contains_key(held_live) && !taken_off.contains(&held_live) {
                taken_off.push(held_live);
            }
        }
        sets.extend(taken_off.into_iter().map(|input| (input.clone(), None)));
    }
    let mut input_values_after_the_sets = configured.input_values.clone();
    for (input, value) in &sets {
        input_values_after_the_sets =
            input_values_after_the_set(&input_values_after_the_sets, input, None, value.as_ref());
    }
    for (input, value) in &sets {
        refuse_a_value_the_input_does_not_take(
            stream_operations,
            configured,
            &input_values_after_the_sets,
            input,
            None,
            value.as_ref(),
            Some(&format!(
                "/config/{}",
                streamlib_control_contract::the_json_pointer_token_of(input)
            )),
        )
        .map_err(|refusal| refusal.of_the_field(input))?;
    }
    Ok(sets)
}
