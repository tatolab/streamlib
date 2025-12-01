use crate::core::execution::ExecutionConfig;
use crate::core::link_channel::{LinkPortType, ProcessFunctionEvent};
use crate::core::schema::ProcessorDescriptor;
use crate::core::{Result, RuntimeContext};

use super::ProcessorType;

pub trait DynProcessor: Send + 'static {
    fn __generated_setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn __generated_teardown(&mut self) -> Result<()> {
        Ok(())
    }

    fn process(&mut self) -> Result<()>;

    fn name(&self) -> &str;
    fn processor_type(&self) -> ProcessorType;
    fn descriptor(&self) -> Option<ProcessorDescriptor>;
    fn descriptor_instance(&self) -> Option<ProcessorDescriptor>;

    /// Returns the execution configuration for this processor.
    ///
    /// This determines how and when `process()` is called:
    /// - `Continuous`: Runtime loops, calling process() repeatedly
    /// - `Reactive`: Called when input data arrives
    /// - `Manual`: Called once, then you control timing
    fn execution_config(&self) -> ExecutionConfig;
    fn get_output_port_type(&self, port_name: &str) -> Option<LinkPortType>;
    fn get_input_port_type(&self, port_name: &str) -> Option<LinkPortType>;

    fn wire_output_producer(
        &mut self,
        port_name: &str,
        producer: Box<dyn std::any::Any + Send>,
    ) -> crate::core::Result<()>;

    fn wire_input_consumer(
        &mut self,
        port_name: &str,
        consumer: Box<dyn std::any::Any + Send>,
    ) -> crate::core::Result<()>;

    fn unwire_output_producer(
        &mut self,
        port_name: &str,
        link_id: &crate::core::link_channel::LinkId,
    ) -> crate::core::Result<()>;

    fn unwire_input_consumer(
        &mut self,
        port_name: &str,
        link_id: &crate::core::link_channel::LinkId,
    ) -> crate::core::Result<()>;

    /// Set the sender for invoking the downstream processor's process() function.
    fn set_output_process_function_invoke_send(
        &mut self,
        port_name: &str,
        process_function_invoke_send: crossbeam_channel::Sender<ProcessFunctionEvent>,
    );

    /// Apply a JSON config update at runtime.
    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> crate::core::Result<()>;

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}
