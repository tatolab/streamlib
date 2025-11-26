use crate::core::link_channel::{LinkPortType, LinkWakeupEvent};
use crate::core::scheduling::SchedulingConfig;
use crate::core::schema::ProcessorDescriptor;
use crate::core::{Result, RuntimeContext};

use super::ProcessorType;

pub trait DynProcessor: Send + 'static {
    fn __generated_setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn __generated_teardown(&mut self) -> Result<()> {
        Ok(())
    }

    fn process(&mut self) -> Result<()>;

    fn name(&self) -> &str;
    fn processor_type(&self) -> ProcessorType;
    fn descriptor(&self) -> Option<ProcessorDescriptor>;
    fn descriptor_instance(&self) -> Option<ProcessorDescriptor>;
    fn scheduling_config(&self) -> SchedulingConfig;
    fn get_output_port_type(&self, port_name: &str) -> Option<LinkPortType>;
    fn get_input_port_type(&self, port_name: &str) -> Option<LinkPortType>;

    fn wire_output_producer(
        &mut self,
        port_name: &str,
        producer: Box<dyn std::any::Any + Send>,
    ) -> bool;

    fn wire_input_consumer(
        &mut self,
        port_name: &str,
        consumer: Box<dyn std::any::Any + Send>,
    ) -> bool;

    fn set_output_wakeup(
        &mut self,
        port_name: &str,
        wakeup_tx: crossbeam_channel::Sender<LinkWakeupEvent>,
    );

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}
