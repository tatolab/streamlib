// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::{DynProcessor, Processor};
use crate::core::execution::ExecutionConfig;
use crate::core::graph::LinkUniqueId;
use crate::core::links::{LinkOutputToProcessorMessage, LinkPortType};
use crate::core::schema::ProcessorDescriptor;
use crate::core::Result;

impl<T> DynProcessor for T
where
    T: Processor,
{
    fn __generated_setup(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        <Self as Processor>::__generated_setup(self, ctx)
    }

    fn __generated_teardown(&mut self) -> Result<()> {
        <Self as Processor>::__generated_teardown(self)
    }

    fn process(&mut self) -> Result<()> {
        <Self as Processor>::process(self)
    }

    fn name(&self) -> &str {
        <Self as Processor>::name(self)
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <T as Processor>::descriptor()
    }

    fn descriptor_instance(&self) -> Option<ProcessorDescriptor> {
        <T as Processor>::descriptor()
    }

    fn execution_config(&self) -> ExecutionConfig {
        <Self as Processor>::execution_config(self)
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<LinkPortType> {
        <Self as Processor>::get_output_port_type(self, port_name)
    }

    fn get_input_port_type(&self, port_name: &str) -> Option<LinkPortType> {
        <Self as Processor>::get_input_port_type(self, port_name)
    }

    fn add_link_output_data_writer(
        &mut self,
        port_name: &str,
        data_writer: Box<dyn std::any::Any + Send>,
    ) -> crate::core::Result<()> {
        <Self as Processor>::add_link_output_data_writer(self, port_name, data_writer)
    }

    fn add_link_input_data_reader(
        &mut self,
        port_name: &str,
        data_reader: Box<dyn std::any::Any + Send>,
    ) -> crate::core::Result<()> {
        <Self as Processor>::add_link_input_data_reader(self, port_name, data_reader)
    }

    fn remove_link_output_data_writer(
        &mut self,
        port_name: &str,
        link_id: &LinkUniqueId,
    ) -> crate::core::Result<()> {
        <Self as Processor>::remove_link_output_data_writer(self, port_name, link_id)
    }

    fn remove_link_input_data_reader(
        &mut self,
        port_name: &str,
        link_id: &LinkUniqueId,
    ) -> crate::core::Result<()> {
        <Self as Processor>::remove_link_input_data_reader(self, port_name, link_id)
    }

    fn set_link_output_to_processor_message_writer(
        &mut self,
        port_name: &str,
        message_writer: crossbeam_channel::Sender<LinkOutputToProcessorMessage>,
    ) {
        <Self as Processor>::set_link_output_to_processor_message_writer(
            self,
            port_name,
            message_writer,
        )
    }

    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> crate::core::Result<()> {
        <Self as Processor>::apply_config_json(self, config_json)
    }

    fn to_runtime_json(&self) -> serde_json::Value {
        <Self as Processor>::to_runtime_json(self)
    }

    fn config_json(&self) -> serde_json::Value {
        <Self as Processor>::config_json(self)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
