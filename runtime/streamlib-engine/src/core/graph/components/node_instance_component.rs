// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use parking_lot::Mutex;
use streamlib_runtime_client_contract::local_api_wire_contract::NodeInstanceOnTheWire;

use super::NodeComponentGraphRenders;
use crate::core::json_schema::GraphNodeComponentsOnTheWire;
use crate::core::nodes::NodeInstance;

pub struct NodeInstanceComponent(pub Arc<Mutex<NodeInstance>>);

impl NodeComponentGraphRenders for NodeInstanceComponent {
    fn render_onto(&self, components: &mut GraphNodeComponentsOnTheWire) {
        components.node_instance = Some(NodeInstanceOnTheWire {
            attached: true,
            arc_strong_count: Arc::strong_count(&self.0),
            arc_weak_count: Arc::weak_count(&self.0),
        });
    }
}
