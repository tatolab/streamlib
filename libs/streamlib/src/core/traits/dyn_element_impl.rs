use super::{DynStreamElement, StreamProcessor, StreamElement, PortConsumer};
use crate::core::{RuntimeContext, Result};
use crate::core::schema::ProcessorDescriptor;
use crate::core::runtime::WakeupEvent;
use crate::core::traits::ElementType;
use std::sync::Arc;

impl<T> DynStreamElement for T
where
    T: StreamProcessor,
{
    fn on_start_dyn(&mut self, ctx: &RuntimeContext) -> Result<()> {
        self.start(ctx)
    }

    fn on_stop_dyn(&mut self) -> Result<()> {
        self.stop()
    }

    fn start_dyn(&mut self, ctx: &RuntimeContext) -> Result<()> {
        self.start(ctx)
    }

    fn stop_dyn(&mut self) -> Result<()> {
        self.stop()
    }

    fn dispatch_dyn(&mut self) -> Result<()> {
        Ok(())
    }

    fn process_dyn(&mut self) -> Result<()> {
        self.process()
    }

    fn set_output_wakeup_dyn(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<WakeupEvent>) {
        self.set_output_wakeup(port_name, wakeup_tx)
    }

    fn set_wakeup_channel_dyn(&mut self, _wakeup_tx: crossbeam_channel::Sender<WakeupEvent>) {
    }

    fn element_type_dyn(&self) -> ElementType {
        self.element_type()
    }

    fn descriptor_dyn(&self) -> Option<ProcessorDescriptor> {
        self.descriptor()
    }

    fn descriptor_instance_dyn(&self) -> Option<ProcessorDescriptor> {
        self.descriptor()
    }

    fn name_dyn(&self) -> &str {
        self.name()
    }

    fn as_any_mut_dyn(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provides_clock(&self) -> Option<Arc<dyn crate::core::clocks::Clock>> {
        StreamElement::provides_clock(self)
    }

    fn take_output_consumer_dyn(&mut self, port_name: &str) -> Option<PortConsumer> {
        self.take_output_consumer(port_name)
    }

    fn connect_input_consumer_dyn(&mut self, port_name: &str, consumer: PortConsumer) -> bool {
        self.connect_input_consumer(port_name, consumer)
    }

    fn scheduling_config_dyn(&self) -> crate::core::scheduling::SchedulingConfig {
        self.scheduling_config()
    }
}
