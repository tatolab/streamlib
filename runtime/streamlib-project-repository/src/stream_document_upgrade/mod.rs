// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A stream document saved by an older runtime, read in the shape this runtime writes. Both
//! document readers pass what they are given through [`upgrade_stream_document_in_place`] first, so
//! the readers after it know only the current keys. It finds an old shape by its content and
//! holds no state, so it covers a document however it reached the project; nothing is
//! rewritten on disk until the document's next committed edit. A transaction an older runtime
//! recorded in history is read in the current forms through `recorded_transaction` the same
//! way.

use serde_json::{Map, Value};

#[cfg(feature = "history")]
pub(crate) mod recorded_transaction;

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
    // The built-in conversions with a typed form, named for the pin types they convert between,
    // under the same ports and config.
    (
        "tatolab.stream:ImageToArray",
        "tatolab.stream:TextureToTensor",
    ),
    (
        "tatolab.stream:ArrayToImage",
        "tatolab.stream:TensorToTexture",
    ),
    (
        "tatolab.stream:ChannelsToArray",
        "tatolab.stream:AudioToTensor",
    ),
    (
        "tatolab.stream:ArrayToChannels",
        "tatolab.stream:TensorToAudio",
    ),
];

/// Upgrade `document` in place to the shape this runtime writes; whether it was in an older one.
///
/// At every level of a fragment, each node of a renamed type takes its new type, and its
/// `config` and `pins` fold into `input_values`: a name `input_values` already holds wins, then
/// `pins`, then `config`. It never refuses: a node it has nothing to do for is left as written.
pub fn upgrade_stream_document_in_place(document: &mut Value) -> bool {
    match document.get_mut("nodes").and_then(Value::as_array_mut) {
        Some(nodes) => each_node_of_the_level_upgraded(nodes),
        None => false,
    }
}

/// `document` in the shape this runtime writes, upgraded when it is not.
pub fn stream_document_in_the_current_shape(mut document: Value) -> Value {
    upgrade_stream_document_in_place(&mut document);
    document
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

/// Which of a node's retired `pins` and `config`, as recorded, holds neither an object nor
/// `null`, which no fold reads; `None` when both fold.
pub(crate) fn the_retired_field_holding_no_object(
    pins: Option<&Value>,
    config: Option<&Value>,
) -> Option<&'static str> {
    [
        (RETIRED_NODE_PINS_KEY, pins),
        (RETIRED_NODE_CONFIG_KEY, config),
    ]
    .into_iter()
    .find(|(_, recorded)| !holds_an_object_or_nothing(*recorded))
    .map(|(key, _)| key)
}

/// Fold a node's retired `pins` and `config`, as recorded, into `input_values`, as
/// [`fold_into_input_values`] does; one [`the_retired_field_holding_no_object`] names folds
/// nothing, so a reader refuses it first.
pub(crate) fn fold_the_retired_fields_into(
    input_values: &mut Map<String, Value>,
    pins: Option<Value>,
    config: Option<Value>,
) {
    fold_into_input_values(input_values, object_or_empty(pins), object_or_empty(config));
}

/// Put `input_values` on `node` under [`NODE_INPUT_VALUES_KEY`], the key left out when it
/// holds none.
pub(crate) fn put_input_values(node: &mut Map<String, Value>, input_values: Map<String, Value>) {
    if input_values.is_empty() {
        node.remove(NODE_INPUT_VALUES_KEY);
    } else {
        node.insert(
            NODE_INPUT_VALUES_KEY.to_owned(),
            Value::Object(input_values),
        );
    }
}

fn holds_an_object_or_nothing(recorded: Option<&Value>) -> bool {
    matches!(recorded, None | Some(Value::Null | Value::Object(_)))
}

fn object_or_empty(recorded: Option<Value>) -> Map<String, Value> {
    match recorded {
        Some(Value::Object(object)) => object,
        _ => Map::new(),
    }
}

/// Upgrade each node of one level and of every fragment under it; whether any changed.
fn each_node_of_the_level_upgraded(nodes: &mut [Value]) -> bool {
    let mut any_changed = false;
    for node in nodes {
        any_changed |= upgrade_a_node_in_place(node);
    }
    any_changed
}

/// Upgrade one node, and every node of a fragment it holds, in place; whether any changed.
pub(crate) fn upgrade_a_node_in_place(node: &mut Value) -> bool {
    let Some(node) = node.as_object_mut() else {
        return false;
    };
    let mut any_changed = the_node_type_renamed(node);
    any_changed |= the_nodes_retired_fields_folded(node);
    if let Some(fragment) = node.get_mut("fragment") {
        any_changed |= upgrade_stream_document_in_place(fragment);
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
fn the_nodes_retired_fields_folded(node: &mut Map<String, Value>) -> bool {
    if !node.contains_key(RETIRED_NODE_CONFIG_KEY) && !node.contains_key(RETIRED_NODE_PINS_KEY) {
        return false;
    }
    if !holds_an_object_or_nothing(node.get(NODE_INPUT_VALUES_KEY))
        || the_retired_field_holding_no_object(
            node.get(RETIRED_NODE_PINS_KEY),
            node.get(RETIRED_NODE_CONFIG_KEY),
        )
        .is_some()
    {
        return false;
    }
    let mut input_values = object_or_empty(node.remove(NODE_INPUT_VALUES_KEY));
    fold_the_retired_fields_into(
        &mut input_values,
        node.remove(RETIRED_NODE_PINS_KEY),
        node.remove(RETIRED_NODE_CONFIG_KEY),
    );
    put_input_values(node, input_values);
    true
}

#[cfg(test)]
mod tests;
