use super::StreamElement;
use crate::core::bus::PortType;
use crate::core::bus::WakeupEvent;
use crate::core::error::Result;
use crate::core::scheduling::SchedulingConfig;
use crate::core::schema::ProcessorDescriptor;
use serde::{Deserialize, Serialize};

/// Core trait for stream processors.
///
/// Processors implement this trait to define their processing logic.
/// The executor manages lifecycle, threading, and wiring - processors
/// just declare their ports and implement `process()`.
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

    // =========================================================================
    // Port introspection (macro-generated, used by executor for type-safe wiring)
    // =========================================================================

    fn get_output_port_type(&self, _port_name: &str) -> Option<PortType> {
        None
    }

    fn get_input_port_type(&self, _port_name: &str) -> Option<PortType> {
        None
    }

    // =========================================================================
    // Wiring (macro-generated, called by executor to connect ports)
    // =========================================================================

    fn wire_output_producer(
        &mut self,
        _port_name: &str,
        _producer: Box<dyn std::any::Any + Send>,
    ) -> bool {
        false
    }

    fn wire_input_consumer(
        &mut self,
        _port_name: &str,
        _consumer: Box<dyn std::any::Any + Send>,
    ) -> bool {
        false
    }

    fn set_output_wakeup(
        &mut self,
        _port_name: &str,
        _wakeup_tx: crossbeam_channel::Sender<WakeupEvent>,
    ) {
    }
}
