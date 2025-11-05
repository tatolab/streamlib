use crate::core::{RuntimeContext, Result};
use crate::core::schema::ProcessorDescriptor;
use crate::core::runtime::WakeupEvent;
use crate::core::traits::ElementType;
use crate::core::bus::PortType;
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
    fn scheduling_config_dyn(&self) -> crate::core::scheduling::SchedulingConfig;

    fn get_output_port_type(&self, port_name: &str) -> Option<PortType>;
    fn get_input_port_type(&self, port_name: &str) -> Option<PortType>;

    fn wire_output_connection(&mut self, port_name: &str, connection: Arc<dyn std::any::Any + Send + Sync>) -> bool;

    fn wire_input_connection(&mut self, port_name: &str, connection: Arc<dyn std::any::Any + Send + Sync>) -> bool;
}
