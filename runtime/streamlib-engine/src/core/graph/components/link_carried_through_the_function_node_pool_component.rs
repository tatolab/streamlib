// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;
use crate::core::graph::LinkUniqueId;

/// Marks one of the two links a link from a function node island into a
/// reader that cannot evaluate it is carried as, through the stream's function
/// node pool; `graph` renders the pair once, as the carried link.
pub struct LinkCarriedThroughTheFunctionNodePoolComponent {
    /// The link as the graph was given it.
    pub carried_link_id: LinkUniqueId,
}

impl JsonSerializableComponent for LinkCarriedThroughTheFunctionNodePoolComponent {
    fn json_key(&self) -> &'static str {
        "carried_through_the_function_node_pool"
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::from(self.carried_link_id.to_string())
    }
}
