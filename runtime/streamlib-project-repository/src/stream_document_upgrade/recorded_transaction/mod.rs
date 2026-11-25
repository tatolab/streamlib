// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A transaction an older runtime recorded in a project's history, read in the forms this
//! runtime records. The one reader of a recorded transaction passes one of an older format
//! through [`upgrade_recorded_transaction_in_place`] before the strict types read it, so undo,
//! redo, the undo state and the listing read every edit as its current form.
//!
//! A `set_pin_value` reads as a `set_node_input_value` of the same input. A `set_node_config`
//! with `from` and `to` reads as one `set_node_input_value` for each name whose value differs
//! between them. A `set_exposed_config_fields` reads as an edit that changes nothing, since no
//! node takes a config field as an input any more. A node inside an `add_node` or `remove_node`
//! has its old fields folded as a document node's are, by the node's own reader.
//!
//! Each action recorded as `set_pin_value` reads as the `set_node_input_value` it stands for, a
//! `set_node_config` action as one `set_node_input_value` for each name it sent, and a
//! `set_exposed_config_fields` action as nothing sent. An `add_node` action reads without the
//! `config`, `pins` and `exposed_config_fields` it was sent with, and an `import_graph_generator`
//! without its `config`: what they set is in the operations recorded beside them.

use serde_json::{Map, Value};

/// The operation and action an older runtime recorded a set of one input value as.
const RETIRED_SET_PIN_VALUE: &str = "set_pin_value";
/// The operation and action an older runtime recorded a whole config replaced as.
const RETIRED_SET_NODE_CONFIG: &str = "set_node_config";
/// The operation and action an older runtime recorded the config fields a node took as inputs
/// replaced as.
const RETIRED_SET_EXPOSED_CONFIG_FIELDS: &str = "set_exposed_config_fields";
/// The keys an older `add_node` action was sent with that no action takes now.
const RETIRED_ADD_NODE_KEYS: [&str; 3] = ["config", "pins", "exposed_config_fields"];
/// The key an older `import_graph_generator` action was sent with that no action takes now.
const RETIRED_IMPORT_GRAPH_GENERATOR_KEY: &str = "config";
/// The action that adds a node.
const ADD_NODE: &str = "add_node";
/// The action that places a graph generator's output.
const IMPORT_GRAPH_GENERATOR: &str = "import_graph_generator";
/// The operation and action that sets one input value.
const SET_NODE_INPUT_VALUE: &str = "set_node_input_value";

/// The key each recorded operation and action names what it is under.
const OPERATION_KEY: &str = "operation";

/// Upgrade `transaction`, as its trailer recorded it, in place to the forms this runtime
/// records; whether it held a retired form. It never refuses: an entry it does not recognise
/// is left as written, for the strict reader after it to read or refuse.
pub(crate) fn upgrade_recorded_transaction_in_place(transaction: &mut Value) -> bool {
    let Some(transaction) = transaction.as_object_mut() else {
        return false;
    };
    let mut any_changed = false;
    for key in ["operations", "inverse_operations"] {
        if let Some(Value::Array(operations)) = transaction.get_mut(key) {
            any_changed |= each_operation_in_its_current_forms(operations);
        }
    }
    if let Some(Value::Array(actions)) = transaction.get_mut("actions") {
        any_changed |= each_action_in_its_current_forms(actions);
    }
    any_changed
}

/// Replace each retired action of `actions` with the actions it reads as, and take the retired
/// keys off each action that still is one, in place; whether any changed.
fn each_action_in_its_current_forms(actions: &mut Vec<Value>) -> bool {
    let mut any_changed = false;
    let mut in_current_forms = Vec::with_capacity(actions.len());
    for mut action in std::mem::take(actions) {
        match the_name_recorded_on(&action) {
            Some(RETIRED_SET_EXPOSED_CONFIG_FIELDS) => any_changed = true,
            Some(RETIRED_SET_NODE_CONFIG) => {
                in_current_forms.extend(the_sets_a_set_node_config_action_sent(&action));
                any_changed = true;
            }
            Some(ADD_NODE) => {
                any_changed |= the_keys_taken_off(&mut action, &RETIRED_ADD_NODE_KEYS);
                in_current_forms.push(action);
            }
            Some(IMPORT_GRAPH_GENERATOR) => {
                any_changed |=
                    the_keys_taken_off(&mut action, &[RETIRED_IMPORT_GRAPH_GENERATOR_KEY]);
                in_current_forms.push(action);
            }
            _ => {
                any_changed |= the_set_pin_value_renamed(&mut action);
                in_current_forms.push(action);
            }
        }
    }
    *actions = in_current_forms;
    any_changed
}

/// The name `recorded`, an operation or an action, is recorded under.
fn the_name_recorded_on(recorded: &Value) -> Option<&str> {
    recorded.get(OPERATION_KEY).and_then(Value::as_str)
}

/// Take each of `keys` off `recorded`; whether it held any.
fn the_keys_taken_off(recorded: &mut Value, keys: &[&str]) -> bool {
    let Some(recorded) = recorded.as_object_mut() else {
        return false;
    };
    let mut any_held = false;
    for key in keys {
        any_held |= recorded.remove(*key).is_some();
    }
    any_held
}

/// `base` as a `set_node_input_value` of the input `input`.
fn a_set_node_input_value_of(base: &Map<String, Value>, input: String) -> Map<String, Value> {
    let mut set = base.clone();
    name_it_a_set_node_input_value(&mut set);
    set.insert("input".to_owned(), Value::String(input));
    set
}

/// Name `recorded` a `set_node_input_value`.
fn name_it_a_set_node_input_value(recorded: &mut Map<String, Value>) {
    recorded.insert(
        OPERATION_KEY.to_owned(),
        Value::String(SET_NODE_INPUT_VALUE.to_owned()),
    );
}

/// The `set_node_input_value` action of each name a recorded `set_node_config` action sent in
/// its `config`, to the value it sent.
fn the_sets_a_set_node_config_action_sent(action: &Value) -> Vec<Value> {
    let Some(recorded) = action.as_object() else {
        return Vec::new();
    };
    let mut base = recorded.clone();
    let config = match base.remove("config") {
        Some(Value::Object(config)) => config,
        _ => Map::new(),
    };
    config
        .into_iter()
        .map(|(name, value)| {
            let mut set = a_set_node_input_value_of(&base, name);
            if !value.is_null() {
                set.insert("value".to_owned(), value);
            }
            Value::Object(set)
        })
        .collect()
}

/// Replace each retired operation of `operations` with the operations it reads as, in place;
/// whether any was replaced.
fn each_operation_in_its_current_forms(operations: &mut Vec<Value>) -> bool {
    let mut any_changed = false;
    let mut in_current_forms = Vec::with_capacity(operations.len());
    for mut operation in std::mem::take(operations) {
        if let Some(sets) = the_sets_a_whole_config_stands_for(&operation) {
            in_current_forms.extend(sets);
            any_changed = true;
            continue;
        }
        if the_name_recorded_on(&operation) == Some(RETIRED_SET_EXPOSED_CONFIG_FIELDS) {
            in_current_forms.push(the_change_of_nothing_on_its_node(&operation));
            any_changed = true;
            continue;
        }
        any_changed |= the_set_pin_value_renamed(&mut operation);
        in_current_forms.push(operation);
    }
    *operations = in_current_forms;
    any_changed
}

/// Name a recorded `set_pin_value`, operation or action, as the `set_node_input_value` its
/// fields already spell; whether it was one.
fn the_set_pin_value_renamed(recorded: &mut Value) -> bool {
    if the_name_recorded_on(recorded) != Some(RETIRED_SET_PIN_VALUE) {
        return false;
    }
    let Some(recorded) = recorded.as_object_mut() else {
        return false;
    };
    name_it_a_set_node_input_value(recorded);
    true
}

/// The `set_node_input_value` of each name whose value differs between the `from` and `to` of a
/// recorded `set_node_config`, `None` when `operation` is no such operation, holds a config
/// that is no object, or names no value at all. A config that changed no value reads as a set of
/// each name it holds, to the value it already held, so the edit still undoes as one that changed
/// nothing; one that names no value stays the `set_node_config` it was recorded as, which applies
/// as a change of nothing, since no set could stand for it and an edit left with no operation
/// would not undo.
fn the_sets_a_whole_config_stands_for(operation: &Value) -> Option<Vec<Value>> {
    if the_name_recorded_on(operation) != Some(RETIRED_SET_NODE_CONFIG) {
        return None;
    }
    let recorded = operation.as_object()?;
    let from = an_object_or_nothing(recorded.get("from"))?;
    let to = an_object_or_nothing(recorded.get("to"))?;
    let value_of = |config: &Map<String, Value>, name: &str| {
        config.get(name).filter(|value| !value.is_null()).cloned()
    };
    let names: Vec<&String> = to
        .keys()
        .chain(from.keys().filter(|name| !to.contains_key(*name)))
        .collect();
    if names.is_empty() {
        return None;
    }
    let differing: Vec<&String> = names
        .iter()
        .copied()
        .filter(|name| value_of(&from, name) != value_of(&to, name))
        .collect();
    let set_names = if differing.is_empty() {
        names
    } else {
        differing
    };
    let mut base = recorded.clone();
    base.remove("from");
    base.remove("to");
    Some(
        set_names
            .into_iter()
            .map(|name| {
                let mut set = a_set_node_input_value_of(&base, name.clone());
                set.insert(
                    "from".to_owned(),
                    value_of(&from, name).unwrap_or(Value::Null),
                );
                set.insert("to".to_owned(), value_of(&to, name).unwrap_or(Value::Null));
                Value::Object(set)
            })
            .collect(),
    )
}

/// The edit of `operation`'s node that changes nothing, a `set_node_config` that names no value,
/// which an older edit no current one stands for reads as, so it still undoes and redoes.
fn the_change_of_nothing_on_its_node(operation: &Value) -> Value {
    let mut change_of_nothing = operation.as_object().cloned().unwrap_or_default();
    change_of_nothing.insert(
        OPERATION_KEY.to_owned(),
        Value::String(RETIRED_SET_NODE_CONFIG.to_owned()),
    );
    change_of_nothing.insert("from".to_owned(), Value::Object(Map::new()));
    change_of_nothing.insert("to".to_owned(), Value::Object(Map::new()));
    Value::Object(change_of_nothing)
}

/// `recorded` as an object, an absent or `null` one as none; `None` for anything else.
fn an_object_or_nothing(recorded: Option<&Value>) -> Option<Map<String, Value>> {
    match recorded {
        None | Some(Value::Null) => Some(Map::new()),
        Some(Value::Object(object)) => Some(object.clone()),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests;
