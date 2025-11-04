use super::{StreamElement, PortConsumer};
use crate::core::error::Result;
use crate::core::schema::ProcessorDescriptor;
use crate::core::context::RuntimeContext;
use crate::core::clocks::Clock;
use crate::core::scheduling::{SchedulingConfig, ClockConfig, SyncMode};
use crate::core::runtime::WakeupEvent;
use serde::{Deserialize, Serialize};
use std::time::Duration;

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

    // Port connection methods for runtime wiring
    fn take_output_consumer(&mut self, _port_name: &str) -> Option<PortConsumer> {
        None  // Default: no output ports
    }

    fn connect_input_consumer(&mut self, _port_name: &str, _consumer: PortConsumer) -> bool {
        false  // Default: no input ports
    }

    fn set_output_wakeup(&mut self, _port_name: &str, _wakeup_tx: crossbeam_channel::Sender<WakeupEvent>) {
        // Default: no output ports, do nothing
    }
}
