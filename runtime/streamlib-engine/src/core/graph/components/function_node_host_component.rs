// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;

/// Where a function node runs, rendered by `graph` as `host`: inside the read
/// of each node its island feeds, or in a helper process of its own once
/// promoted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionNodeHostComponent {
    /// Inside the read of each of these nodes, by name, in name order.
    InsideTheReadOf(Vec<String>),
    /// In a helper process of its own.
    InAHelperProcessOfItsOwn,
}

impl JsonSerializableComponent for FunctionNodeHostComponent {
    fn json_key(&self) -> &'static str {
        "host"
    }

    fn to_json(&self) -> JsonValue {
        match self {
            Self::InsideTheReadOf(reader_ids) => {
                serde_json::json!({ "inside_the_read_of": reader_ids })
            }
            Self::InAHelperProcessOfItsOwn => {
                serde_json::json!({ "in_a_helper_process_of_its_own": true })
            }
        }
    }
}
