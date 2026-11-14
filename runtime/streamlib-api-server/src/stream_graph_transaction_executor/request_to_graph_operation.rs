// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// The operation `request` asks of the graph `staged_graph` holds, with everything its inverse
/// needs read from it; an added node takes a new UUID, recorded under its `as` when it has one.
pub(super) fn graph_operation_for_the_request(
    request: StreamGraphAction,
    staged_graph: &StreamGraphDocument,
    node_by_label: &mut HashMap<String, Uuid>,
    what_the_request_reads: &WhatAGraphRequestReadsOfItsStreamAndProject<'_>,
    placed_stream_a_collapse_saves: &mut Option<PlacedStreamACollapseSaves>,
) -> Result<StreamGraphOperation, String> {
    let WhatAGraphRequestReadsOfItsStreamAndProject {
        run_the_graph_generator,
        place_the_imported_stream,
        document_of_a_placed_stream,
        family_of_an_output_port,
        function_nodes_of_the_running_stream,
    } = what_the_request_reads;
    let node_named = |node: &str, node_by_label: &HashMap<String, Uuid>| -> Result<Uuid, String> {
        node_by_label
            .get(node)
            .copied()
            .or_else(|| Uuid::parse_str(node).ok())
            .ok_or_else(|| {
                format!(
                    "`{node}` is neither a node's UUID nor the `as` of a node an earlier \
                     `add_node` of this transaction adds"
                )
            })
    };
    let link_between = |source: &StreamGraphActionLinkEnd,
                        target: &StreamGraphActionLinkEnd,
                        node_by_label: &HashMap<String, Uuid>|
     -> Result<StreamGraphDocumentLink, String> {
        Ok(StreamGraphDocumentLink {
            source: StreamGraphDocumentLinkEnd {
                node: node_named(&source.node, node_by_label)?,
                port: source.port.clone(),
            },
            target: StreamGraphDocumentLinkEnd {
                node: node_named(&target.node, node_by_label)?,
                port: target.port.clone(),
            },
        })
    };
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
            config,
            label,
            ..
        } => {
            let placed: StreamGraphFragmentNodeAGeneratorRunPlaced = serde_json::from_value(
                run_the_graph_generator(&generator, name.as_deref(), &config.unwrap_or_default())?,
            )
            .map_err(|unread| {
                format!("graph generator `{generator}`'s run placed no fragment node: {unread}")
            })?;
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
                            config: placed.config,
                            pins: Map::new(),
                            exposed_config_fields: Vec::new(),
                            studio: Default::default(),
                            fragment: Some(Box::new(fragment)),
                        },
                        links: Vec::new(),
                    }
                }
                Some(StreamGraphDocumentNode {
                    id,
                    node_type,
                    config,
                    fragment: Some(previous_fragment),
                    ..
                }) => {
                    let fragment = fragment_with_each_node_uuid_kept_by_its_name(
                        &placed.fragment,
                        Some(&previous_fragment),
                        &mut Uuid::new_v4,
                    )?;
                    label_the_node(label, id, node_by_label)?;
                    StreamGraphOperation::RefreshFragment {
                        node: id,
                        from: StreamGraphFragmentPlacement {
                            generator: node_type,
                            config,
                            fragment: *previous_fragment,
                        },
                        to: StreamGraphFragmentPlacement {
                            generator: placed.generator,
                            config: placed.config,
                            fragment,
                        },
                    }
                }
                Some(StreamGraphDocumentNode { id, node_type, .. }) => {
                    return Err(format!(
                        "node {id} `{}` is a `{node_type}`, no fragment, so no graph \
                         generator's run refreshes it; give the import another `name`",
                        placed.name
                    ));
                }
            };
            Ok(operation)
        }
        StreamGraphAction::AddNode {
            node_type,
            name,
            config,
            pins,
            exposed_config_fields,
            studio,
            label,
            ..
        } => {
            let node = Uuid::new_v4();
            label_the_node(label, node, node_by_label)?;
            let config = config_holding_the_default_name_of_a_graph_port_or_variable(
                &node_type,
                config.unwrap_or_default(),
                staged_graph,
            );
            Ok(StreamGraphOperation::AddNode {
                node: StreamGraphDocumentNode {
                    id: node,
                    name: name.unwrap_or_default(),
                    node_type,
                    config: Value::Object(config),
                    pins: pins.unwrap_or_default(),
                    exposed_config_fields: exposed_config_fields.unwrap_or_default(),
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
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::RemoveNode {
                node: staged_graph.node(node)?,
                links: staged_graph.links_touching(node),
            })
        }
        StreamGraphAction::Connect { source, target, .. } => {
            let link = link_between(&source, &target, node_by_label)?;
            if the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in(
                staged_graph,
                &link,
                function_nodes_of_the_running_stream,
            )? {
                Ok(the_previous_value_node_added_on_the_link(
                    staged_graph,
                    link,
                ))
            } else {
                Ok(StreamGraphOperation::Connect { link })
            }
        }
        StreamGraphAction::Disconnect { source, target, .. } => {
            Ok(StreamGraphOperation::Disconnect {
                link: link_between(&source, &target, node_by_label)?,
            })
        }
        StreamGraphAction::SetNodeConfig { node, config, .. } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::SetNodeConfig {
                node,
                from: staged_graph.node(node)?.config,
                to: Value::Object(config),
            })
        }
        StreamGraphAction::SetPinValue {
            node, input, value, ..
        } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::SetPinValue {
                node,
                from: staged_graph.node(node)?.pins.get(&input).cloned(),
                input,
                to: value.filter(|value| !value.is_null()),
            })
        }
        StreamGraphAction::RenameNode { node, name, .. } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::RenameNode {
                node,
                from: staged_graph.node(node)?.name,
                to: name,
            })
        }
        StreamGraphAction::SetStudioFields { node, studio, .. } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::SetStudioFields {
                node,
                from: staged_graph.node(node)?.studio,
                to: Box::new(StreamGraphNodeStudioFields::from_a_callers_studio(studio)?),
            })
        }
        StreamGraphAction::SetExposedConfigFields {
            node,
            exposed_config_fields,
            ..
        } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::SetExposedConfigFields {
                node,
                from: staged_graph.node(node)?.exposed_config_fields,
                to: exposed_config_fields,
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
                return Err("a group needs a `title`".to_owned());
            }
            let nodes = nodes
                .iter()
                .map(|node| node_named(node, node_by_label))
                .collect::<Result<Vec<Uuid>, String>>()?;
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
                return Err("a group needs a `title`".to_owned());
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
            let config = json!({ STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY: {
                "asset": dependency_stream.to_string(),
                "from": project.to_string(),
            }});
            let node = Uuid::new_v4();
            let (fragment, stream_name) =
                place_the_imported_stream(node, &config[STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY])?;
            let name = name.unwrap_or(stream_name);
            let name = streamlib::sdk::graph::cast_exposed_name_to_url_safe(&name)
                .map_err(|casts_to_nothing| {
                    format!("the fragment name `{name}` names nothing: {casts_to_nothing}")
                })?
                .into_owned();
            if let Some(held) = staged_graph.node_named(&name) {
                return Err(
                    if held.node_type == STREAM_FRAGMENT_NODE_TYPE && held.config == config {
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
                    },
                );
            }
            label_the_node(label, node, node_by_label)?;
            Ok(StreamGraphOperation::AddNode {
                node: StreamGraphDocumentNode {
                    id: node,
                    name,
                    node_type: STREAM_FRAGMENT_NODE_TYPE.to_owned(),
                    config,
                    pins: Map::new(),
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
                .map(|node| node_named(node, node_by_label))
                .collect::<Result<Vec<Uuid>, String>>()?;
            let name = streamlib::sdk::graph::cast_exposed_name_to_url_safe(&name)
                .map_err(|casts_to_nothing| {
                    format!("the subgraph name `{name}` names nothing: {casts_to_nothing}")
                })?
                .into_owned();
            let fragment = Uuid::new_v4();
            let placed_stream = Uuid::new_v4();
            let collapsed = staged_graph.the_collapse_of_nodes_into_a_placed_stream(
                &nodes,
                fragment,
                &name,
                placed_stream,
                family_of_an_output_port,
                &mut Uuid::new_v4,
            )?;
            let path = ProjectRelativeAssetPath::parse(&format!(
                "{}/{name}{GRAPH_DOCUMENT_FILE_SUFFIX}",
                ProjectAssetKind::Stream.standard_folder_in_a_project()
            ))
            .map_err(|refusal| refusal.to_string())?;
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
            Ok(StreamGraphOperation::UnfoldFragment {
                fold: staged_graph.the_fold_a_fragment_unfolds(
                    node_named(&fragment, node_by_label)?,
                    document_of_a_placed_stream,
                    &mut Uuid::new_v4,
                )?,
            })
        }
    }
}

pub(super) fn the_note_named(note: &str) -> Result<Uuid, String> {
    Uuid::parse_str(note)
        .map_err(|_| format!("`{note}` is no note's UUID; `graph` lists each note's under `notes`"))
}

pub(super) fn the_group_named(group: &str) -> Result<Uuid, String> {
    Uuid::parse_str(group).map_err(|_| {
        format!("`{group}` is no group's UUID; `graph` lists each group's under `groups`")
    })
}

/// The fragment node one graph generator run places, as the run entry prints it: its nodes
/// and links by name.
#[derive(Deserialize)]
pub(super) struct StreamGraphFragmentNodeAGeneratorRunPlaced {
    name: String,
    #[serde(rename = "type")]
    generator: String,
    config: Value,
    fragment: Value,
}
