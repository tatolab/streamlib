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

use crate::stream_graph_document::{GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_VALUE_PORT_NAME};
use streamlib_node_schema::CONSTANT_NODE_TYPE;

use inputs_moved_onto_pin_group_slots::INPUTS_MOVED_ONTO_PIN_GROUP_SLOTS;
use merged_inputs::{MERGED_INPUTS, MergedInput};
use node_references_that_became_wires::INPUTS_THAT_HELD_A_NODE_REFERENCE;
use pin_type_chosen_from_a_family_and_codec::PIN_TYPES_CHOSEN_FROM_A_FAMILY_AND_CODEC;

mod get_variables_named_by_their_setter;
mod inputs_moved_onto_pin_group_slots;
mod interface_pin_type_named_by_its_family;
mod merged_inputs;
mod node_references_that_became_wires;
mod pin_type_chosen_from_a_family_and_codec;
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
    ("tatolab.ops:Constant", CONSTANT_NODE_TYPE),
    (
        "imports/core-nodes/core_nodes.math_nodes:Constant",
        CONSTANT_NODE_TYPE,
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
/// `pins`, then `config`. Then the old inputs that became one take it, the merged value or the
/// combining node a wired part keeps, the numbered inputs that became a pin group's slots
/// move onto them, wires and values, each node reference that became a wire is wired, each
/// `GetVariable` naming a variable its level sets names that setter by UUID, and the family and
/// codec that became a pin type choice are chosen. It never refuses: a node it has
/// nothing to do for is left as written.
pub fn upgrade_stream_document_in_place(document: &mut Value) -> bool {
    upgrade_the_document_with(document, MERGED_INPUTS)
}

/// Upgrade `document` in place, merging the inputs `merged_inputs` names.
fn upgrade_the_document_with(document: &mut Value, merged_inputs: &[MergedInput]) -> bool {
    let Some(level) = document.as_object_mut() else {
        return false;
    };
    let Some(nodes) = level.get_mut("nodes").and_then(Value::as_array_mut) else {
        return false;
    };
    let mut any_changed = false;
    for node in nodes.iter_mut() {
        any_changed |= the_node_reshaped(node, merged_inputs);
    }
    let held_links = level.get_mut("links").and_then(Value::as_array_mut);
    let had_links = held_links.is_some();
    let mut links = held_links.map(std::mem::take).unwrap_or_default();
    if let Some(nodes) = level.get_mut("nodes").and_then(Value::as_array_mut) {
        any_changed |=
            merged_inputs::the_inputs_of_the_level_merged(nodes, &mut links, merged_inputs);
        any_changed |=
            inputs_moved_onto_pin_group_slots::the_inputs_of_the_level_moved_onto_their_slots(
                nodes,
                &mut links,
                INPUTS_MOVED_ONTO_PIN_GROUP_SLOTS,
            );
        any_changed |= node_references_that_became_wires::the_references_of_the_level_wired(
            nodes,
            &mut links,
            INPUTS_THAT_HELD_A_NODE_REFERENCE,
        );
        any_changed |=
            get_variables_named_by_their_setter::the_gets_of_the_level_named_by_their_setter(nodes);
    }
    if had_links || !links.is_empty() {
        level.insert("links".to_owned(), Value::Array(links));
    }
    any_changed
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

/// Drop `node`'s [`NODE_INPUT_VALUES_KEY`] when it holds no value.
pub(crate) fn drop_the_input_values_when_empty(node: &mut Map<String, Value>) {
    if node
        .get(NODE_INPUT_VALUES_KEY)
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty)
    {
        node.remove(NODE_INPUT_VALUES_KEY);
    }
}

/// The name a link gives `node` by: its id, else its name.
fn the_name_a_link_gives(node: &Map<String, Value>) -> Option<&str> {
    node.get("id")
        .or_else(|| node.get("name"))
        .and_then(Value::as_str)
}

/// A link of a level from `source`'s output `source_port` into `target`'s input `target_port`.
fn a_link(source: &str, source_port: &str, target: &str, target_port: &str) -> Value {
    serde_json::json!({
        "source": {"node": source, "port": source_port},
        "target": {"node": target, "port": target_port},
    })
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

/// Upgrade one node, and every node of a fragment it holds, in place; whether any changed. A
/// node placed on its own carries no link, so its old inputs that became one take the merged
/// value of what it saved, and each node reference it held that became a wire is dropped.
pub(crate) fn upgrade_a_node_in_place(node: &mut Value) -> bool {
    upgrade_a_node_with(node, MERGED_INPUTS)
}

fn upgrade_a_node_with(node: &mut Value, merged_inputs: &[MergedInput]) -> bool {
    let mut any_changed = the_node_reshaped(node, merged_inputs);
    if let Some(node) = node.as_object_mut() {
        any_changed |= merged_inputs::the_inputs_of_a_node_merged(
            node,
            &mut Vec::new(),
            merged_inputs,
            &mut Vec::new(),
        );
        any_changed |=
            inputs_moved_onto_pin_group_slots::the_inputs_of_a_node_moved_onto_their_slots(
                node,
                &mut [],
                INPUTS_MOVED_ONTO_PIN_GROUP_SLOTS,
            );
        any_changed |= node_references_that_became_wires::the_references_of_a_node_dropped(
            node,
            INPUTS_THAT_HELD_A_NODE_REFERENCE,
        );
    }
    any_changed
}

/// Rename one node's type, fold its retired fields, choose the pin type its saved family and codec
/// name, or an interface node's saved family implied, and upgrade every level of a fragment it
/// holds; whether any changed. Its own old inputs are merged with the level's links after.
fn the_node_reshaped(node: &mut Value, merged_inputs: &[MergedInput]) -> bool {
    let Some(node) = node.as_object_mut() else {
        return false;
    };
    let held_one_float = the_node_was_a_constant_holding_one_float(node);
    let mut any_changed = the_node_type_renamed(node);
    any_changed |= the_nodes_retired_fields_folded(node);
    if held_one_float {
        a_constant_renamed_holds_the_float_it_held(node);
    }
    any_changed |= pin_type_chosen_from_a_family_and_codec::the_family_and_codec_of_a_node_chosen(
        node,
        PIN_TYPES_CHOSEN_FROM_A_FAMILY_AND_CODEC,
    );
    any_changed |=
        interface_pin_type_named_by_its_family::the_family_of_an_interface_node_chosen(node);
    if let Some(fragment) = node.get_mut("fragment") {
        any_changed |= upgrade_the_document_with(fragment, merged_inputs);
    }
    any_changed
}

fn the_node_type_renamed(node: &mut Map<String, Value>) -> bool {
    the_node_type_renamed_by(node, the_type_a_node_type_was_renamed_to)
}

/// Give `node` the type `renamed` maps its type to; whether it had one to take.
pub(crate) fn the_node_type_renamed_by<'renamed>(
    node: &mut Map<String, Value>,
    renamed: impl Fn(&str) -> Option<&'renamed str>,
) -> bool {
    let Some(renamed_to) = node.get("type").and_then(Value::as_str).and_then(renamed) else {
        return false;
    };
    node.insert("type".to_owned(), Value::String(renamed_to.to_owned()));
    true
}

/// The setting a `Constant` chooses the type of its value by.
const THE_SETTING_A_CONSTANT_CHOOSES_ITS_TYPE_BY: &str = "pin_type";

/// The `value` a `Constant` holding one Float read while it saved none.
const THE_FLOAT_A_CONSTANT_HELD_BY_DEFAULT: f64 = 0.0;

/// Whether `node` is of a type renamed to `Constant`, each of which holds one Float.
fn the_node_was_a_constant_holding_one_float(node: &Map<String, Value>) -> bool {
    node.get("type")
        .and_then(Value::as_str)
        .and_then(the_type_a_node_type_was_renamed_to)
        == Some(CONSTANT_NODE_TYPE)
}

/// Have `node`, a `Constant` renamed from a type that held one Float, choose a Float as its
/// `pin_type` and hold the Float it read: the one it saved, else the one it read while it saved
/// none.
fn a_constant_renamed_holds_the_float_it_held(node: &mut Map<String, Value>) {
    let mut input_values = object_or_empty(node.remove(NODE_INPUT_VALUES_KEY));
    input_values.insert(
        THE_SETTING_A_CONSTANT_CHOOSES_ITS_TYPE_BY.to_owned(),
        serde_json::json!({ "type": "float" }),
    );
    if input_values
        .get(GRAPH_INTERFACE_VALUE_PORT_NAME)
        .is_none_or(Value::is_null)
    {
        input_values.insert(
            GRAPH_INTERFACE_VALUE_PORT_NAME.to_owned(),
            serde_json::json!(THE_FLOAT_A_CONSTANT_HELD_BY_DEFAULT),
        );
    }
    put_input_values(node, input_values);
}

/// The type `node_type` was renamed to, `None` for a type never renamed.
fn the_type_a_node_type_was_renamed_to(node_type: &str) -> Option<&'static str> {
    RENAMED_NODE_TYPES
        .iter()
        .find(|(from, _)| *from == node_type)
        .map(|(_, to)| *to)
}

/// The key a `graph` render of a `GraphInput` repeats its saved `value` under in its `config`, for
/// a published Tato Studio until TAT-484, and the name `value` had before it was an input.
const GRAPH_INPUT_VALUE_RENDERED_FOR_TATO_STUDIO_KEY: &str = "default";

/// Fold `config` and `pins` into `input_values`; whether the node held either. A `GraphInput`
/// holding `input_values` beside its `config` is a `graph` render, whose `value` its input values
/// hold, so the `default` its `config` repeats it under is not folded: a cleared `value` stays
/// cleared. A node holding
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
    let holds_input_values = node.contains_key(NODE_INPUT_VALUES_KEY);
    let mut config = node.remove(RETIRED_NODE_CONFIG_KEY);
    if holds_input_values
        && node.get("type").and_then(Value::as_str) == Some(GRAPH_INPUT_NODE_TYPE)
        && let Some(Value::Object(config)) = &mut config
    {
        config.remove(GRAPH_INPUT_VALUE_RENDERED_FOR_TATO_STUDIO_KEY);
    }
    let mut input_values = object_or_empty(node.remove(NODE_INPUT_VALUES_KEY));
    fold_the_retired_fields_into(
        &mut input_values,
        node.remove(RETIRED_NODE_PINS_KEY),
        config,
    );
    put_input_values(node, input_values);
    true
}

#[cfg(test)]
mod tests;
