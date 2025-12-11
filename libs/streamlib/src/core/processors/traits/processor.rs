// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::Value as JsonValue;

use super::{BaseProcessor, Config};
use crate::core::error::Result;
use crate::core::execution::ExecutionConfig;
use crate::core::graph::LinkUniqueId;
use crate::core::links::{LinkOutputToProcessorMessage, LinkPortType};
use crate::core::schema::ProcessorDescriptor;

pub trait Processor: BaseProcessor {
    type Config: Config;

    fn from_config(config: Self::Config) -> Result<Self>
    where
        Self: Sized;

    fn process(&mut self) -> Result<()>;

    /// Update configuration at runtime (hot-reload).
    fn update_config(&mut self, _config: Self::Config) -> Result<()> {
        Ok(())
    }

    /// Apply a JSON config update at runtime.
    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> Result<()>
    where
        Self: Sized,
    {
        let config: Self::Config = serde_json::from_value(config_json.clone())
            .map_err(|e| crate::core::StreamError::Config(e.to_string()))?;
        self.update_config(config)
    }

    /// Returns the execution configuration for this processor.
    fn execution_config(&self) -> ExecutionConfig {
        ExecutionConfig::default()
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

    /// Add a data writer to an output port.
    fn add_link_output_data_writer(
        &mut self,
        port_name: &str,
        _data_writer: Box<dyn std::any::Any + Send>,
    ) -> Result<()> {
        Err(crate::core::StreamError::PortError(format!(
            "Output port '{}' not found or type mismatch",
            port_name
        )))
    }

    /// Add a data reader to an input port.
    fn add_link_input_data_reader(
        &mut self,
        port_name: &str,
        _data_reader: Box<dyn std::any::Any + Send>,
    ) -> Result<()> {
        Err(crate::core::StreamError::PortError(format!(
            "Input port '{}' not found or type mismatch",
            port_name
        )))
    }

    /// Remove a data writer from an output port by link ID.
    fn remove_link_output_data_writer(
        &mut self,
        _port_name: &str,
        _link_id: &LinkUniqueId,
    ) -> Result<()> {
        Ok(())
    }

    /// Remove a data reader from an input port by link ID.
    fn remove_link_input_data_reader(
        &mut self,
        _port_name: &str,
        _link_id: &LinkUniqueId,
    ) -> Result<()> {
        Ok(())
    }

    /// Set the message writer for LinkOutput to processor communication.
    fn set_link_output_to_processor_message_writer(
        &mut self,
        _port_name: &str,
        _message_writer: crossbeam_channel::Sender<LinkOutputToProcessorMessage>,
    ) {
    }

    /// Serialize processor-specific runtime state to JSON.
    ///
    /// Override this method to include custom runtime state in JSON serialization.
    /// The default implementation returns the current config.
    fn to_runtime_json(&self) -> JsonValue {
        JsonValue::Null
    }

    /// Get the current config as JSON.
    fn config_json(&self) -> JsonValue
    where
        Self: Sized,
    {
        JsonValue::Null
    }
}
