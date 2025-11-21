use super::{DynStreamElement, StreamProcessor};
use crate::core::runtime::WakeupEvent;
use crate::core::schema::ProcessorDescriptor;
use crate::core::traits::ElementType;
use crate::core::Result;
use std::sync::Arc;

/// Blanket implementation of DynStreamElement for all StreamProcessor types.
///
/// This allows any StreamProcessor to be used as a trait object (Box<dyn DynStreamElement>)
/// in the runtime's heterogeneous processor collections.
impl<T> DynStreamElement for T
where
    T: StreamProcessor,
{
    fn __generated_setup(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        self.__generated_setup(ctx)
    }

    fn __generated_teardown(&mut self) -> Result<()> {
        self.__generated_teardown()
    }

    fn dispatch(&mut self) -> Result<()> {
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        self.process()
    }

    fn set_output_wakeup(
        &mut self,
        port_name: &str,
        wakeup_tx: crossbeam_channel::Sender<WakeupEvent>,
    ) {
        self.set_output_wakeup(port_name, wakeup_tx)
    }

    fn set_wakeup_channel(&mut self, _wakeup_tx: crossbeam_channel::Sender<WakeupEvent>) {
        // No-op for now - processors use data-driven wakeups
    }

    fn element_type(&self) -> ElementType {
        self.element_type()
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <T as StreamProcessor>::descriptor()
    }

    fn descriptor_instance(&self) -> Option<ProcessorDescriptor> {
        <T as StreamProcessor>::descriptor()
    }

    fn name(&self) -> &str {
        self.name()
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn scheduling_config(&self) -> crate::core::scheduling::SchedulingConfig {
        self.scheduling_config()
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        self.get_output_port_type(port_name)
    }

    fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        self.get_input_port_type(port_name)
    }

    fn wire_output_connection(
        &mut self,
        port_name: &str,
        connection: Arc<dyn std::any::Any + Send + Sync>,
    ) -> bool {
        self.wire_output_connection(port_name, connection)
    }

    fn wire_input_connection(
        &mut self,
        port_name: &str,
        connection: Arc<dyn std::any::Any + Send + Sync>,
    ) -> bool {
        self.wire_input_connection(port_name, connection)
    }

    fn wire_output_producer(
        &mut self,
        port_name: &str,
        producer: Box<dyn std::any::Any + Send>,
    ) -> bool {
        self.wire_output_producer(port_name, producer)
    }

    fn wire_input_consumer(
        &mut self,
        port_name: &str,
        consumer: Box<dyn std::any::Any + Send>,
    ) -> bool {
        self.wire_input_consumer(port_name, consumer)
    }
}
