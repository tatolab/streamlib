// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Map, Value};
use uuid::Uuid;

use crate::stream_document_upgrade::stream_document_in_the_current_shape;

use super::{
    GROUPS_KEY, STUDIO_GROUP_KEY, StreamGraphDocumentGroup, StreamGraphGroupFields, node_name_of,
    node_uuid_of,
};

/// `fresh_fragment`, a graph generator's output naming its nodes and link ends by name, with
/// each node given a UUID and each link end naming its node by it: the UUID the node of the
/// same name holds in `previous_fragment`, at the same level of nested fragments, and a new
/// one from `new_node_uuid` for a node no earlier run built. A generator that writes its nodes
/// in an older shape has them read in the current one.
pub fn fragment_with_each_node_uuid_kept_by_its_name(
    fresh_fragment: &Value,
    previous_fragment: Option<&Value>,
    new_node_uuid: &mut dyn FnMut() -> Uuid,
) -> Result<Value, String> {
    fragment_level_with_each_node_uuid_kept_by_its_name(
        &stream_document_in_the_current_shape(fresh_fragment.clone()),
        previous_fragment,
        new_node_uuid,
    )
}

fn fragment_level_with_each_node_uuid_kept_by_its_name(
    fresh_fragment: &Value,
    previous_fragment: Option<&Value>,
    new_node_uuid: &mut dyn FnMut() -> Uuid,
) -> Result<Value, String> {
    let Value::Object(fresh_level) = fresh_fragment else {
        return Err("a fragment is a JSON object of `nodes` and `links`".to_owned());
    };
    let previous_node_of_the_name = |name: &str| -> Option<&Map<String, Value>> {
        previous_fragment?
            .get("nodes")?
            .as_array()?
            .iter()
            .filter_map(Value::as_object)
            .find(|previous_node| node_name_of(previous_node) == Some(name))
    };
    let mut kept_level = fresh_level.clone();
    let mut uuid_of_the_name: Map<String, Value> = Map::new();
    let mut nodes = Vec::new();
    for fresh_node in fresh_level
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Value::Object(fresh_node) = fresh_node else {
            return Err("a fragment's node is a JSON object".to_owned());
        };
        let name = node_name_of(fresh_node)
            .ok_or("a fragment's node holds its `name`")?
            .to_owned();
        let previous_node = previous_node_of_the_name(&name);
        let node_uuid = previous_node
            .and_then(node_uuid_of)
            .unwrap_or_else(&mut *new_node_uuid);
        let mut kept_node = fresh_node.clone();
        kept_node.insert("id".to_owned(), Value::String(node_uuid.to_string()));
        if let Some(inner_fragment) = fresh_node.get("fragment") {
            kept_node.insert(
                "fragment".to_owned(),
                fragment_level_with_each_node_uuid_kept_by_its_name(
                    inner_fragment,
                    previous_node.and_then(|previous_node| previous_node.get("fragment")),
                    new_node_uuid,
                )?,
            );
        }
        uuid_of_the_name.insert(name, Value::String(node_uuid.to_string()));
        nodes.push(Value::Object(kept_node));
    }
    let mut links = Vec::new();
    for fresh_link in fresh_level
        .get("links")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let mut kept_link = fresh_link.clone();
        for end_key in ["source", "target"] {
            if let Some(Value::Object(end)) = kept_link.get_mut(end_key)
                && let Some(node_uuid) = end
                    .get("node")
                    .and_then(Value::as_str)
                    .and_then(|named| uuid_of_the_name.get(named))
                    .cloned()
            {
                end.insert("node".to_owned(), node_uuid);
            }
        }
        links.push(kept_link);
    }
    if let Some(fresh_groups) = fresh_level.get(GROUPS_KEY) {
        let groups = groups_with_each_uuid_kept_by_its_title(
            fresh_groups,
            previous_fragment.and_then(|previous| previous.get(GROUPS_KEY)),
            &uuid_of_the_name,
            &mut nodes,
            new_node_uuid,
        )?;
        kept_level.insert(GROUPS_KEY.to_owned(), Value::Array(groups));
    }
    kept_level.insert("nodes".to_owned(), Value::Array(nodes));
    kept_level.insert("links".to_owned(), Value::Array(links));
    Ok(Value::Object(kept_level))
}

/// `fresh_groups`, the groups a graph generator made of one level's nodes, each naming its
/// nodes by name, as the document holds them: each given the UUID the group of the same title
/// holds in `previous_groups`, or a new one from `new_group_uuid`, and each node it names given
/// that UUID as its `studio.group` in `nodes`.
fn groups_with_each_uuid_kept_by_its_title(
    fresh_groups: &Value,
    previous_groups: Option<&Value>,
    uuid_of_the_name: &Map<String, Value>,
    nodes: &mut [Value],
    new_group_uuid: &mut dyn FnMut() -> Uuid,
) -> Result<Vec<Value>, String> {
    let previous_groups: Vec<StreamGraphDocumentGroup> = previous_groups
        .and_then(|previous| serde_json::from_value(previous.clone()).ok())
        .unwrap_or_default();
    let mut groups = Vec::new();
    for fresh_group in fresh_groups
        .as_array()
        .ok_or("a fragment's `groups` is an array")?
    {
        let fields: StreamGraphGroupFields = serde_json::from_value(Value::Object(
            fresh_group
                .as_object()
                .ok_or("a fragment's group is a JSON object")?
                .iter()
                .filter(|(key, _)| key.as_str() != "nodes")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ))
        .map_err(|not_read| format!("a fragment's group does not read: {not_read}"))?;
        let group_uuid = previous_groups
            .iter()
            .find(|previous| previous.fields.title == fields.title)
            .map_or_else(&mut *new_group_uuid, |previous| previous.id);
        for named in fresh_group["nodes"].as_array().into_iter().flatten() {
            let node_name = named.as_str().unwrap_or_default();
            let node_uuid = uuid_of_the_name.get(node_name).ok_or_else(|| {
                format!(
                    "group `{}` names node `{node_name}`, which the fragment does not hold",
                    fields.title
                )
            })?;
            let node_object = nodes
                .iter_mut()
                .filter_map(Value::as_object_mut)
                .find(|node_object| node_object.get("id") == Some(node_uuid))
                .ok_or_else(|| format!("the fragment holds no node {node_uuid}"))?;
            let studio = node_object
                .entry("studio")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(studio) = studio {
                studio.insert(
                    STUDIO_GROUP_KEY.to_owned(),
                    Value::String(group_uuid.to_string()),
                );
            }
        }
        groups.push(
            serde_json::to_value(StreamGraphDocumentGroup {
                id: group_uuid,
                fields,
            })
            .map_err(|not_written| format!("a group does not write: {not_written}"))?,
        );
    }
    Ok(groups)
}
