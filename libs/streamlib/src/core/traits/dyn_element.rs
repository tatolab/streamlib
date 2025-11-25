use crate::core::bus::PortType;
use crate::core::bus::WakeupEvent;
use crate::core::schema::ProcessorDescriptor;
use crate::core::traits::ElementType;
use crate::core::{Result, RuntimeContext};

/// Dynamic trait object interface for heterogeneous processor collections.
///
/// This trait provides a uniform interface for the executor to interact with
/// different processor types through trait objects (`Box<dyn DynStreamElement>`).
pub trait DynStreamElement: Send + 'static {
    // =========================================================================
    // Lifecycle (called by executor)
    // =========================================================================

    fn __generated_setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn __generated_teardown(&mut self) -> Result<()> {
        Ok(())
    }

    // =========================================================================
    // Processing (called by executor's processor thread loop)
    // =========================================================================

    fn process(&mut self) -> Result<()>;

    // =========================================================================
    // Introspection (queried by executor for wiring and scheduling)
    // =========================================================================

    fn name(&self) -> &str;
    fn element_type(&self) -> ElementType;
    fn descriptor(&self) -> Option<ProcessorDescriptor>;
    fn descriptor_instance(&self) -> Option<ProcessorDescriptor>;
    fn scheduling_config(&self) -> crate::core::scheduling::SchedulingConfig;
    fn get_output_port_type(&self, port_name: &str) -> Option<PortType>;
    fn get_input_port_type(&self, port_name: &str) -> Option<PortType>;

    // =========================================================================
    // Wiring (called by executor to connect ports)
    // =========================================================================

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
        wakeup_tx: crossbeam_channel::Sender<WakeupEvent>,
    );

    // =========================================================================
    // Downcasting (for special cases)
    // =========================================================================

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}
