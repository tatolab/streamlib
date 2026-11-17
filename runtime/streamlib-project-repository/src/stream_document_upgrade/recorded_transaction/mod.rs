// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A transaction an older runtime recorded in a project's history, read in the forms this
//! runtime records. The one reader of a recorded transaction passes one of an older format
//! through [`upgrade_recorded_transaction_in_place`] before the strict types read it, so undo,
//! redo, the undo state and the listing read every edit as its current form.
//!
//! A `set_pin_value` reads as a `set_node_input_value` of the same input. A `set_node_config`
//! with `from` and `to` reads as one `set_node_input_value` for each name whose value differs
//! between them. A node inside an `add_node` or `remove_node` has its old fields folded as a
//! document node's are, by the node's own reader. Each action recorded as `set_pin_value`
//! reads as the `set_node_input_value` it stands for; a `set_node_config` action names no value
//! it replaced, so it stays as sent.

use serde_json::{Map, Value};

/// The operation and action an older runtime recorded a set of one input value as.
const RETIRED_SET_PIN_VALUE: &str = "set_pin_value";
/// The operation an older runtime recorded a whole config replaced as.
const RETIRED_SET_NODE_CONFIG: &str = "set_node_config";
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
        for action in actions {
            any_changed |= the_set_pin_value_renamed(action);
        }
    }
    any_changed
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
        any_changed |= the_set_pin_value_renamed(&mut operation);
        in_current_forms.push(operation);
    }
    *operations = in_current_forms;
    any_changed
}

/// Name a recorded `set_pin_value`, operation or action, as the `set_node_input_value` its
/// fields already spell; whether it was one.
fn the_set_pin_value_renamed(recorded: &mut Value) -> bool {
    let Some(recorded) = recorded.as_object_mut() else {
        return false;
    };
    if recorded.get(OPERATION_KEY).and_then(Value::as_str) != Some(RETIRED_SET_PIN_VALUE) {
        return false;
    }
    recorded.insert(
        OPERATION_KEY.to_owned(),
        Value::String(SET_NODE_INPUT_VALUE.to_owned()),
    );
    true
}

/// The `set_node_input_value` of each name whose value differs between the `from` and `to` of a
/// recorded `set_node_config`, `None` when `operation` is no such operation, or holds a config
/// that is no object. A config that changed no value reads as a set of each name it holds, to
/// the value it already held, so the edit still undoes as one that changed nothing.
fn the_sets_a_whole_config_stands_for(operation: &Value) -> Option<Vec<Value>> {
    let recorded = operation.as_object()?;
    if recorded.get(OPERATION_KEY).and_then(Value::as_str) != Some(RETIRED_SET_NODE_CONFIG) {
        return None;
    }
    let from = an_object_or_nothing(recorded.get("from"))?;
    let to = an_object_or_nothing(recorded.get("to"))?;
    let value_of = |config: &Map<String, Value>, name: &str| {
        config.get(name).filter(|value| !value.is_null()).cloned()
    };
    let names: Vec<&String> = to
        .keys()
        .chain(from.keys().filter(|name| !to.contains_key(*name)))
        .collect();
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
    Some(
        set_names
            .into_iter()
            .map(|name| {
                let mut set = recorded.clone();
                set.remove("from");
                set.remove("to");
                set.insert(
                    OPERATION_KEY.to_owned(),
                    Value::String(SET_NODE_INPUT_VALUE.to_owned()),
                );
                set.insert("input".to_owned(), Value::String(name.clone()));
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
