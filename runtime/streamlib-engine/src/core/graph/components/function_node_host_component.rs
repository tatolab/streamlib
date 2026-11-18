// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_runtime_client_contract::local_api_wire_contract::FunctionNodeHostOnTheWire;

use super::GraphRenders;
use crate::core::json_schema::GraphNodeComponentsOnTheWire;

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

impl GraphRenders<GraphNodeComponentsOnTheWire> for FunctionNodeHostComponent {
    fn render_onto(&self, components: &mut GraphNodeComponentsOnTheWire) {
        components.host = Some(match self {
            Self::InsideTheReadOf(reader_ids) => {
                FunctionNodeHostOnTheWire::InsideTheReadOf(reader_ids.clone())
            }
            Self::InAHelperProcessOfItsOwn => FunctionNodeHostOnTheWire::InAHelperProcessOfItsOwn,
        });
    }
}
