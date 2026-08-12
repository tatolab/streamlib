// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor and port descriptor types for introspection.
//!
//! These are engine-free serde data types shared by the engine and the
//! `#[processor]` macro.

use serde::{Deserialize, Serialize};

use crate::{ProcessorClassImportPath, ProcessorScheduling, SchemaIdent};

/// Runtime environment for a processor.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProcessorRuntime {
    #[default]
    Rust,
    Python,
}

/// Describes an input or output port.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortDescriptor {
    pub name: String,
    pub description: String,
    pub required: bool,
    /// Whether this port uses iceoryx2 IPC.
    #[serde(default)]
    pub is_iceoryx2: bool,
    /// Delivery profile declared by an *input* port (the destination of an
    /// iceoryx2 service) — `"latest"`, `"every_sample"`, or `"lossless"`.
    /// Required on every input port and always `None` on an output port.
    #[serde(default)]
    pub delivery_profile: Option<String>,
}

impl PortDescriptor {
    pub fn new(name: impl Into<String>, description: impl Into<String>, required: bool) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            required,
            is_iceoryx2: false,
            delivery_profile: None,
        }
    }

    /// Create a port descriptor for an iceoryx2 port.
    pub fn iceoryx2(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            is_iceoryx2: true,
            ..Self::new(name, description, true)
        }
    }

    /// Builder-style override for the delivery profile. Meaningful only on
    /// input ports; engine-side derivation ignores this on output ports.
    pub fn with_delivery_profile(mut self, delivery_profile: impl Into<String>) -> Self {
        self.delivery_profile = Some(delivery_profile.into());
        self
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
    /// The `@org/package/Type@version` the authoring surface still
    /// declares. Vestigial: nothing keys on it — a processor is named by
    /// `processor_class_import_path` — and it supplies the default display
    /// name until the authoring grammar retires.
    pub name: SchemaIdent,
    /// The processor class's fully-qualified import path — what the registry,
    /// the control plane and helper-process spawning all name this processor
    /// by.
    ///
    /// Derived mechanically by the authoring surface — the `#[processor]`
    /// macro captures it at the expansion site, the wheel reads it off the
    /// class — and never authored. Taken by `new` rather than by a builder so
    /// that adding a descriptor without deriving one does not compile.
    pub processor_class_import_path: ProcessorClassImportPath,
    pub description: String,
    pub version: String,
    pub repository: String,
    /// Runtime environment (Rust, Python).
    #[serde(default)]
    pub runtime: ProcessorRuntime,
    /// Entrypoint for non-Rust runtimes (e.g., "src.blur:BlurProcessor").
    #[serde(default)]
    pub entrypoint: Option<String>,
    /// Reference to config schema (e.g., "com.example.blur.config@1.0.0").
    #[serde(default)]
    pub config_schema: Option<String>,
    /// Declarative scheduling intent declared in the `#[processor]` attribute.
    /// Read at thread-spawn time; defaults to `Normal` priority.
    #[serde(default)]
    pub scheduling: ProcessorScheduling,
    pub inputs: Vec<PortDescriptor>,
    pub outputs: Vec<PortDescriptor>,
    pub examples: CodeExamples,
}

impl ProcessorDescriptor {
    pub fn new(
        name: SchemaIdent,
        processor_class_import_path: ProcessorClassImportPath,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name,
            processor_class_import_path,
            description: description.into(),
            version: String::new(),
            repository: String::new(),
            runtime: ProcessorRuntime::default(),
            entrypoint: None,
            config_schema: None,
            scheduling: ProcessorScheduling::default(),
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

    pub fn with_runtime(mut self, runtime: ProcessorRuntime) -> Self {
        self.runtime = runtime;
        self
    }

    pub fn with_entrypoint(mut self, entrypoint: impl Into<String>) -> Self {
        self.entrypoint = Some(entrypoint.into());
        self
    }

    pub fn with_config_schema(mut self, schema: impl Into<String>) -> Self {
        self.config_schema = Some(schema.into());
        self
    }

    pub fn with_scheduling(mut self, scheduling: ProcessorScheduling) -> Self {
        self.scheduling = scheduling;
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
