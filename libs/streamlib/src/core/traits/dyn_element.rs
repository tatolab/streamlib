use crate::core::{RuntimeContext, Result};
use crate::core::schema::ProcessorDescriptor;
use crate::core::runtime::WakeupEvent;
use crate::core::traits::ElementType;
use crate::core::bus::{Bus, BusReader, BusMessage};
use crate::core::ports::PortType;
use std::sync::Arc;

pub trait DynStreamElement: Send + 'static {
    fn on_start_dyn(&mut self, ctx: &RuntimeContext) -> Result<()>;
    fn on_stop_dyn(&mut self) -> Result<()>;
    fn start_dyn(&mut self, ctx: &RuntimeContext) -> Result<()>;
    fn stop_dyn(&mut self) -> Result<()>;
    fn dispatch_dyn(&mut self) -> Result<()>;
    fn process_dyn(&mut self) -> Result<()>;
    fn set_output_wakeup_dyn(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<WakeupEvent>);
    fn set_wakeup_channel_dyn(&mut self, wakeup_tx: crossbeam_channel::Sender<WakeupEvent>);
    fn element_type_dyn(&self) -> ElementType;
    fn descriptor_dyn(&self) -> Option<ProcessorDescriptor>;
    fn descriptor_instance_dyn(&self) -> Option<ProcessorDescriptor>;
    fn name_dyn(&self) -> &str;
    fn as_any_mut_dyn(&mut self) -> &mut dyn std::any::Any;
    fn provides_clock(&self) -> Option<Arc<dyn crate::core::clocks::Clock>>;
    fn scheduling_config_dyn(&self) -> crate::core::scheduling::SchedulingConfig;

    fn get_output_port_type(&self, port_name: &str) -> Option<PortType>;
    fn get_input_port_type(&self, port_name: &str) -> Option<PortType>;
    fn create_bus_for_output(&self, port_name: &str) -> Option<Arc<dyn std::any::Any + Send + Sync>>;
    fn connect_bus_to_output(&mut self, port_name: &str, bus: Arc<dyn std::any::Any + Send + Sync>) -> bool;
    fn connect_bus_to_input(&mut self, port_name: &str, bus: Arc<dyn std::any::Any + Send + Sync>) -> bool;
    fn connect_reader_to_input(&mut self, port_name: &str, reader: Box<dyn std::any::Any + Send>) -> bool;
}
