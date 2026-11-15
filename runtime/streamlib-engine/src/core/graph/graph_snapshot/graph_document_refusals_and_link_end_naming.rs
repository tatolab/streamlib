// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Refusing what a graph document holds that this runtime does not read, and naming its link ends by node name or UUID.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::core::graph::{
    PortInfo, cast_exposed_name_to_url_safe, port_names_listed_for_a_refusal,
    the_input_ports_of_the_exposed_config_fields,
};
use crate::core::json_schema::LabelledNodeGroup;
use crate::core::processors::NodeTypesOneStreamResolves;
use crate::core::{Error, PortDirection, Result};

use super::{
    GRAPH_DOCUMENT_KEYS, GRAPH_FRAGMENT_KEYS, GRAPH_LINK_END_KEYS, GRAPH_LINK_KEYS,
    GRAPH_NODE_KEYS, GraphDocumentObject, GraphDocumentObjectKeys, GraphSnapshotLink,
    GraphSnapshotNode, LINK_END_KEYS_NAMING_A_PORT_OUTSIDE_THIS_STREAM,
    TRACK_RECEIVE_READS_ANOTHER_STREAMS_PORT,
};

/// Refuse the first key of `graph_document` the runtime does not read — at the
/// top, on a node, a fragment, a link or a link end, at every level — naming it and where it
/// sits, before the spec is read, so a misspelled key is named rather than
/// reported as the key it stands in for being missing. A link end naming a
/// machine or a stream is refused naming how a stream reads a port outside it.
/// An object the walk expects and does not find is left for the read to
/// refuse.
/// Refuse a labelled group of `groups`, one level's, whose `id` is no UUID or names another
/// group of the level too, and the same in every fragment `nodes` holds; `node_path` is the
/// path of names down to the level, each followed by `/`.
pub(super) fn refuse_a_group_of_no_uuid_or_one_uuid_twice(
    groups: &[LabelledNodeGroup],
    nodes: &[GraphSnapshotNode],
    node_path: &str,
) -> Result<()> {
    let level_named = || {
        if node_path.is_empty() {
            "the graph".to_string()
        } else {
            format!("fragment `{}`", node_path.trim_end_matches('/'))
        }
    };
    let mut group_uuids = HashSet::new();
    for group in groups {
        let group_uuid = Uuid::parse_str(&group.id).map_err(|_| {
            Error::GraphError(format!(
                "group `{}` of {} has the id `{}`, which is no UUID",
                group.title,
                level_named(),
                group.id
            ))
        })?;
        if !group_uuids.insert(group_uuid) {
            return Err(Error::GraphError(format!(
                "{} holds two groups of the UUID {group_uuid}",
                level_named()
            )));
        }
    }
    for node in nodes {
        if let Some(fragment) = &node.fragment {
            refuse_a_group_of_no_uuid_or_one_uuid_twice(
                &fragment.groups,
                &fragment.nodes,
                &format!("{node_path}{}/", node.name),
            )?;
        }
    }
    Ok(())
}

pub(super) fn refuse_a_key_this_runtime_does_not_read(
    graph_document: &serde_json::Value,
) -> Result<()> {
    if let Some(graph_object) = graph_document.as_object() {
        refuse_a_key_no_such_object_holds(graph_object, &GRAPH_DOCUMENT_KEYS, || {
            "the graph".to_string()
        })?;
    }
    refuse_a_key_one_level_does_not_read(graph_document, "")
}

/// Refuse the first key the runtime does not read on a node, a link or a link end of `level` —
/// the graph, or a fragment — and of every fragment it holds; `node_path` is the path of names
/// down to `level`, each followed by `/`.
fn refuse_a_key_one_level_does_not_read(level: &serde_json::Value, node_path: &str) -> Result<()> {
    fn text_at<'o>(object: &'o GraphDocumentObject, key: &str) -> &'o str {
        object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
    }
    fn port_address_of(object: &GraphDocumentObject) -> String {
        ["machine", "stream"]
            .into_iter()
            .filter(|leading_key| object.contains_key(*leading_key))
            .chain(["node", "port"])
            .map(|key| text_at(object, key))
            .collect::<Vec<&str>>()
            .join("/")
    }
    fn port_address_at(link: &GraphDocumentObject, end_key: &str) -> String {
        link.get(end_key)
            .and_then(serde_json::Value::as_object)
            .map_or_else(|| "?".to_string(), port_address_of)
    }
    let objects_of = |key: &str| {
        level
            .get(key)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_object)
    };

    for node in objects_of("nodes") {
        let node_named = || format!("node `{node_path}{}`", text_at(node, "name"));
        refuse_a_key_no_such_object_holds(node, &GRAPH_NODE_KEYS, node_named)?;
        let Some(fragment) = node.get("fragment") else {
            continue;
        };
        if let Some(fragment_object) = fragment.as_object() {
            refuse_a_key_no_such_object_holds(fragment_object, &GRAPH_FRAGMENT_KEYS, || {
                format!("the fragment of {}", node_named())
            })?;
        }
        refuse_a_key_one_level_does_not_read(
            fragment,
            &format!("{node_path}{}/", text_at(node, "name")),
        )?;
    }
    for link in objects_of("links") {
        let link_named = || {
            format!(
                "the link `{node_path}{}` → `{node_path}{}`",
                port_address_at(link, "source"),
                port_address_at(link, "target")
            )
        };
        let link_ends = ["source", "target"].into_iter().filter_map(|end_key| {
            let link_end = link.get(end_key).and_then(serde_json::Value::as_object)?;
            Some((link_end, move || {
                format!("the {end_key} end of {}", link_named())
            }))
        });
        // A link to another stream is refused naming the way to read one before
        // any key a runtime once rendered on such a link is refused unread.
        for (link_end, link_end_named) in link_ends.clone() {
            refuse_a_link_end_naming_a_port_outside_this_stream(link_end, link_end_named)?;
        }
        refuse_a_key_no_such_object_holds(link, &GRAPH_LINK_KEYS, link_named)?;
        for (link_end, link_end_named) in link_ends {
            refuse_a_key_no_such_object_holds(link_end, &GRAPH_LINK_END_KEYS, link_end_named)?;
        }
    }
    Ok(())
}

/// Name each link end of one level that names its node by UUID by that node's name instead,
/// and so for every fragment the level holds, refusing a UUID `node_name_by_uuid` already holds.
pub(super) fn name_each_link_end_of_one_level_by_its_nodes_name(
    nodes: &mut [GraphSnapshotNode],
    links: &mut [GraphSnapshotLink],
    node_name_by_uuid: &mut HashMap<Uuid, String>,
) -> Result<()> {
    let mut node_name_by_uuid_on_this_level: HashMap<Uuid, String> = HashMap::new();
    for node in nodes.iter_mut() {
        if let Some(node_uuid) = node.id {
            if let Some(holder) = node_name_by_uuid.insert(node_uuid, node.name.clone()) {
                return Err(Error::GraphError(format!(
                    "nodes `{holder}` and `{}` both have the UUID {node_uuid}; a node's UUID \
                     names it alone",
                    node.name
                )));
            }
            node_name_by_uuid_on_this_level.insert(node_uuid, node.name.clone());
        }
        if let Some(fragment) = &mut node.fragment {
            name_each_link_end_of_one_level_by_its_nodes_name(
                &mut fragment.nodes,
                &mut fragment.links,
                node_name_by_uuid,
            )?;
        }
    }
    for link in links {
        for end in [&mut link.source, &mut link.target] {
            if let Some(name) = Uuid::parse_str(&end.node)
                .ok()
                .and_then(|node_uuid| node_name_by_uuid_on_this_level.get(&node_uuid))
            {
                end.node = name.clone();
            }
        }
    }
    Ok(())
}

/// Name each link end of one level by its node's UUID, and so for every fragment the level
/// holds, refusing a node with no UUID.
pub(super) fn name_each_link_end_of_one_level_by_its_nodes_uuid(
    nodes: &mut [GraphSnapshotNode],
    links: &mut [GraphSnapshotLink],
) -> Result<()> {
    let mut node_uuid_by_name: HashMap<String, Uuid> = HashMap::new();
    for node in nodes.iter_mut() {
        let node_uuid = node.id.ok_or_else(|| {
            Error::GraphError(format!(
                "node `{}` has no UUID, which every node of a graph document has",
                node.name
            ))
        })?;
        node_uuid_by_name.insert(
            cast_exposed_name_to_url_safe(&node.name)?.into_owned(),
            node_uuid,
        );
        if let Some(fragment) = &mut node.fragment {
            name_each_link_end_of_one_level_by_its_nodes_uuid(
                &mut fragment.nodes,
                &mut fragment.links,
            )?;
        }
    }
    for link in links {
        for end in [&mut link.source, &mut link.target] {
            if let Some(node_uuid) =
                node_uuid_by_name.get(cast_exposed_name_to_url_safe(&end.node)?.as_ref())
            {
                end.node = node_uuid.to_string();
            }
        }
    }
    Ok(())
}

/// Refuse `link_end` when it names a machine or a stream, naming each and how
/// a stream reads a port outside it, where `link_end_named` says it sits.
fn refuse_a_link_end_naming_a_port_outside_this_stream(
    link_end: &GraphDocumentObject,
    link_end_named: impl FnOnce() -> String,
) -> Result<()> {
    let what_it_names: Vec<String> = LINK_END_KEYS_NAMING_A_PORT_OUTSIDE_THIS_STREAM
        .into_iter()
        .filter_map(|key| {
            link_end.get(key).map(|named| {
                let named = named
                    .as_str()
                    .map_or_else(|| named.to_string(), str::to_string);
                format!("the {key} `{named}`")
            })
        })
        .collect();
    if what_it_names.is_empty() {
        return Ok(());
    }
    Err(Error::GraphError(format!(
        "{} names {}: {TRACK_RECEIVE_READS_ANOTHER_STREAMS_PORT}",
        link_end_named(),
        what_it_names.join(" and ")
    )))
}

/// Refuse `object`'s first key that is neither a spec key nor a live key of its
/// kind, naming it where `object_named` says it sits.
fn refuse_a_key_no_such_object_holds(
    object: &GraphDocumentObject,
    keys: &GraphDocumentObjectKeys,
    object_named: impl FnOnce() -> String,
) -> Result<()> {
    let Some(unread_key) = object.keys().find(|key| {
        !keys.spec_keys.contains(&key.as_str()) && !keys.live_keys.contains(&key.as_str())
    }) else {
        return Ok(());
    };
    let live_keys_clause = if keys.live_keys.is_empty() {
        String::new()
    } else {
        format!(
            ", beside the live keys `graph` renders on it, which a load skips: {}",
            keys_listed_for_a_refusal(keys.live_keys)
        )
    };
    Err(Error::GraphError(format!(
        "{} carries the key `{unread_key}`, which this runtime does not read. The keys of {} \
         are {}{live_keys_clause}",
        object_named(),
        keys.object_kind,
        keys_listed_for_a_refusal(keys.spec_keys)
    )))
}

/// `keys` as "`a`, `b` and `c`".
fn keys_listed_for_a_refusal(keys: &[&str]) -> String {
    let quoted: Vec<String> = keys.iter().map(|key| format!("`{key}`")).collect();
    match quoted.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, before)) => format!("{} and {last}", before.join(", ")),
        None => String::new(),
    }
}

/// The input and output ports node `node_name` of a graph document has: those its type
/// declares and, among the inputs, one for each config field it exposes, which a placeholder
/// of a type not described already holds. `None` when the stream resolves no node type of
/// its `type`.
pub(crate) fn the_ports_of_a_graph_document_node(
    node_types: &NodeTypesOneStreamResolves,
    node_name: &str,
    node: &GraphSnapshotNode,
) -> Result<Option<(Vec<PortInfo>, Vec<PortInfo>)>> {
    let Some((mut inputs, outputs)) = node_types.port_info(&node.processor_type) else {
        return Ok(None);
    };
    if node_types
        .placeholder_of_the_node_type_not_described(&node.processor_type)
        .is_some()
    {
        return Ok(Some((inputs, outputs)));
    }
    let declared_ports: Vec<PortInfo> = inputs.iter().chain(&outputs).cloned().collect();
    inputs.extend(the_input_ports_of_the_exposed_config_fields(
        node_name,
        &node.processor_type,
        node_types,
        &declared_ports,
        &node.exposed_config_fields,
    )?);
    Ok(Some((inputs, outputs)))
}

/// Refuse `port` unless `node` has it in `direction`, whether its type declares it or it is
/// a config field the node exposes, listing the ports it does have there.
pub(crate) fn refuse_a_port_the_node_does_not_have(
    node_types: &NodeTypesOneStreamResolves,
    node_name: &str,
    node: &GraphSnapshotNode,
    port: &str,
    direction: PortDirection,
) -> Result<()> {
    let port_cast = cast_exposed_name_to_url_safe(port)?;
    let port_names: Vec<String> = the_ports_of_a_graph_document_node(node_types, node_name, node)?
        .map(|(inputs, outputs)| match direction {
            PortDirection::Input => inputs,
            PortDirection::Output => outputs,
        })
        .unwrap_or_default()
        .into_iter()
        .map(|declared| declared.name)
        .collect();
    if port_names.iter().any(|name| *name == port_cast) {
        return Ok(());
    }
    Err(Error::GraphError(format!(
        "node `{node_name}` has no {direction} port `{port}`. Its {direction} ports are: {}",
        port_names_listed_for_a_refusal(port_names.iter().map(String::as_str))
    )))
}

/// Refuse an integer literal outside `i64::MIN..=u64::MAX`, which `serde_json`
/// would otherwise read as the nearest `f64`, handing a node another number.
pub(crate) fn refuse_an_integer_literal_wider_than_64_bits(json: &str) -> Result<()> {
    let json_bytes = json.as_bytes();
    let mut byte_index = 0;
    while byte_index < json_bytes.len() {
        match json_bytes[byte_index] {
            b'"' => {
                byte_index += 1;
                while byte_index < json_bytes.len() && json_bytes[byte_index] != b'"' {
                    byte_index += if json_bytes[byte_index] == b'\\' {
                        2
                    } else {
                        1
                    };
                }
                byte_index += 1;
            }
            b'-' | b'0'..=b'9' => {
                let literal_start = byte_index;
                byte_index += 1;
                while byte_index < json_bytes.len()
                    && matches!(
                        json_bytes[byte_index],
                        b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'
                    )
                {
                    byte_index += 1;
                }
                let number_literal = &json[literal_start..byte_index];
                let integer_digits = number_literal.strip_prefix('-').unwrap_or(number_literal);
                let is_an_integer_literal = !integer_digits.is_empty()
                    && integer_digits.bytes().all(|byte| byte.is_ascii_digit());
                if is_an_integer_literal
                    && number_literal.parse::<i64>().is_err()
                    && number_literal.parse::<u64>().is_err()
                {
                    return Err(Error::GraphError(format!(
                        "the graph does not parse: the integer {number_literal} does not fit in 64 bits"
                    )));
                }
            }
            _ => byte_index += 1,
        }
    }
    Ok(())
}
