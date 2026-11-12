// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor and port descriptor types for introspection.
//!
//! These are engine-free serde data types shared by the engine and the
//! `#[processor]` macro.

use serde::{Deserialize, Serialize};

use crate::{
    AudioWindowContract, NodeKind, NodeValueLifetime, PortFamilyDeclaration,
    ProcessorClassImportPath, ProcessorClassShortName, ProcessorScheduling,
};

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
    /// Delivery profile declared by an *input* port (the destination of an
    /// iceoryx2 service) — `"newest"` or `"ordered"`. Required on every input
    /// port and always `None` on an output port.
    #[serde(default)]
    pub delivery_profile: Option<String>,
    /// The samples each link into this *input* port may hold beyond its
    /// mailbox at once; `None` takes the engine's default. Always `None` on an
    /// output port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub borrow_quota: Option<usize>,
    /// Window contract declared by an audio *input* port — the rate, channels,
    /// dtype, window size and hop it wants, or the sentinel that resolves them
    /// from the device. `None` on a port that declares none and on every
    /// output port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_window: Option<AudioWindowContract>,
    /// The port family the port declares and its family's keys.
    pub family: PortFamilyDeclaration,
    /// The value an *input* port reads while nothing is linked into it and no
    /// pin value is set on its node; `None` when it declares none, and on
    /// every output port. A JSON `null` is a default of no value, which an
    /// optional function node input declares.
    #[serde(
        default,
        deserialize_with = "deserialize_a_pin_default_that_may_be_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub pin_default: Option<serde_json::Value>,
}

fn deserialize_a_pin_default_that_may_be_null<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

impl PortDescriptor {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        required: bool,
        family: PortFamilyDeclaration,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            required,
            delivery_profile: None,
            borrow_quota: None,
            audio_window: None,
            family,
            pin_default: None,
        }
    }

    /// Builder-style override for the value an unlinked input port reads.
    /// Meaningful only on an input port.
    pub fn with_pin_default(mut self, pin_default: serde_json::Value) -> Self {
        self.pin_default = Some(pin_default);
        self
    }

    /// Builder-style override for the delivery profile. Meaningful only on
    /// input ports; engine-side derivation ignores this on output ports.
    pub fn with_delivery_profile(mut self, delivery_profile: impl Into<String>) -> Self {
        self.delivery_profile = Some(delivery_profile.into());
        self
    }

    /// Builder-style override for the borrow quota. Meaningful only on an
    /// input port.
    pub fn with_borrow_quota(mut self, borrow_quota: usize) -> Self {
        self.borrow_quota = Some(borrow_quota);
        self
    }

    /// Builder-style override for the window contract. Meaningful only on an
    /// audio input port; an output port declares none.
    pub fn with_audio_window_contract(mut self, audio_window: AudioWindowContract) -> Self {
        self.audio_window = Some(audio_window);
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

/// Describes a processor with its ports and configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorDescriptor {
    /// The class's short name — what an instance's display name defaults to.
    /// Never the processor's identity: that is `processor_class_import_path`.
    pub processor_class_short_name: ProcessorClassShortName,
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
    pub repository: String,
    /// The node kind: how the runtime runs this type's code.
    #[serde(flatten)]
    pub node_kind: NodeKind,
    /// Runtime environment (Rust, Python).
    #[serde(default)]
    pub runtime: ProcessorRuntime,
    /// Entrypoint for non-Rust runtimes (e.g., "src.blur:BlurProcessor").
    #[serde(default)]
    pub entrypoint: Option<String>,
    /// The config type's JSON Schema, as JSON Schema draft 2020-12.
    ///
    /// `None` only on a descriptor built by hand without one — every
    /// `#[processor]`-emitted descriptor carries a document, an empty-object
    /// one where the processor declares no config.
    #[serde(default)]
    pub config_schema: Option<serde_json::Value>,
    /// Declarative scheduling intent declared in the `#[processor]` attribute.
    /// Read at thread-spawn time; defaults to `Normal` priority.
    #[serde(default)]
    pub scheduling: ProcessorScheduling,
    /// Whether the processor paces itself on real time, as a live device does:
    /// in a stepped stream it stands outside the step barrier.
    #[serde(default)]
    pub paced_by_real_time: bool,
    /// A stateful function node's state record fields, in the order its state
    /// values travel in; empty for every other node type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub function_node_state_fields: Vec<String>,
    /// How long a stateful function node's state lives: `reset` unless its node declares it
    /// `kept`.
    #[serde(
        default = "the_state_lifetime_a_function_node_declaring_none_has",
        skip_serializing_if = "NodeValueLifetime::is_reset"
    )]
    pub function_node_state_lifetime: NodeValueLifetime,
    /// The palette category an editor files the node type under; absent on a
    /// type that declares none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Whether a Python function node's body does not compile, so it runs as
    /// Python and only a reader holding the interpreter's lock can call it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub function_node_runs_as_python: bool,
    pub inputs: Vec<PortDescriptor>,
    pub outputs: Vec<PortDescriptor>,
    pub examples: CodeExamples,
}

impl ProcessorDescriptor {
    pub fn new(
        processor_class_short_name: ProcessorClassShortName,
        processor_class_import_path: ProcessorClassImportPath,
        description: impl Into<String>,
    ) -> Self {
        Self {
            processor_class_short_name,
            processor_class_import_path,
            description: description.into(),
            repository: String::new(),
            node_kind: NodeKind::default(),
            runtime: ProcessorRuntime::default(),
            entrypoint: None,
            config_schema: None,
            scheduling: ProcessorScheduling::default(),
            paced_by_real_time: false,
            function_node_state_fields: Vec::new(),
            function_node_state_lifetime: NodeValueLifetime::Reset,
            category: None,
            function_node_runs_as_python: false,
            inputs: Vec::new(),
            outputs: Vec::new(),
            examples: CodeExamples::default(),
        }
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

    /// Declare the palette category an editor files the node type under.
    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    pub fn with_config_schema(mut self, config_schema: serde_json::Value) -> Self {
        self.config_schema = Some(config_schema);
        self
    }

    pub fn with_scheduling(mut self, scheduling: ProcessorScheduling) -> Self {
        self.scheduling = scheduling;
        self
    }

    /// Declare the processor paced by real time.
    pub fn paced_by_real_time(mut self) -> Self {
        self.paced_by_real_time = true;
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
}

fn the_state_lifetime_a_function_node_declaring_none_has() -> NodeValueLifetime {
    NodeValueLifetime::Reset
}
