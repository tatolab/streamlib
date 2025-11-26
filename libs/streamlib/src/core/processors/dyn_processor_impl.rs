use super::{DynProcessor, Processor, ProcessorType};
use crate::core::link_channel::{LinkPortType, LinkWakeupEvent};
use crate::core::scheduling::SchedulingConfig;
use crate::core::schema::ProcessorDescriptor;
use crate::core::Result;

impl<T> DynProcessor for T
where
    T: Processor,
{
    fn __generated_setup(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        <Self as super::BaseProcessor>::__generated_setup(self, ctx)
    }

    fn __generated_teardown(&mut self) -> Result<()> {
        <Self as super::BaseProcessor>::__generated_teardown(self)
    }

    fn process(&mut self) -> Result<()> {
        <Self as Processor>::process(self)
    }

    fn name(&self) -> &str {
        <Self as super::BaseProcessor>::name(self)
    }

    fn processor_type(&self) -> ProcessorType {
        <Self as super::BaseProcessor>::processor_type(self)
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <T as Processor>::descriptor()
    }

    fn descriptor_instance(&self) -> Option<ProcessorDescriptor> {
        <T as Processor>::descriptor()
    }

    fn scheduling_config(&self) -> SchedulingConfig {
        <Self as Processor>::scheduling_config(self)
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<LinkPortType> {
        <Self as Processor>::get_output_port_type(self, port_name)
    }

    fn get_input_port_type(&self, port_name: &str) -> Option<LinkPortType> {
        <Self as Processor>::get_input_port_type(self, port_name)
    }

    fn wire_output_producer(
        &mut self,
        port_name: &str,
        producer: Box<dyn std::any::Any + Send>,
    ) -> bool {
        <Self as Processor>::wire_output_producer(self, port_name, producer)
    }

    fn wire_input_consumer(
        &mut self,
        port_name: &str,
        consumer: Box<dyn std::any::Any + Send>,
    ) -> bool {
        <Self as Processor>::wire_input_consumer(self, port_name, consumer)
    }

    fn set_output_wakeup(
        &mut self,
        port_name: &str,
        wakeup_tx: crossbeam_channel::Sender<LinkWakeupEvent>,
    ) {
        <Self as Processor>::set_output_wakeup(self, port_name, wakeup_tx)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
