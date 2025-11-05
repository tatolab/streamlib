use super::StreamElement;
use crate::core::error::Result;
use crate::core::schema::ProcessorDescriptor;
use crate::core::scheduling::SchedulingConfig;
use crate::core::runtime::WakeupEvent;
use crate::core::bus::PortType;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub trait StreamProcessor: StreamElement {
    type Config: Serialize + for<'de> Deserialize<'de> + Default;

    fn from_config(config: Self::Config) -> Result<Self>
    where
        Self: Sized;

    fn process(&mut self) -> Result<()>;

    fn scheduling_config(&self) -> SchedulingConfig {
        SchedulingConfig::default()
    }

    fn descriptor() -> Option<ProcessorDescriptor>
    where
        Self: Sized;

    fn set_output_wakeup(&mut self, _port_name: &str, _wakeup_tx: crossbeam_channel::Sender<WakeupEvent>) {
    }

    fn get_output_port_type(&self, _port_name: &str) -> Option<PortType> {
        None
    }

    fn get_input_port_type(&self, _port_name: &str) -> Option<PortType> {
        None
    }

    fn wire_output_connection(&mut self, _port_name: &str, _connection: Arc<dyn std::any::Any + Send + Sync>) -> bool {
        false
    }

    fn wire_input_connection(&mut self, _port_name: &str, _connection: Arc<dyn std::any::Any + Send + Sync>) -> bool {
        false
    }
}
