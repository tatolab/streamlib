use super::StreamElement;
use crate::core::error::Result;
use crate::core::schema::ProcessorDescriptor;
use crate::core::context::RuntimeContext;
use crate::core::scheduling::{SchedulingConfig, ClockConfig, SyncMode};
use crate::core::runtime::WakeupEvent;
use crate::core::ports::PortType;
use serde::{Deserialize, Serialize};
use std::time::Duration;
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

    fn clock_sync_point(&self) -> Duration {
        Duration::ZERO
    }

    fn clock_config(&self) -> ClockConfig {
        ClockConfig::default()
    }

    fn sync_mode(&self) -> SyncMode {
        SyncMode::Timestamp
    }

    fn handle_late_frame(&mut self, _lateness_ns: i64) -> bool {
        true
    }

    fn frame_duration_ns(&self) -> Option<i64> {
        None
    }

    fn run_source_loop(
        &mut self,
        _ctx: &RuntimeContext,
        _stop_rx: crossbeam_channel::Receiver<()>,
    ) -> Result<()> {
        Ok(())
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

    /// Wire a type-erased connection to an output port.
    /// Override this in processors that have output ports.
    fn wire_output_connection(&mut self, _port_name: &str, _connection: Arc<dyn std::any::Any + Send + Sync>) -> bool {
        false
    }

    /// Wire a type-erased connection to an input port.
    /// Override this in processors that have input ports.
    fn wire_input_connection(&mut self, _port_name: &str, _connection: Arc<dyn std::any::Any + Send + Sync>) -> bool {
        false
    }
}
