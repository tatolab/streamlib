use super::{DynStreamElement, StreamProcessor};
use crate::core::bus::WakeupEvent;
use crate::core::schema::ProcessorDescriptor;
use crate::core::traits::ElementType;
use crate::core::Result;

/// Blanket implementation of DynStreamElement for all StreamProcessor types.
///
/// This allows any StreamProcessor to be used as a trait object (`Box<dyn DynStreamElement>`)
/// in the executor's heterogeneous processor collections.
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

    fn process(&mut self) -> Result<()> {
        self.process()
    }

    fn name(&self) -> &str {
        self.name()
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

    fn scheduling_config(&self) -> crate::core::scheduling::SchedulingConfig {
        self.scheduling_config()
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        self.get_output_port_type(port_name)
    }

    fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        self.get_input_port_type(port_name)
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

    fn set_output_wakeup(
        &mut self,
        port_name: &str,
        wakeup_tx: crossbeam_channel::Sender<WakeupEvent>,
    ) {
        self.set_output_wakeup(port_name, wakeup_tx)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
