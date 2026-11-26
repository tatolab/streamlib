// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A value input of a Rust node written as one function: a value the node reads with `.get()` in
//! a computed value or its per-frame function, so a value set on it while its stream plays
//! reaches the node with no rebuild.

use serde::Serialize;

use crate::descriptors::PortDescriptor;
use crate::node_settings::SettingPinType;
use crate::pin_type::{NodeInputMode, PinDeclaration, SettingReadTime};

/// The input `name` of a node written as one function is declared as: described by
/// `description`, of the pin type a value of `T` holds, holding `default` until it is set, and
/// read each run, so a set reaches the node with no rebuild. An input whose default serializes
/// to `null` holds nothing until it is set, so it may be unset.
///
/// # Panics
///
/// When `default` does not serialize, which a value an input holds always does.
pub fn the_value_input_of_a_node_function<T: SettingPinType + Serialize>(
    name: &str,
    description: &str,
    default: &T,
) -> PortDescriptor {
    let default = serde_json::to_value(default)
        .expect("a value input's default serializes, as every value an input holds does");
    let may_be_unset = default.is_null();
    let input = PortDescriptor::new(
        name,
        description,
        false,
        PinDeclaration {
            may_be_unset,
            ..PinDeclaration::exactly(T::setting_pin_type())
        },
    )
    .with_input_mode(NodeInputMode::SettingOnly {
        read: SettingReadTime::EachRun,
    });
    let mut input = if may_be_unset {
        input
    } else {
        input.with_pin_default(default)
    };
    // The node reads the value from its scope, so no message is delivered to it.
    input.delivery_profile = None;
    input
}

#[cfg(test)]
#[path = "node_function_value_input_tests.rs"]
mod tests;
