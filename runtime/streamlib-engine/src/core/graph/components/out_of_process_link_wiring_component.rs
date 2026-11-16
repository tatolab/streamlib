// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use parking_lot::Mutex;

use super::{HelperProcessRestartsComponent, NodeInstanceComponent, StateComponent};
use crate::core::graph::{GraphNode, GraphNodeWithComponents};
use crate::core::nodes::{NodeInstance, OutOfProcessLinkWiringEnvelope};

pub struct OutOfProcessLinkWiringComponent(pub Arc<OutOfProcessLinkWiringEnvelope>);

pub(crate) struct NodeInstanceWithItsOutOfProcessLinkWiring {
    pub(crate) node_instance: Arc<Mutex<NodeInstance>>,
    out_of_process_link_wiring: Option<Arc<OutOfProcessLinkWiringEnvelope>>,
}

impl From<NodeInstance> for NodeInstanceWithItsOutOfProcessLinkWiring {
    fn from(node_instance: NodeInstance) -> Self {
        let out_of_process_link_wiring = node_instance.out_of_process_link_wiring();
        Self {
            node_instance: Arc::new(Mutex::new(node_instance)),
            out_of_process_link_wiring,
        }
    }
}

impl NodeInstanceWithItsOutOfProcessLinkWiring {
    pub(crate) fn attach_to(self, node: &mut GraphNode) -> Arc<Mutex<NodeInstance>> {
        node.insert(NodeInstanceComponent(Arc::clone(&self.node_instance)));
        if let Some(out_of_process_link_wiring) = self.out_of_process_link_wiring {
            node.insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(
                out_of_process_link_wiring,
            ));
            if let Some(observable_state) = node
                .get::<StateComponent>()
                .map(StateComponent::clone_inner)
            {
                node.insert(HelperProcessRestartsComponent(observable_state));
            }
        }
        self.node_instance
    }
}
