// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The value of one member of a bundle input. A bundle input's stored value is one object keyed
//! by member, so a set naming a member sets or clears that key alone and leaves every other
//! member as it is.

use serde_json::{Map, Value};

/// The value `input_values` holds on input `input`, or on its member `member` when one is named;
/// `None` when it holds none.
pub fn the_input_value_held_on<'held>(
    input_values: &'held Map<String, Value>,
    input: &str,
    member: Option<&str>,
) -> Option<&'held Value> {
    the_value_held_on_the_input_or_its_member(input_values.get(input), member)
}

/// `held`, the value an input holds, or its member `member` when one is named; `None` when it
/// holds none.
pub fn the_value_held_on_the_input_or_its_member<'held>(
    held: Option<&'held Value>,
    member: Option<&str>,
) -> Option<&'held Value> {
    match member {
        None => held,
        Some(member) => held?.as_object()?.get(member),
    }
}

/// `input_values` once input `input`, or its member `member` when one is named, is set to
/// `value`, `None` taking it off. A member set on an input holding no object starts one; the
/// last member taken off takes the input's value off.
pub fn input_values_after_the_set(
    input_values: &Map<String, Value>,
    input: &str,
    member: Option<&str>,
    value: Option<&Value>,
) -> Map<String, Value> {
    let mut after_the_set = input_values.clone();
    set_the_input_value_in(&mut after_the_set, input, member, value);
    after_the_set
}

/// Set input `input`, or its member `member`, to `value` in `input_values`, as
/// [`input_values_after_the_set`] says.
pub(crate) fn set_the_input_value_in(
    input_values: &mut Map<String, Value>,
    input: &str,
    member: Option<&str>,
    value: Option<&Value>,
) {
    let held = input_values.remove(input);
    if let Some(after_the_set) = the_input_value_after_the_set(held, member, value) {
        input_values.insert(input.to_owned(), after_the_set);
    }
}

/// The value of an input holding `held` once it, or its member `member` when one is named, is
/// set to `value`, `None` taking it off: `None` when the input holds no value after the set. A
/// member set on an input holding no object starts one; the last member taken off takes the
/// input's value off.
pub fn the_input_value_after_the_set(
    held: Option<Value>,
    member: Option<&str>,
    value: Option<&Value>,
) -> Option<Value> {
    let Some(member) = member else {
        return value.cloned();
    };
    let mut members = match held {
        Some(Value::Object(members)) => members,
        _ => Map::new(),
    };
    match value {
        Some(value) => members.insert(member.to_owned(), value.clone()),
        None => members.remove(member),
    };
    (!members.is_empty()).then_some(Value::Object(members))
}

#[cfg(test)]
mod tests;
