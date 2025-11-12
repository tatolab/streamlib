use crate::core::{RuntimeContext, Result};
use crate::core::schema::ProcessorDescriptor;
use crate::core::runtime::WakeupEvent;
use crate::core::traits::ElementType;
use crate::core::bus::PortType;
use std::sync::Arc;

/// Dynamic trait object interface for heterogeneous processor collections.
///
/// This trait provides a uniform interface for the runtime to interact with
/// different processor types through trait objects (Box<dyn DynStreamElement>).
///
/// Performance note: Virtual dispatch adds ~5-10ns per call, which is <1% of
/// typical processing time for media processors. This is acceptable overhead
/// for the flexibility gained.
pub trait DynStreamElement: Send + 'static {
    fn __generated_setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())  // Default: no-op
    }

    fn __generated_teardown(&mut self) -> Result<()> {
        Ok(())  // Default: no-op
    }

    fn dispatch(&mut self) -> Result<()>;
    fn process(&mut self) -> Result<()>;
    fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<WakeupEvent>);
    fn set_wakeup_channel(&mut self, wakeup_tx: crossbeam_channel::Sender<WakeupEvent>);
    fn element_type(&self) -> ElementType;
    fn descriptor(&self) -> Option<ProcessorDescriptor>;
    fn descriptor_instance(&self) -> Option<ProcessorDescriptor>;
    fn name(&self) -> &str;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    fn scheduling_config(&self) -> crate::core::scheduling::SchedulingConfig;

    fn get_output_port_type(&self, port_name: &str) -> Option<PortType>;
    fn get_input_port_type(&self, port_name: &str) -> Option<PortType>;

    // Phase 1 methods (deprecated, for backward compatibility)
    fn wire_output_connection(&mut self, port_name: &str, connection: Arc<dyn std::any::Any + Send + Sync>) -> bool;
    fn wire_input_connection(&mut self, port_name: &str, connection: Arc<dyn std::any::Any + Send + Sync>) -> bool;

    // Phase 2 methods (lock-free)
    fn wire_output_producer(&mut self, port_name: &str, producer: Box<dyn std::any::Any + Send>) -> bool;
    fn wire_input_consumer(&mut self, port_name: &str, consumer: Box<dyn std::any::Any + Send>) -> bool;
}
