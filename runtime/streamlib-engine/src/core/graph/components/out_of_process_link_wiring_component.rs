// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use parking_lot::Mutex;

use super::{HelperProcessRestartsComponent, ProcessorInstanceComponent, StateComponent};
use crate::core::graph::{GraphNodeWithComponents, ProcessorNode};
use crate::core::processors::{OutOfProcessLinkWiringEnvelope, ProcessorInstance};

pub struct OutOfProcessLinkWiringComponent(pub Arc<OutOfProcessLinkWiringEnvelope>);

pub(crate) struct ProcessorInstanceWithItsOutOfProcessLinkWiring {
    pub(crate) processor_instance: Arc<Mutex<ProcessorInstance>>,
    out_of_process_link_wiring: Option<Arc<OutOfProcessLinkWiringEnvelope>>,
}

impl From<ProcessorInstance> for ProcessorInstanceWithItsOutOfProcessLinkWiring {
    fn from(processor_instance: ProcessorInstance) -> Self {
        let out_of_process_link_wiring = processor_instance.out_of_process_link_wiring();
        Self {
            processor_instance: Arc::new(Mutex::new(processor_instance)),
            out_of_process_link_wiring,
        }
    }
}

impl ProcessorInstanceWithItsOutOfProcessLinkWiring {
    pub(crate) fn attach_to(self, node: &mut ProcessorNode) -> Arc<Mutex<ProcessorInstance>> {
        node.insert(ProcessorInstanceComponent(Arc::clone(
            &self.processor_instance,
        )));
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
        self.processor_instance
    }
}
