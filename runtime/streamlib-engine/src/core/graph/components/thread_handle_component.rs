// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::thread::JoinHandle;

use streamlib_runtime_client_contract::local_api_wire_contract::NodeThreadHandleOnTheWire;

use super::GraphRenders;
use crate::core::compiler::NodeThreadKind;
use crate::core::json_schema::GraphNodeComponentsOnTheWire;

pub struct ThreadHandleComponent {
    pub join_handle: JoinHandle<()>,
    pub kind: NodeThreadKind,
}

impl GraphRenders<GraphNodeComponentsOnTheWire> for ThreadHandleComponent {
    fn render_onto(&self, components: &mut GraphNodeComponentsOnTheWire) {
        components.thread_handle = Some(NodeThreadHandleOnTheWire {
            thread_id: format!("{:?}", self.join_handle.thread().id()),
            thread_name: self
                .join_handle
                .thread()
                .name()
                .unwrap_or("<unnamed>")
                .to_string(),
        });
    }
}
