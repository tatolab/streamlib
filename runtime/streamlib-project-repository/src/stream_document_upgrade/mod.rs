// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A stream document saved by an older runtime, read in the shape this runtime writes. Both
//! document readers pass what they are given through [`upgraded_stream_document`] first, so
//! the readers after it know only the current keys. It finds an old shape by its content and
//! holds no state, so it covers a document however it reached the project; nothing is
//! rewritten on disk until the document's next committed edit.

use serde_json::{Map, Value};

/// The key a node holds its value of each input under, by input name.
pub const NODE_INPUT_VALUES_KEY: &str = "input_values";

/// The key an older runtime saved a node's config under.
const RETIRED_NODE_CONFIG_KEY: &str = "config";

/// The key an older runtime saved the value of a node's unlinked inputs under.
const RETIRED_NODE_PINS_KEY: &str = "pins";

/// The keys an older runtime saved a node's values under, which the upgrade folds into
/// [`NODE_INPUT_VALUES_KEY`]; `graph` still renders both, so a render reads back.
pub const NODE_KEYS_FOLDED_INTO_INPUT_VALUES: [&str; 2] =
    [RETIRED_NODE_CONFIG_KEY, RETIRED_NODE_PINS_KEY];

/// Each node type that was renamed, from its old import path to its new one. A retired name is
/// never reused, which is what makes finding a node by its type sound.
const RENAMED_NODE_TYPES: &[(&str, &str)] = &[
    // The `tatolab-ops` node types, moved to Core Nodes under the same name, ports and config.
    (
        "tatolab.ops:Lfo",
        "imports/core-nodes/core_nodes.animation:Lfo",
    ),
    (
        "tatolab.ops:Timer",
        "imports/core-nodes/core_nodes.animation:Timer",
    ),
    (
        "tatolab.ops:Lag",
        "imports/core-nodes/core_nodes.animation:Lag",
    ),
    (
        "tatolab.ops:Remap",
        "imports/core-nodes/core_nodes.math_nodes:Remap",
    ),
    (
        "tatolab.ops:Clamp",
        "imports/core-nodes/core_nodes.math_nodes:Clamp",
    ),
    (
        "tatolab.ops:Add",
        "imports/core-nodes/core_nodes.math_nodes:Add",
    ),
    (
        "tatolab.ops:Subtract",
        "imports/core-nodes/core_nodes.math_nodes:Subtract",
    ),
    (
        "tatolab.ops:Multiply",
        "imports/core-nodes/core_nodes.math_nodes:Multiply",
    ),
    (
        "tatolab.ops:Divide",
        "imports/core-nodes/core_nodes.math_nodes:Divide",
    ),
    (
        "tatolab.ops:Power",
        "imports/core-nodes/core_nodes.math_nodes:Power",
    ),
    (
        "tatolab.ops:Minimum",
        "imports/core-nodes/core_nodes.math_nodes:Minimum",
    ),
    (
        "tatolab.ops:Maximum",
        "imports/core-nodes/core_nodes.math_nodes:Maximum",
    ),
    (
        "tatolab.ops:Absolute",
        "imports/core-nodes/core_nodes.math_nodes:Absolute",
    ),
    (
        "tatolab.ops:Modulo",
        "imports/core-nodes/core_nodes.math_nodes:Modulo",
    ),
    (
        "tatolab.ops:Lerp",
        "imports/core-nodes/core_nodes.math_nodes:Lerp",
    ),
    (
        "tatolab.ops:Smoothstep",
        "imports/core-nodes/core_nodes.math_nodes:Smoothstep",
    ),
    (
        "tatolab.ops:Step",
        "imports/core-nodes/core_nodes.math_nodes:Step",
    ),
    (
        "tatolab.ops:Constant",
        "imports/core-nodes/core_nodes.math_nodes:Constant",
    ),
    (
        "tatolab.ops:Random",
        "imports/core-nodes/core_nodes.math_nodes:Random",
    ),
    (
        "tatolab.ops:PerlinNoise",
        "imports/core-nodes/core_nodes.math_nodes:PerlinNoise",
    ),
    (
        "tatolab.ops:Compare",
        "imports/core-nodes/core_nodes.logic:Compare",
    ),
    (
        "tatolab.ops:Gate",
        "imports/core-nodes/core_nodes.logic:Gate",
    ),
    (
        "tatolab.ops:Switch",
        "imports/core-nodes/core_nodes.logic:Switch",
    ),
    (
        "tatolab.ops:Select",
        "imports/core-nodes/core_nodes.logic:Select",
    ),
    (
        "tatolab.ops:Trigger",
        "imports/core-nodes/core_nodes.logic:Trigger",
    ),
    (
        "tatolab.ops:Counter",
        "imports/core-nodes/core_nodes.logic:Counter",
    ),
    (
        "tatolab.ops:SampleAndHold",
        "imports/core-nodes/core_nodes.logic:SampleAndHold",
    ),
    (
        "tatolab.ops:HoldFrame",
        "imports/core-nodes/core_nodes.logic:HoldFrame",
    ),
    ("tatolab.ops:Feedback", "tatolab.stream:PreviousValue"),
    (
        "tatolab.ops.image_file:ImageFile",
        "imports/core-nodes/core_nodes.image_file:ImageFile",
    ),
];

/// `document` in the shape this runtime writes; `None` when it already is.
///
/// At every level of a fragment, each node of a renamed type takes its new type, and its
/// `config` and `pins` fold into `input_values`: a name `input_values` already holds wins, then
/// `pins`, then `config`. It never refuses: a node it has nothing to do for is left as written.
pub fn upgraded_stream_document(document: &Value) -> Option<Value> {
    let mut upgraded = document.clone();
    let nodes = upgraded.get_mut("nodes")?.as_array_mut()?;
    each_node_of_the_level_upgraded(nodes).then_some(upgraded)
}

/// `document` in the shape this runtime writes, upgraded when it is not.
pub fn stream_document_in_the_current_shape(document: Value) -> Value {
    upgraded_stream_document(&document).unwrap_or(document)
}

/// Fold a node's `pins` and then its `config` into `input_values`, each name `input_values`
/// already holds kept. A `null` is never held: it reads as the input's default, as an absent
/// name does.
pub fn fold_into_input_values(
    input_values: &mut Map<String, Value>,
    pins: Map<String, Value>,
    config: Map<String, Value>,
) {
    for (input, value) in pins.into_iter().chain(config) {
        if !value.is_null() {
            input_values.entry(input).or_insert(value);
        }
    }
    input_values.retain(|_, value| !value.is_null());
}

/// Upgrade each node of one level and of every fragment under it; whether any changed.
pub(crate) fn each_node_of_the_level_upgraded(nodes: &mut [Value]) -> bool {
    let mut any_changed = false;
    for node in nodes.iter_mut().filter_map(Value::as_object_mut) {
        any_changed |= the_node_type_renamed(node);
        any_changed |= the_nodes_old_fields_folded(node);
        if let Some(inner_nodes) = node
            .get_mut("fragment")
            .and_then(|fragment| fragment.get_mut("nodes"))
            .and_then(Value::as_array_mut)
        {
            any_changed |= each_node_of_the_level_upgraded(inner_nodes);
        }
    }
    any_changed
}

fn the_node_type_renamed(node: &mut Map<String, Value>) -> bool {
    let Some(renamed_to) = node
        .get("type")
        .and_then(Value::as_str)
        .and_then(the_type_a_node_type_was_renamed_to)
    else {
        return false;
    };
    node.insert("type".to_owned(), Value::String(renamed_to.to_owned()));
    true
}

/// The type `node_type` was renamed to, `None` for a type never renamed.
fn the_type_a_node_type_was_renamed_to(node_type: &str) -> Option<&'static str> {
    RENAMED_NODE_TYPES
        .iter()
        .find(|(from, _)| *from == node_type)
        .map(|(_, to)| *to)
}

/// Fold `config` and `pins` into `input_values`; whether the node held either. A node holding
/// one of the three as anything but an object (or `null`) is left as written, for the reader
/// after this to refuse by name.
fn the_nodes_old_fields_folded(node: &mut Map<String, Value>) -> bool {
    if !node.contains_key(RETIRED_NODE_CONFIG_KEY) && !node.contains_key(RETIRED_NODE_PINS_KEY) {
        return false;
    }
    let is_foldable =
        |key: &str| matches!(node.get(key), None | Some(Value::Null | Value::Object(_)));
    if ![
        NODE_INPUT_VALUES_KEY,
        RETIRED_NODE_PINS_KEY,
        RETIRED_NODE_CONFIG_KEY,
    ]
    .into_iter()
    .all(is_foldable)
    {
        return false;
    }
    let mut take_the_object_under = |key: &str| match node.remove(key) {
        Some(Value::Object(object)) => object,
        _ => Map::new(),
    };
    let mut input_values = take_the_object_under(NODE_INPUT_VALUES_KEY);
    let pins = take_the_object_under(RETIRED_NODE_PINS_KEY);
    let config = take_the_object_under(RETIRED_NODE_CONFIG_KEY);
    fold_into_input_values(&mut input_values, pins, config);
    if !input_values.is_empty() {
        node.insert(
            NODE_INPUT_VALUES_KEY.to_owned(),
            Value::Object(input_values),
        );
    }
    true
}

#[cfg(test)]
mod tests;
