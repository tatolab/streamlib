// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Numbered inputs of one node type that became the slots of a pin group: `layer_0` to
//! `layer_7` and the `layers` list that placed them became the group `layers`, slot `n` holding
//! `layers.<n>.picture` and the values entry `n` held. A saved document opens with each wire into
//! a numbered input on its slot's member, and each list entry's values on its slot's members.

use serde_json::{Map, Value};

use super::NODE_INPUT_VALUES_KEY;

/// What separates a group's name, a slot's index and a member's name in a slot's port.
const PIN_GROUP_SLOT_PORT_NAME_SEPARATOR: char = '.';

/// The numbered inputs and the list of one node type that became the slots of a pin group.
#[derive(Debug, Clone, Copy)]
pub(super) struct InputsMovedOntoPinGroupSlots {
    /// The node type's path, after any rename.
    pub(super) node_type: &'static str,
    /// The group the inputs became.
    pub(super) group: &'static str,
    /// Each numbered input, by what its name was before its slot's index, `layer_`, and the
    /// member of the slot it became, `picture`.
    pub(super) numbered_inputs: &'static [(&'static str, &'static str)],
    /// The list input whose entry `n`, a map of member to value, became slot `n`'s values.
    pub(super) list_input: Option<&'static str>,
}

/// Each node type whose numbered inputs became the slots of a pin group.
pub(super) const INPUTS_MOVED_ONTO_PIN_GROUP_SLOTS: &[InputsMovedOntoPinGroupSlots] =
    &[InputsMovedOntoPinGroupSlots {
        node_type: "tatolab.compositor.alpha_over_compositor:AlphaOverCompositor",
        group: "layers",
        numbered_inputs: &[("layer_", "picture")],
        list_input: Some("layers"),
    }];

/// Move the numbered inputs of each node of one level that `rows` name onto their slots, in
/// place, each wire in `links` into one retargeted; whether any moved.
pub(super) fn the_inputs_of_the_level_moved_onto_their_slots(
    nodes: &mut [Value],
    links: &mut [Value],
    rows: &[InputsMovedOntoPinGroupSlots],
) -> bool {
    let mut any_moved = false;
    for node in nodes.iter_mut() {
        if let Some(node) = node.as_object_mut() {
            any_moved |= the_inputs_of_a_node_moved_onto_their_slots(node, links, rows);
        }
    }
    any_moved
}

/// Move the numbered inputs of `node` that `rows` name onto their slots, in place: each wire in
/// `links` into one retargeted, and each list entry's values put on its slot; whether any moved.
pub(super) fn the_inputs_of_a_node_moved_onto_their_slots(
    node: &mut Map<String, Value>,
    links: &mut [Value],
    rows: &[InputsMovedOntoPinGroupSlots],
) -> bool {
    let Some(node_type) = node.get("type").and_then(Value::as_str) else {
        return false;
    };
    let Some(row) = rows.iter().find(|row| row.node_type == node_type) else {
        return false;
    };
    let names_of_the_node: Vec<String> = ["id", "name"]
        .into_iter()
        .filter_map(|key| node.get(key).and_then(Value::as_str).map(str::to_owned))
        .collect();
    let mut any_moved = false;
    for link in links.iter_mut() {
        any_moved |= the_link_retargeted_onto_its_slot(link, &names_of_the_node, row);
    }
    any_moved |= the_list_moved_onto_its_slots(node, row);
    any_moved
}

/// Retarget `link` when it feeds a numbered input of the node named by `names_of_the_node`.
fn the_link_retargeted_onto_its_slot(
    link: &mut Value,
    names_of_the_node: &[String],
    row: &InputsMovedOntoPinGroupSlots,
) -> bool {
    let Some(target) = link.get_mut("target").and_then(Value::as_object_mut) else {
        return false;
    };
    let feeds_the_node = target
        .get("node")
        .and_then(Value::as_str)
        .is_some_and(|node| names_of_the_node.iter().any(|name| name == node));
    if !feeds_the_node {
        return false;
    }
    let Some(slot_port) = target
        .get("port")
        .and_then(Value::as_str)
        .and_then(|port| the_slot_port_a_numbered_input_became(port, row))
    else {
        return false;
    };
    target.insert("port".to_owned(), Value::String(slot_port));
    true
}

/// The port of the slot member `port` became, `layer_2` becoming `layers.2.picture`; `None` for
/// a port that is no numbered input of `row`.
fn the_slot_port_a_numbered_input_became(
    port: &str,
    row: &InputsMovedOntoPinGroupSlots,
) -> Option<String> {
    row.numbered_inputs
        .iter()
        .find_map(|(before_the_index, member)| {
            let index = port.strip_prefix(before_the_index)?;
            let slot = the_slot_an_index_names(index)?;
            Some(slot_port(row.group, slot, member))
        })
}

/// The slot `index` names: digits with no sign and no leading zero but `0` itself.
fn the_slot_an_index_names(index: &str) -> Option<usize> {
    let all_digits = !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit());
    let no_leading_zero = index == "0" || !index.starts_with('0');
    if all_digits && no_leading_zero {
        index.parse().ok()
    } else {
        None
    }
}

fn slot_port(group: &str, slot: usize, member: &str) -> String {
    format!(
        "{group}{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}{slot}{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}{member}"
    )
}

/// Put each entry of the node's list input on its slot's members, then remove the list; whether
/// the node held it. A value a slot member already holds is kept, and a `null` is never held. A
/// list that is no array, or an entry that is no map, is left as written, for the reader after
/// this to refuse by name.
fn the_list_moved_onto_its_slots(
    node: &mut Map<String, Value>,
    row: &InputsMovedOntoPinGroupSlots,
) -> bool {
    let Some(list_input) = row.list_input else {
        return false;
    };
    let Some(Value::Object(input_values)) = node.get_mut(NODE_INPUT_VALUES_KEY) else {
        return false;
    };
    let Some(Value::Array(entries)) = input_values.get(list_input) else {
        return false;
    };
    if !entries.iter().all(Value::is_object) {
        return false;
    }
    let Some(Value::Array(entries)) = input_values.remove(list_input) else {
        return false;
    };
    for (slot, entry) in entries.into_iter().enumerate() {
        let Value::Object(entry) = entry else {
            continue;
        };
        for (member, value) in entry {
            if !value.is_null() {
                input_values
                    .entry(slot_port(row.group, slot, &member))
                    .or_insert(value);
            }
        }
    }
    if input_values.is_empty() {
        node.remove(NODE_INPUT_VALUES_KEY);
    }
    true
}

#[cfg(test)]
mod tests;
