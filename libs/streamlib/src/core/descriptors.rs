// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor and port descriptor types for introspection.

use serde::{Deserialize, Serialize};

/// Describes an input or output port.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortDescriptor {
    pub name: String,
    pub description: String,
    /// Reference to a schema by name.
    pub schema: String,
    pub required: bool,
    /// Whether this port uses iceoryx2 IPC.
    #[serde(default)]
    pub is_iceoryx2: bool,
}

impl PortDescriptor {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: impl Into<String>,
        required: bool,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema: schema.into(),
            required,
            is_iceoryx2: false,
        }
    }

    /// Create a port descriptor for an iceoryx2 port.
    pub fn iceoryx2(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema: schema.into(),
            required: true,
            is_iceoryx2: true,
        }
    }
}

/// Code examples for a processor in different languages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodeExamples {
    pub rust: String,
    pub python: String,
    pub typescript: String,
}

/// A configuration field for a processor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigField {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    pub required: bool,
    pub description: String,
}

impl ConfigField {
    pub fn new(
        name: impl Into<String>,
        field_type: impl Into<String>,
        required: bool,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            field_type: field_type.into(),
            required,
            description: description.into(),
        }
    }
}

/// Trait for config structs to provide field metadata for descriptors.
pub trait ConfigDescriptor {
    /// Returns the list of config fields with their types and descriptions.
    fn config_fields() -> Vec<ConfigField>;
}

/// Default implementation for unit type (no config).
impl ConfigDescriptor for () {
    fn config_fields() -> Vec<ConfigField> {
        Vec::new()
    }
}

/// Describes a processor with its ports and configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorDescriptor {
    pub name: String,
    pub description: String,
    pub version: String,
    pub repository: String,
    pub config: Vec<ConfigField>,
    pub inputs: Vec<PortDescriptor>,
    pub outputs: Vec<PortDescriptor>,
    pub examples: CodeExamples,
}

impl ProcessorDescriptor {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            version: String::new(),
            repository: String::new(),
            config: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            examples: CodeExamples::default(),
        }
    }

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    pub fn with_repository(mut self, repository: impl Into<String>) -> Self {
        self.repository = repository.into();
        self
    }

    pub fn with_config(mut self, fields: Vec<ConfigField>) -> Self {
        self.config = fields;
        self
    }

    pub fn with_config_field(mut self, field: ConfigField) -> Self {
        self.config.push(field);
        self
    }

    pub fn with_input(mut self, port: PortDescriptor) -> Self {
        self.inputs.push(port);
        self
    }

    pub fn with_output(mut self, port: PortDescriptor) -> Self {
        self.outputs.push(port);
        self
    }

    pub fn with_rust_example(mut self, example: impl Into<String>) -> Self {
        self.examples.rust = example.into();
        self
    }

    pub fn with_python_example(mut self, example: impl Into<String>) -> Self {
        self.examples.python = example.into();
        self
    }

    pub fn with_typescript_example(mut self, example: impl Into<String>) -> Self {
        self.examples.typescript = example.into();
        self
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }
}
