// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::GraphRenders;
use crate::core::graph::edges::LinkState;
use crate::core::json_schema::GraphLinkComponentsOnTheWire;

pub struct LinkStateComponent(pub LinkState);

impl Default for LinkStateComponent {
    fn default() -> Self {
        Self(LinkState::Pending)
    }
}

impl GraphRenders<GraphLinkComponentsOnTheWire> for LinkStateComponent {
    fn render_onto(&self, components: &mut GraphLinkComponentsOnTheWire) {
        components.state = Some(crate::core::json_schema::LinkStateOutput::from(self.0).into());
    }
}
