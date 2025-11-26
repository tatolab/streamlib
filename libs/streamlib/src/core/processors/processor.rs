use super::BaseProcessor;
use crate::core::error::Result;
use crate::core::link_channel::{LinkPortType, LinkWakeupEvent};
use crate::core::scheduling::SchedulingConfig;
use crate::core::schema::ProcessorDescriptor;
use serde::{Deserialize, Serialize};

pub trait Processor: BaseProcessor {
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

    fn get_output_port_type(&self, _port_name: &str) -> Option<LinkPortType> {
        None
    }

    fn get_input_port_type(&self, _port_name: &str) -> Option<LinkPortType> {
        None
    }

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

    /// Remove a producer from an output port by link ID.
    fn unwire_output_producer(
        &mut self,
        _port_name: &str,
        _link_id: &crate::core::link_channel::LinkId,
    ) -> Result<()> {
        Ok(())
    }

    /// Remove a consumer from an input port by link ID.
    fn unwire_input_consumer(
        &mut self,
        _port_name: &str,
        _link_id: &crate::core::link_channel::LinkId,
    ) -> Result<()> {
        Ok(())
    }

    fn set_output_wakeup(
        &mut self,
        _port_name: &str,
        _wakeup_tx: crossbeam_channel::Sender<LinkWakeupEvent>,
    ) {
    }
}
