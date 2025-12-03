use crate::core::execution::ExecutionConfig;
use crate::core::links::{LinkOutputToProcessorMessage, LinkPortType};
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
    fn execution_config(&self) -> ExecutionConfig;
    fn get_output_port_type(&self, port_name: &str) -> Option<LinkPortType>;
    fn get_input_port_type(&self, port_name: &str) -> Option<LinkPortType>;

    fn add_link_output_data_writer(
        &mut self,
        port_name: &str,
        data_writer: Box<dyn std::any::Any + Send>,
    ) -> crate::core::Result<()>;

    fn add_link_input_data_reader(
        &mut self,
        port_name: &str,
        data_reader: Box<dyn std::any::Any + Send>,
    ) -> crate::core::Result<()>;

    fn remove_link_output_data_writer(
        &mut self,
        port_name: &str,
        link_id: &crate::core::links::LinkId,
    ) -> crate::core::Result<()>;

    fn remove_link_input_data_reader(
        &mut self,
        port_name: &str,
        link_id: &crate::core::links::LinkId,
    ) -> crate::core::Result<()>;

    /// Set the message writer for LinkOutput to processor communication.
    fn set_link_output_to_processor_message_writer(
        &mut self,
        port_name: &str,
        message_writer: crossbeam_channel::Sender<LinkOutputToProcessorMessage>,
    );

    /// Apply a JSON config update at runtime.
    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> crate::core::Result<()>;

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}
