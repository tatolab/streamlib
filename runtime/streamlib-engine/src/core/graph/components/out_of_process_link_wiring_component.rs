// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::Mutex;

use super::ProcessorInstanceComponent;
use crate::core::graph::{GraphNodeWithComponents, ProcessorNode};
use crate::core::processors::{OutOfProcessLinkWiringEnvelope, ProcessorInstance};

/// The link wiring of a processor whose iceoryx2 ports live out of process,
/// attached beside its instance.
///
/// Its presence is what says the processor is out of process, and it is how the
/// compiler op wires that processor's links without taking the processor's own
/// lock. Stored but never rendered.
pub struct OutOfProcessLinkWiringComponent(pub Arc<OutOfProcessLinkWiringEnvelope>);

/// A created processor instance, with the out-of-process link wiring it carries
/// taken while nothing else can hold its lock.
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
    /// Attach the instance to its node, and its wiring beside it when it has
    /// some — the one attach every path takes, since the wiring's presence is
    /// what classifies the processor.
    pub(crate) fn attach_to(self, node: &mut ProcessorNode) -> Arc<Mutex<ProcessorInstance>> {
        node.insert(ProcessorInstanceComponent(Arc::clone(
            &self.processor_instance,
        )));
        if let Some(out_of_process_link_wiring) = self.out_of_process_link_wiring {
            node.insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(
                out_of_process_link_wiring,
            ));
        }
        self.processor_instance
    }
}
