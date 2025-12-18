// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Object-safe wrapper for GeneratedProcessor - DO NOT USE DIRECTLY.

use super::GeneratedProcessor;
use crate::core::execution::ExecutionConfig;
use crate::core::graph::LinkUniqueId;
use crate::core::links::{LinkOutputToProcessorMessage, LinkPortType};
use crate::core::schema::ProcessorDescriptor;
use crate::core::{Result, RuntimeContext};

/// Object-safe version of [`GeneratedProcessor`] for dynamic dispatch.
///
/// **DO NOT USE DIRECTLY** - This is an internal implementation detail.
pub trait DynGeneratedProcessor: Send + 'static {
    fn __generated_setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn __generated_teardown(&mut self) -> Result<()> {
        Ok(())
    }

    fn process(&mut self) -> Result<()>;

    fn name(&self) -> &str;
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
        link_id: &LinkUniqueId,
    ) -> crate::core::Result<()>;

    fn remove_link_input_data_reader(
        &mut self,
        port_name: &str,
        link_id: &LinkUniqueId,
    ) -> crate::core::Result<()>;

    /// Set the message writer for LinkOutput to processor communication.
    fn set_link_output_to_processor_message_writer(
        &mut self,
        port_name: &str,
        message_writer: crossbeam_channel::Sender<LinkOutputToProcessorMessage>,
    );

    /// Apply a JSON config update at runtime.
    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> crate::core::Result<()>;

    /// Serialize processor-specific runtime state to JSON.
    fn to_runtime_json(&self) -> serde_json::Value;

    /// Get the current config as JSON.
    fn config_json(&self) -> serde_json::Value;

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// Blanket implementation of DynGeneratedProcessor for all GeneratedProcessor types.
impl<T> DynGeneratedProcessor for T
where
    T: GeneratedProcessor,
{
    fn __generated_setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        <Self as GeneratedProcessor>::__generated_setup(self, ctx)
    }

    fn __generated_teardown(&mut self) -> Result<()> {
        <Self as GeneratedProcessor>::__generated_teardown(self)
    }

    fn process(&mut self) -> Result<()> {
        <Self as GeneratedProcessor>::process(self)
    }

    fn name(&self) -> &str {
        <Self as GeneratedProcessor>::name(self)
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <T as GeneratedProcessor>::descriptor()
    }

    fn descriptor_instance(&self) -> Option<ProcessorDescriptor> {
        <T as GeneratedProcessor>::descriptor()
    }

    fn execution_config(&self) -> ExecutionConfig {
        <Self as GeneratedProcessor>::execution_config(self)
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<LinkPortType> {
        <Self as GeneratedProcessor>::get_output_port_type(self, port_name)
    }

    fn get_input_port_type(&self, port_name: &str) -> Option<LinkPortType> {
        <Self as GeneratedProcessor>::get_input_port_type(self, port_name)
    }

    fn add_link_output_data_writer(
        &mut self,
        port_name: &str,
        data_writer: Box<dyn std::any::Any + Send>,
    ) -> crate::core::Result<()> {
        <Self as GeneratedProcessor>::add_link_output_data_writer(self, port_name, data_writer)
    }

    fn add_link_input_data_reader(
        &mut self,
        port_name: &str,
        data_reader: Box<dyn std::any::Any + Send>,
    ) -> crate::core::Result<()> {
        <Self as GeneratedProcessor>::add_link_input_data_reader(self, port_name, data_reader)
    }

    fn remove_link_output_data_writer(
        &mut self,
        port_name: &str,
        link_id: &LinkUniqueId,
    ) -> crate::core::Result<()> {
        <Self as GeneratedProcessor>::remove_link_output_data_writer(self, port_name, link_id)
    }

    fn remove_link_input_data_reader(
        &mut self,
        port_name: &str,
        link_id: &LinkUniqueId,
    ) -> crate::core::Result<()> {
        <Self as GeneratedProcessor>::remove_link_input_data_reader(self, port_name, link_id)
    }

    fn set_link_output_to_processor_message_writer(
        &mut self,
        port_name: &str,
        message_writer: crossbeam_channel::Sender<LinkOutputToProcessorMessage>,
    ) {
        <Self as GeneratedProcessor>::set_link_output_to_processor_message_writer(
            self,
            port_name,
            message_writer,
        )
    }

    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> crate::core::Result<()> {
        <Self as GeneratedProcessor>::apply_config_json(self, config_json)
    }

    fn to_runtime_json(&self) -> serde_json::Value {
        <Self as GeneratedProcessor>::to_runtime_json(self)
    }

    fn config_json(&self) -> serde_json::Value {
        <Self as GeneratedProcessor>::config_json(self)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
