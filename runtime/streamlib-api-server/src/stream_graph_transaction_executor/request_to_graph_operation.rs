// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use streamlib_project_repository::NodeNotRead;

/// The node `node` names: its UUID, or the `as` of a node an earlier add of the transaction
/// adds, in `node_by_label`.
fn the_node_named(
    node: &str,
    node_by_label: &HashMap<String, Uuid>,
) -> Result<Uuid, GraphRequestRefusal> {
    node_by_label
        .get(node)
        .copied()
        .or_else(|| Uuid::parse_str(node).ok())
        .ok_or_else(|| {
            GraphRequestRefusal::of_a_node_not_found(
                node,
                format!(
                    "`{node}` is neither a node's UUID nor the `as` of a node an earlier \
                     `add_node` of this transaction adds"
                ),
            )
        })
}

/// The UUID of the node a `connect`'s end names, read as a transaction's `connect` reads one
/// with no earlier `add_node` to name: refused, as `connect` refuses it, when it is no UUID.
pub(crate) fn the_node_a_connects_end_names(node: &str) -> Result<Uuid, GraphRequestRefusal> {
    the_node_named(node, &HashMap::new())
}

/// The link from `source` to `target`, each node as [`the_node_named`] reads it.
pub(super) fn the_link_named(
    source: &StreamGraphActionLinkEnd,
    target: &StreamGraphActionLinkEnd,
    node_by_label: &HashMap<String, Uuid>,
) -> Result<StreamGraphDocumentLink, GraphRequestRefusal> {
    Ok(StreamGraphDocumentLink {
        source: StreamGraphDocumentLinkEnd {
            node: the_node_named(&source.node, node_by_label)?,
            port: source.port.clone(),
            member: source.member.clone(),
        },
        target: StreamGraphDocumentLinkEnd {
            node: the_node_named(&target.node, node_by_label)?,
            port: target.port.clone(),
            member: target.member.clone(),
        },
    })
}

/// What a `connect` asks of a graph: the link it names, made as it is, or a node of its own
/// added on that link and carrying it, and why the node is added.
pub(crate) enum WhatAConnectAsks {
    /// The link, made as it is.
    Link(StreamGraphDocumentLink),
    /// A node added on the link, carrying it through the node's input and output.
    ThroughANode {
        /// The addition of the node, with both links through it.
        operation: Box<StreamGraphOperation>,
        /// The link the `connect` named, from its output to its input.
        original_link: StreamGraphDocumentLink,
        /// Why the node is added.
        because: GraphTransactionNodeInsertedBecause,
    },
}

impl WhatAConnectAsks {
    /// The operation the graph is asked to make.
    pub(super) fn into_the_operation(self) -> StreamGraphOperation {
        match self {
            Self::Link(link) => StreamGraphOperation::Connect { link },
            Self::ThroughANode { operation, .. } => *operation,
        }
    }
}

/// What a connect from `source` to `target` asks of the graph `staged_graph` holds: the link
/// between them, as it is when `staged_graph` already holds it; when it closes a loop of
/// function nodes no `PreviousValue` node is in, a `PreviousValue` node added on it; and when its
/// two ends' pin types connect only through a built-in conversion, as `convert_node_a_link_needs`
/// judges it, that convert node added on it. A link `refuse_a_link_judged_before_commit` refuses
/// is refused, a loop-closing one among them, though no conversion is added on that.
pub(super) fn the_operation_a_connect_asks(
    source: &StreamGraphActionLinkEnd,
    target: &StreamGraphActionLinkEnd,
    staged_graph: &StreamGraphDocument,
    node_by_label: &HashMap<String, Uuid>,
    function_nodes_of_the_running_stream: &FunctionNodesOfTheRunningStream<'_>,
    convert_node_a_link_needs: &ConvertNodeALinkOfTheRunningStreamNeeds<'_>,
    refuse_a_link_judged_before_commit: &RefusalOfALinkJudgedBeforeCommit<'_>,
) -> Result<WhatAConnectAsks, GraphRequestRefusal> {
    let link = the_link_named(source, target, node_by_label)?;
    if staged_graph.links().contains(&link) {
        return Ok(WhatAConnectAsks::Link(link));
    }
    if the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in(
        staged_graph,
        &link,
        function_nodes_of_the_running_stream,
    )? {
        refuse_a_link_judged_before_commit(&link)?;
        return Ok(the_previous_value_node_added_on_the_link(
            staged_graph,
            link,
        ));
    }
    Ok(match convert_node_a_link_needs(&link)? {
        Some(convert_node) => the_convert_node_added_on_the_link(staged_graph, link, &convert_node),
        None => WhatAConnectAsks::Link(link),
    })
}

/// Refuse a connect of `link` when `graph` already holds it.
pub(super) fn refuse_a_link_the_graph_already_holds(
    graph: &StreamGraphDocument,
    link: &StreamGraphDocumentLink,
) -> Result<(), GraphRequestRefusal> {
    if graph.links().contains(link) {
        return Err(format!(
            "the graph already links {}/{} -> {}/{}",
            link.source.node, link.source.port, link.target.node, link.target.port
        )
        .into());
    }
    Ok(())
}

/// The operation `request` asks of the graph `staged_graph` holds, with everything its inverse
/// needs read from it; an added node takes a new UUID, recorded under its `as` when it has one,
/// and a node a `connect` adds on its link is recorded in `node_inserted_because` with why.
pub(super) fn graph_operation_for_the_request(
    request: StreamGraphAction,
    staged_graph: &StreamGraphDocument,
    node_by_label: &mut HashMap<String, Uuid>,
    what_the_request_reads: &WhatAGraphRequestReadsOfItsStreamAndProject<'_>,
    placed_stream_a_collapse_saves: &mut Option<PlacedStreamACollapseSaves>,
    node_inserted_because: &mut Option<GraphTransactionNodeInsertedBecause>,
) -> Result<StreamGraphOperation, GraphRequestRefusal> {
    let WhatAGraphRequestReadsOfItsStreamAndProject {
        run_the_graph_generator,
        place_the_imported_stream,
        document_of_a_placed_stream,
        pin_type_of_an_output_port,
        function_nodes_of_the_running_stream,
        convert_node_a_link_needs,
        refuse_a_link_judged_before_commit,
    } = what_the_request_reads;
    let label_the_node = |label: Option<String>,
                          node: Uuid,
                          node_by_label: &mut HashMap<String, Uuid>|
     -> Result<(), String> {
        match label {
            Some(label) if node_by_label.insert(label.clone(), node).is_some() => Err(format!(
                "two operations of this transaction both name a node `as` `{label}`"
            )),
            _ => Ok(()),
        }
    };
    match request {
        StreamGraphAction::ImportGraphGenerator {
            generator,
            name,
            label,
            ..
        } => {
            let mut placed = the_fragment_node_a_run_places(
                run_the_graph_generator,
                &generator,
                name.as_deref(),
                &Map::new(),
            )?;
            // A re-import runs again with the settings the node it refreshes holds: which node
            // that is, the run's name answers.
            if let Some(held) = staged_graph.node_named(&placed.name)
                && held.fragment.is_some()
                && held.input_values != placed.input_values
            {
                placed = the_fragment_node_a_run_places(
                    run_the_graph_generator,
                    &generator,
                    Some(&placed.name),
                    &held.input_values,
                )?;
            }
            let operation = match staged_graph.node_named(&placed.name) {
                None => {
                    let node = Uuid::new_v4();
                    let fragment = fragment_with_each_node_uuid_kept_by_its_name(
                        &placed.fragment,
                        None,
                        &mut Uuid::new_v4,
                    )?;
                    label_the_node(label, node, node_by_label)?;
                    StreamGraphOperation::AddNode {
                        node: StreamGraphDocumentNode {
                            id: node,
                            name: placed.name,
                            node_type: placed.generator,
                            input_values: placed.input_values,
                            exposed_config_fields: Vec::new(),
                            studio: Default::default(),
                            fragment: Some(Box::new(fragment)),
                        },
                        links: Vec::new(),
                    }
                }
                Some(
                    held @ StreamGraphDocumentNode {
                        fragment: Some(_), ..
                    },
                ) => {
                    label_the_node(label, held.id, node_by_label)?;
                    the_refresh_of_a_fragment_node_by_a_run(held, placed)?
                }
                Some(StreamGraphDocumentNode { id, node_type, .. }) => {
                    return Err(format!(
                        "node {id} `{}` is a `{node_type}`, no fragment, so no graph \
                         generator's run refreshes it; give the import another `name`",
                        placed.name
                    )
                    .into());
                }
            };
            Ok(operation)
        }
        StreamGraphAction::AddNode {
            node_type,
            name,
            studio,
            label,
            ..
        } => {
            let node = Uuid::new_v4();
            label_the_node(label, node, node_by_label)?;
            let input_values = config_holding_the_default_name_of_a_graph_port_or_variable(
                &node_type,
                Map::new(),
                staged_graph,
            );
            Ok(StreamGraphOperation::AddNode {
                node: StreamGraphDocumentNode {
                    id: node,
                    name: name.unwrap_or_default(),
                    node_type,
                    input_values,
                    exposed_config_fields: Vec::new(),
                    studio: studio
                        .map(StreamGraphNodeStudioFields::from_a_callers_studio)
                        .transpose()?
                        .map(Box::new)
                        .unwrap_or_default(),
                    fragment: None,
                },
                links: Vec::new(),
            })
        }
        StreamGraphAction::RemoveNode { node, .. } => {
            let node = the_node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::RemoveNode {
                node: the_node_the_document_holds(staged_graph, node)?,
                links: staged_graph.links_touching(node),
            })
        }
        StreamGraphAction::Connect { source, target, .. } => {
            let asked = the_operation_a_connect_asks(
                &source,
                &target,
                staged_graph,
                node_by_label,
                function_nodes_of_the_running_stream,
                convert_node_a_link_needs,
                refuse_a_link_judged_before_commit,
            )?;
            if let WhatAConnectAsks::ThroughANode { because, .. } = &asked {
                *node_inserted_because = Some(*because);
            }
            Ok(asked.into_the_operation())
        }
        StreamGraphAction::Disconnect { source, target, .. } => {
            Ok(StreamGraphOperation::Disconnect {
                link: the_link_named(&source, &target, node_by_label)?,
            })
        }
        StreamGraphAction::SetNodeInputValue {
            node,
            input,
            member,
            value,
            ..
        } => {
            let node = the_node_named(&node, node_by_label)?;
            let held = the_node_the_document_holds(staged_graph, node)?;
            if held.node_type != STREAM_FRAGMENT_NODE_TYPE
                && a_setting_its_generator_promotes(&held, &input)
            {
                return the_refresh_of_a_generated_fragment_with_one_setting_set(
                    held,
                    input,
                    member,
                    value,
                    run_the_graph_generator,
                );
            }
            if let Some(to) = the_input_values_of_a_get_choosing_a_variable(
                &held,
                &input,
                member.as_deref(),
                value.as_ref(),
                staged_graph,
            ) {
                return Ok(StreamGraphOperation::SetNodeConfig {
                    node: held.id,
                    from: Value::Object(held.input_values),
                    to: Value::Object(to),
                });
            }
            Ok(StreamGraphOperation::set_node_input_value_of(
                &held, input, member, value,
            ))
        }
        StreamGraphAction::RenameNode { node, name, .. } => {
            let node = the_node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::RenameNode {
                node,
                from: the_node_the_document_holds(staged_graph, node)?.name,
                to: name,
            })
        }
        StreamGraphAction::SetStudioFields { node, studio, .. } => {
            let node = the_node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::SetStudioFields {
                node,
                from: the_node_the_document_holds(staged_graph, node)?.studio,
                to: Box::new(StreamGraphNodeStudioFields::from_a_callers_studio(studio)?),
            })
        }
        StreamGraphAction::AddGroup {
            title,
            description,
            colour,
            nodes,
            ..
        } => {
            if title.is_empty() {
                return Err("a group needs a `title`".to_owned().into());
            }
            let nodes = nodes
                .iter()
                .map(|node| the_node_named(node, node_by_label))
                .collect::<Result<Vec<Uuid>, GraphRequestRefusal>>()?;
            Ok(StreamGraphOperation::AddGroup {
                group: StreamGraphDocumentGroup {
                    id: Uuid::new_v4(),
                    fields: StreamGraphGroupFields {
                        title,
                        description: description.unwrap_or_default(),
                        colour: colour.unwrap_or_default(),
                    },
                },
                nodes,
            })
        }
        StreamGraphAction::SetGroupFields {
            group,
            title,
            description,
            colour,
            ..
        } => {
            let from = staged_graph.group(the_group_named(&group)?)?;
            if title.as_deref() == Some("") {
                return Err("a group needs a `title`".to_owned().into());
            }
            let to = StreamGraphGroupFields {
                title: title.unwrap_or_else(|| from.fields.title.clone()),
                description: description.unwrap_or_else(|| from.fields.description.clone()),
                colour: colour.unwrap_or_else(|| from.fields.colour.clone()),
            };
            Ok(StreamGraphOperation::SetGroupFields {
                group: from.id,
                from: from.fields,
                to,
            })
        }
        StreamGraphAction::UseDependencyStream {
            project,
            dependency_stream,
            name,
            label,
            ..
        } => {
            let project = Uuid::parse_str(&project)
                .map_err(|_| format!("`{project}` is no project's UUID"))?;
            let dependency_stream = Uuid::parse_str(&dependency_stream)
                .map_err(|_| format!("`{dependency_stream}` is no stream asset's UUID"))?;
            let held_stream = json!({
                "asset": dependency_stream.to_string(),
                "from": project.to_string(),
            });
            let input_values = Map::from_iter([(
                STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY.to_owned(),
                held_stream.clone(),
            )]);
            let node = Uuid::new_v4();
            let (fragment, stream_name) = place_the_imported_stream(node, &held_stream)?;
            let name = name.unwrap_or(stream_name);
            let name = streamlib::sdk::graph::cast_exposed_name_to_url_safe(&name)
                .map_err(|casts_to_nothing| {
                    format!("the fragment name `{name}` names nothing: {casts_to_nothing}")
                })?
                .into_owned();
            if let Some(held) = staged_graph.node_named(&name) {
                return Err(if held.node_type == STREAM_FRAGMENT_NODE_TYPE
                    && held.input_values == input_values
                {
                    format!(
                        "node {} `{name}` already places this stream, and follows its import's \
                         folder as it is on disk, so there is nothing to place again",
                        held.id
                    )
                } else {
                    format!(
                        "node {} `{name}` is a `{}`; give the stream another `name`",
                        held.id, held.node_type
                    )
                }
                .into());
            }
            label_the_node(label, node, node_by_label)?;
            Ok(StreamGraphOperation::AddNode {
                node: StreamGraphDocumentNode {
                    id: node,
                    name,
                    node_type: STREAM_FRAGMENT_NODE_TYPE.to_owned(),
                    input_values,
                    exposed_config_fields: Vec::new(),
                    studio: Default::default(),
                    fragment: Some(Box::new(fragment)),
                },
                links: Vec::new(),
            })
        }
        StreamGraphAction::AddNote {
            text,
            position,
            size,
            ..
        } => Ok(StreamGraphOperation::AddNote {
            note: StreamGraphDocumentNote {
                id: Uuid::new_v4(),
                fields: StreamGraphNoteFields {
                    text,
                    position,
                    size,
                },
            },
        }),
        StreamGraphAction::SetNoteFields {
            note,
            text,
            position,
            size,
            ..
        } => {
            let from = staged_graph.note(the_note_named(&note)?)?;
            let to = StreamGraphNoteFields {
                text: text.unwrap_or_else(|| from.fields.text.clone()),
                position: position.unwrap_or_else(|| from.fields.position.clone()),
                size: size.unwrap_or_else(|| from.fields.size.clone()),
            };
            Ok(StreamGraphOperation::SetNoteFields {
                note: from.id,
                from: from.fields,
                to,
            })
        }
        StreamGraphAction::RemoveNote { note, .. } => Ok(StreamGraphOperation::RemoveNote {
            note: staged_graph.note(the_note_named(&note)?)?,
        }),
        StreamGraphAction::RemoveGroup { group, .. } => {
            let group = staged_graph.group(the_group_named(&group)?)?;
            Ok(StreamGraphOperation::RemoveGroup {
                nodes: staged_graph.nodes_of_the_group(group.id),
                group,
            })
        }
        StreamGraphAction::CollapseSelectionIntoSubgraph {
            nodes, name, label, ..
        } => {
            let nodes = nodes
                .iter()
                .map(|node| the_node_named(node, node_by_label))
                .collect::<Result<Vec<Uuid>, GraphRequestRefusal>>()?;
            let name = streamlib::sdk::graph::cast_exposed_name_to_url_safe(&name)
                .map_err(|casts_to_nothing| {
                    format!("the subgraph name `{name}` names nothing: {casts_to_nothing}")
                })?
                .into_owned();
            let fragment = Uuid::new_v4();
            let placed_stream = Uuid::new_v4();
            let kept = std::cell::RefCell::new(None);
            let collapsed = staged_graph
                .the_collapse_of_nodes_into_a_placed_stream(
                    &nodes,
                    fragment,
                    &name,
                    placed_stream,
                    &|end| {
                        pin_type_of_an_output_port(end)
                            .map_err(|refusal| refusal.words_kept_in(&kept))
                    },
                    &mut Uuid::new_v4,
                )
                .map_err(|why| GraphRequestRefusal::kept_in_or(&kept, why))?;
            let path = ProjectRelativeAssetPath::parse(&format!(
                "{}/{name}{GRAPH_DOCUMENT_FILE_SUFFIX}",
                ProjectAssetKind::Stream.standard_folder_in_a_project()
            ))?;
            *placed_stream_a_collapse_saves = Some(PlacedStreamACollapseSaves {
                placed_stream,
                document: StreamGraphDocument::from_value(
                    collapsed.placed_stream_document.clone(),
                )?,
                creation: ProjectOperation::StreamDocument(
                    ProjectStreamDocumentOperation::CreateStreamDocument {
                        stream: placed_stream,
                        path,
                        document: collapsed.placed_stream_document,
                    },
                ),
            });
            label_the_node(label, fragment, node_by_label)?;
            Ok(StreamGraphOperation::FoldIntoFragment {
                fold: collapsed.fold,
            })
        }
        StreamGraphAction::UnlockSubgraph { fragment, .. } => {
            let kept = std::cell::RefCell::new(None);
            Ok(StreamGraphOperation::UnfoldFragment {
                fold: staged_graph
                    .the_fold_a_fragment_unfolds(
                        the_node_named(&fragment, node_by_label)?,
                        &|held_stream| {
                            document_of_a_placed_stream(held_stream)
                                .map_err(|refusal| refusal.words_kept_in(&kept))
                        },
                        &mut Uuid::new_v4,
                    )
                    .map_err(|why| GraphRequestRefusal::kept_in_or(&kept, why))?,
            })
        }
    }
}

/// Whether `link`, once `staged_graph` holds it, closes a loop every node of which is a
/// function node and none a `PreviousValue` node: the loop the compiler refuses.
pub(super) fn the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in(
    staged_graph: &StreamGraphDocument,
    link: &StreamGraphDocumentLink,
    function_nodes_of_the_running_stream: &FunctionNodesOfTheRunningStream<'_>,
) -> Result<bool, GraphRequestRefusal> {
    let links = staged_graph.links();
    let nodes_reached_from = |node: Uuid, may_pass_through: &dyn Fn(Uuid) -> bool| {
        let mut reached = HashSet::from([node]);
        let mut nodes_to_walk_from = vec![node];
        while let Some(walked_from) = nodes_to_walk_from.pop() {
            for held in &links {
                if held.source.node == walked_from
                    && may_pass_through(held.target.node)
                    && reached.insert(held.target.node)
                {
                    nodes_to_walk_from.push(held.target.node);
                }
            }
        }
        reached
    };
    if !nodes_reached_from(link.target.node, &|_| true).contains(&link.source.node) {
        return Ok(false);
    }
    let previous_value_nodes: HashSet<Uuid> = staged_graph
        .nodes()
        .into_iter()
        .filter(|node| node.node_type == PREVIOUS_VALUE_NODE_TYPE)
        .map(|node| node.id)
        .collect();
    let function_nodes = function_nodes_of_the_running_stream()?;
    let may_be_in_the_loop =
        |node: Uuid| function_nodes.contains(&node) && !previous_value_nodes.contains(&node);
    Ok(may_be_in_the_loop(link.target.node)
        && nodes_reached_from(link.target.node, &may_be_in_the_loop).contains(&link.source.node))
}

/// The addition of a `PreviousValue` node to `staged_graph` with `link` carried through it,
/// drawn halfway between the two nodes `link` joins when the canvas places both.
pub(super) fn the_previous_value_node_added_on_the_link(
    staged_graph: &StreamGraphDocument,
    link: StreamGraphDocumentLink,
) -> WhatAConnectAsks {
    a_node_added_on_the_link(staged_graph, link, NodeCarryingTheLink::closing_a_loop())
}

/// The addition of `convert_node` to `staged_graph` with `link` carried through it: holding the
/// settings its conversion picked, drawn collapsed, halfway between the two nodes `link` joins
/// when the canvas places both.
fn the_convert_node_added_on_the_link(
    staged_graph: &StreamGraphDocument,
    link: StreamGraphDocumentLink,
    convert_node: &ConvertNodeToInsert,
) -> WhatAConnectAsks {
    a_node_added_on_the_link(
        staged_graph,
        link,
        NodeCarryingTheLink::converting_pin_types(convert_node),
    )
}

/// A node a connect adds on the link it asks for: its type, the input and output the link is
/// carried through, the input values it holds, whether the canvas draws it collapsed, and why it
/// is added.
struct NodeCarryingTheLink<'node> {
    node_type: &'node str,
    input_port: &'node str,
    output_port: &'node str,
    input_values: Map<String, Value>,
    collapsed: Option<bool>,
    because: GraphTransactionNodeInsertedBecause,
}

impl<'node> NodeCarryingTheLink<'node> {
    /// A `PreviousValue` node, added because the link closes a loop of function nodes.
    fn closing_a_loop() -> Self {
        Self {
            node_type: PREVIOUS_VALUE_NODE_TYPE,
            input_port: PREVIOUS_VALUE_NODE_INPUT_PORT,
            output_port: PREVIOUS_VALUE_NODE_OUTPUT_PORT,
            input_values: Map::new(),
            collapsed: None,
            because: GraphTransactionNodeInsertedBecause::ClosesALoop,
        }
    }

    /// `convert_node`, holding the settings its conversion picked and drawn collapsed, added
    /// because the link's two pin types connect only through it.
    fn converting_pin_types(convert_node: &'node ConvertNodeToInsert) -> Self {
        let conversion = &convert_node.conversion;
        Self {
            node_type: conversion.node_type.as_str(),
            input_port: &conversion.input_port_name,
            output_port: &conversion.output_port_name,
            input_values: convert_node.input_values.clone(),
            collapsed: Some(true),
            because: GraphTransactionNodeInsertedBecause::ConvertsPinTypes,
        }
    }
}

/// The addition of `carrying` to `staged_graph`, with no name of its own, `link` carried through
/// it, drawn halfway between the two nodes `link` joins when the canvas places both.
fn a_node_added_on_the_link(
    staged_graph: &StreamGraphDocument,
    link: StreamGraphDocumentLink,
    carrying: NodeCarryingTheLink<'_>,
) -> WhatAConnectAsks {
    let canvas_position_of = |node: Uuid| {
        let [x, y] = staged_graph.node(node).ok()?.studio.position?;
        Some((x.as_f64()?, y.as_f64()?))
    };
    let position_halfway_along_the_link = canvas_position_of(link.source.node)
        .zip(canvas_position_of(link.target.node))
        .and_then(|((source_x, source_y), (target_x, target_y))| {
            Some([
                serde_json::Number::from_f64((source_x + target_x) / 2.0)?,
                serde_json::Number::from_f64((source_y + target_y) / 2.0)?,
            ])
        });
    let added_node = Uuid::new_v4();
    let operation = StreamGraphOperation::AddNode {
        node: StreamGraphDocumentNode {
            id: added_node,
            name: String::new(),
            node_type: carrying.node_type.to_owned(),
            input_values: carrying.input_values,
            exposed_config_fields: Vec::new(),
            studio: Box::new(StreamGraphNodeStudioFields {
                position: position_halfway_along_the_link,
                collapsed: carrying.collapsed,
                ..StreamGraphNodeStudioFields::default()
            }),
            fragment: None,
        },
        links: vec![
            StreamGraphDocumentLink {
                source: link.source.clone(),
                target: StreamGraphDocumentLinkEnd {
                    node: added_node,
                    port: carrying.input_port.to_owned(),
                    member: None,
                },
            },
            StreamGraphDocumentLink {
                source: StreamGraphDocumentLinkEnd {
                    node: added_node,
                    port: carrying.output_port.to_owned(),
                    member: None,
                },
                target: link.target.clone(),
            },
        ],
    };
    WhatAConnectAsks::ThroughANode {
        operation: Box::new(operation),
        original_link: link,
        because: carrying.because,
    }
}

/// The node `node` of `document`, or the refusal coded `node_not_found` when the document holds
/// none of that UUID.
fn the_node_the_document_holds(
    document: &StreamGraphDocument,
    node: Uuid,
) -> Result<StreamGraphDocumentNode, GraphRequestRefusal> {
    document.node(node).map_err(|not_read| match not_read {
        NodeNotRead::NoSuchNode(why) => {
            GraphRequestRefusal::of_a_node_not_found(&node.to_string(), why)
        }
        NodeNotRead::DoesNotRead(why) => why.into(),
    })
}

fn the_note_named(note: &str) -> Result<Uuid, String> {
    Uuid::parse_str(note)
        .map_err(|_| format!("`{note}` is no note's UUID; `graph` lists each note's under `notes`"))
}

fn the_group_named(group: &str) -> Result<Uuid, String> {
    Uuid::parse_str(group).map_err(|_| {
        format!("`{group}` is no group's UUID; `graph` lists each group's under `groups`")
    })
}

/// The refresh of `held`, a fragment node, by `placed`, a fresh run of its generator: each node
/// the run builds keeps the UUID of the node of the same name `held` stores.
fn the_refresh_of_a_fragment_node_by_a_run(
    held: StreamGraphDocumentNode,
    placed: StreamGraphFragmentNodeAGeneratorRunPlaced,
) -> Result<StreamGraphOperation, GraphRequestRefusal> {
    let StreamGraphDocumentNode {
        id,
        node_type,
        input_values,
        fragment: Some(previous_fragment),
        ..
    } = held
    else {
        return Err(format!("node {} stores no fragment for a run to refresh", held.id).into());
    };
    let fragment = fragment_with_each_node_uuid_kept_by_its_name(
        &placed.fragment,
        Some(&previous_fragment),
        &mut Uuid::new_v4,
    )?;
    Ok(StreamGraphOperation::RefreshFragment {
        node: id,
        from: StreamGraphFragmentPlacement {
            generator: node_type,
            input_values,
            fragment: *previous_fragment,
        },
        to: StreamGraphFragmentPlacement {
            generator: placed.generator,
            input_values: placed.input_values,
            fragment,
        },
    })
}

/// The refresh of `held`, a fragment node a graph generator placed, run again with the value
/// `value` set on its setting `input`, or with the setting taken off when `value` is `None`,
/// beside every other value it holds. A setting has no member.
fn the_refresh_of_a_generated_fragment_with_one_setting_set(
    held: StreamGraphDocumentNode,
    input: String,
    member: Option<String>,
    value: Option<Value>,
    run_the_graph_generator: &GraphGeneratorRunOfTheStream<'_>,
) -> Result<StreamGraphOperation, GraphRequestRefusal> {
    if let Some(member) = member {
        return Err(format!(
            "node {} `{}` is placed by graph generator `{}`, whose setting `{input}` is set \
             whole and has no member `{member}`",
            held.id, held.name, held.node_type
        )
        .into());
    }
    let mut input_values = held.input_values.clone();
    match value {
        Some(value) => input_values.insert(input, value),
        None => input_values.remove(&input),
    };
    let placed = the_fragment_node_a_run_places(
        run_the_graph_generator,
        &held.node_type,
        Some(&held.name),
        &input_values,
    )?;
    the_refresh_of_a_fragment_node_by_a_run(held, placed)
}

/// Whether `input` is one of the settings the run of the generator that placed `held` declared,
/// which a set runs that generator again with; any other input of `held` is set as it holds it.
fn a_setting_its_generator_promotes(held: &StreamGraphDocumentNode, input: &str) -> bool {
    held.fragment
        .as_deref()
        .and_then(|fragment| fragment.get("settings"))
        .and_then(Value::as_array)
        .is_some_and(|settings| settings.iter().any(|setting| setting["name"] == input))
}

/// The fragment node one run of `generator`, named `name` and given `input_values`, places.
fn the_fragment_node_a_run_places(
    run_the_graph_generator: &GraphGeneratorRunOfTheStream<'_>,
    generator: &str,
    name: Option<&str>,
    input_values: &Map<String, Value>,
) -> Result<StreamGraphFragmentNodeAGeneratorRunPlaced, GraphRequestRefusal> {
    Ok(
        serde_json::from_value(run_the_graph_generator(generator, name, input_values)?).map_err(
            |unread| {
                format!("graph generator `{generator}`'s run placed no fragment node: {unread}")
            },
        )?,
    )
}

/// The fragment node one graph generator run places, as the run entry prints it: its nodes
/// and links by name.
#[derive(Deserialize)]
struct StreamGraphFragmentNodeAGeneratorRunPlaced {
    name: String,
    #[serde(rename = "type")]
    generator: String,
    #[serde(default)]
    input_values: Map<String, Value>,
    fragment: Value,
}

/// The input values a `GetVariable` takes when `value` is set on its `variable`, the variable
/// it reads chosen by name: the UUID of the setter of it on `staged_graph`'s level, so renaming
/// the variable later keeps the get reading it, in place of whichever setter it named; `None`
/// for any other set.
fn the_input_values_of_a_get_choosing_a_variable(
    held: &StreamGraphDocumentNode,
    input: &str,
    member: Option<&str>,
    value: Option<&Value>,
    staged_graph: &StreamGraphDocument,
) -> Option<Map<String, Value>> {
    let variable = value?.as_str().filter(|variable| !variable.is_empty())?;
    (held.node_type == GET_VARIABLE_NODE_TYPE
        && input == GRAPH_VARIABLE_NAME_CONFIG_KEY
        && member.is_none())
    .then(|| the_input_values_of_a_get_reading(variable, staged_graph))
}
