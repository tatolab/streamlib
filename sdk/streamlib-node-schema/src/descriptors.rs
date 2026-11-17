// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

use crate::pin_type::{PinDeclaration, PinFlow};
use crate::{
    AudioWindowContract, NodeClassImportPath, NodeClassShortName, NodeKind, NodeScheduling,
    NodeValueLifetime, PortFamilyDeclaration,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NodeRuntime {
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
    /// iceoryx2 service) — `"newest"` or `"ordered"`. An input with a pin that
    /// states none reads by its pin's default, which
    /// [`NodeDescriptor::with_input_deliveries_defaulted_by_pin`] fills in; every
    /// other input states one. Always `None` on an output port.
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
    /// The pin type and flow the port's author states; `None` when the code states none
    /// exactly. [`PortDescriptor::declared_pin`] is what the port declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_declaration: Option<PinDeclaration>,
    /// Whether a value set live on this *input* is kept across Stop and a restart of the
    /// runtime, or reset at Stop; kept when it declares none, and on every output port.
    #[serde(default, skip_serializing_if = "NodeValueLifetime::is_kept")]
    pub lifetime: NodeValueLifetime,
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
            pin_declaration: None,
            lifetime: NodeValueLifetime::Kept,
        }
    }

    /// The pin type and flow the port declares: the ones its author states, else a Texture for
    /// a picture port. `None` for a port whose type the code does not state exactly; it links
    /// by its family alone.
    pub fn declared_pin(&self) -> Option<PinDeclaration> {
        self.pin_declaration
            .clone()
            .or_else(|| self.family.exact_pin_type().map(PinDeclaration::exactly))
    }

    /// Whether the port's pin flows as a Value: an output port so declared keeps its last
    /// message for a reader that connects after it.
    pub fn flows_as_a_value(&self) -> bool {
        self.declared_pin()
            .is_some_and(|pin| pin.flow == PinFlow::Value)
    }

    /// Builder-style override for the pin type and flow the port's author states.
    pub fn with_pin_declaration(mut self, pin_declaration: PinDeclaration) -> Self {
        self.pin_declaration = Some(pin_declaration);
        self
    }

    /// Builder-style override for the value an unlinked input port reads.
    /// Meaningful only on an input port.
    pub fn with_pin_default(mut self, pin_default: serde_json::Value) -> Self {
        self.pin_default = Some(pin_default);
        self
    }

    /// Builder-style override for how long a value set live on the input lives. Meaningful only
    /// on an input port.
    pub fn with_lifetime(mut self, lifetime: NodeValueLifetime) -> Self {
        self.lifetime = lifetime;
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodeExamples {
    pub rust: String,
    pub python: String,
    pub typescript: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDescriptor {
    pub node_class_short_name: NodeClassShortName,
    pub node_class_import_path: NodeClassImportPath,
    pub description: String,
    pub repository: String,
    /// The node kind: how the runtime runs this type's code.
    #[serde(flatten)]
    pub node_kind: NodeKind,
    /// Runtime environment (Rust, Python).
    #[serde(default)]
    pub runtime: NodeRuntime,
    #[serde(default)]
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub config_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub scheduling: NodeScheduling,
    #[serde(default)]
    pub paced_by_real_time: bool,
    /// A stateful function node's state record fields, in the order its state
    /// values travel in; empty for every other node type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub function_node_state_fields: Vec<String>,
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

impl NodeDescriptor {
    pub fn new(
        node_class_short_name: NodeClassShortName,
        node_class_import_path: NodeClassImportPath,
        description: impl Into<String>,
    ) -> Self {
        Self {
            node_class_short_name,
            node_class_import_path,
            description: description.into(),
            repository: String::new(),
            node_kind: NodeKind::default(),
            runtime: NodeRuntime::default(),
            entrypoint: None,
            config_schema: None,
            scheduling: NodeScheduling::default(),
            paced_by_real_time: false,
            function_node_state_fields: Vec::new(),
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

    pub fn with_runtime(mut self, runtime: NodeRuntime) -> Self {
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

    pub fn with_scheduling(mut self, scheduling: NodeScheduling) -> Self {
        self.scheduling = scheduling;
        self
    }

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

    /// This node type with each input that states no delivery profile reading by its pin's
    /// default. Refuses a port that states a flow its pin type does not take, and a windowed
    /// input whose default would skip messages, each by name. An input with no pin keeps the
    /// delivery profile it states, or none.
    pub fn with_input_deliveries_defaulted_by_pin(mut self) -> Result<Self, String> {
        let node_type = &self.node_class_import_path;
        for port in self.inputs.iter().chain(&self.outputs) {
            if let Some(refusal) = port
                .declared_pin()
                .and_then(|pin| pin.refusal_of_its_flow(&port.name))
            {
                return Err(format!("`{node_type}`: {refusal}"));
            }
        }
        for input in &mut self.inputs {
            if input.delivery_profile.is_some() {
                continue;
            }
            let Some(default_delivery_profile) = input
                .declared_pin()
                .and_then(|pin| pin.default_input_delivery_profile())
            else {
                continue;
            };
            if input.audio_window.is_some() {
                crate::refuse_audio_window_beside_a_skipping_delivery_profile(Some(
                    default_delivery_profile,
                ))
                .map_err(|refusal| {
                    format!(
                        "`{node_type}`: input port `{}` reads by its pin's default delivery \
                         profile, `{default_delivery_profile}`: {refusal}",
                        input.name
                    )
                })?;
            }
            input.delivery_profile = Some(default_delivery_profile.to_string());
        }
        Ok(self)
    }
}
