// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

use crate::pin_type::{DeclaredPinType, NodeInputMode, PinDeclaration, PinFlow, PinType};
use crate::{
    AudioWindowContract, ImageOutputPortFrameRate, NodeClassImportPath, NodeClassShortName,
    NodeKind, NodeScheduling, NodeValueLifetime,
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
    /// The pin type and flow the port's author states. [`PortDescriptor::declared_pin`] is what
    /// the port declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_declaration: Option<PinDeclaration>,
    /// Whether a value set live on this *input* is kept across Stop and a restart of the
    /// runtime, or reset at Stop; kept when it declares none, and on every output port.
    #[serde(default, skip_serializing_if = "NodeValueLifetime::is_kept")]
    pub lifetime: NodeValueLifetime,
    /// Whether this *input* takes a wire, a stored value, or both; `None` when it declares none,
    /// as an input port that takes a wire does, and on every output port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_mode: Option<NodeInputMode>,
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
        pin_declaration: PinDeclaration,
    ) -> Self {
        Self::of_a_pin_declared_or_not(name, description, required, Some(pin_declaration))
    }

    /// A port named `name` declaring `pin_declaration`, or, given none, untyped: a shader's port
    /// before its program types it, or a setting whose schema alone checks its value.
    pub fn of_a_pin_declared_or_not(
        name: impl Into<String>,
        description: impl Into<String>,
        required: bool,
        pin_declaration: Option<PinDeclaration>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            required,
            delivery_profile: None,
            borrow_quota: None,
            audio_window: None,
            pin_default: None,
            pin_declaration,
            lifetime: NodeValueLifetime::Kept,
            input_mode: None,
        }
    }

    /// The pin type and flow the port declares, as its author states them.
    pub fn declared_pin(&self) -> Option<PinDeclaration> {
        self.pin_declaration.clone()
    }

    /// Whether this input is a setting, read at setup or each run.
    pub fn is_a_setting(&self) -> bool {
        self.input_mode.is_some_and(NodeInputMode::is_a_setting)
    }

    /// Whether the port's pin flows as a Value: an output port so declared keeps its last
    /// message for a reader that connects after it.
    pub fn flows_as_a_value(&self) -> bool {
        self.declared_pin()
            .is_some_and(|pin| pin.flow == PinFlow::Value)
    }

    /// Builder-style override for whether this input takes a wire, a stored value, or both.
    /// Meaningful only on an input port.
    pub fn with_input_mode(mut self, input_mode: NodeInputMode) -> Self {
        self.input_mode = Some(input_mode);
        self
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
    /// Whether the node type states that it converts what its one wired input takes into what
    /// its one output gives, so the link verdict finds it between two ends.
    /// [`NodeDescriptor::pin_conversion`] is the conversion it states.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub conversion: bool,
    pub inputs: Vec<PortDescriptor>,
    pub outputs: Vec<PortDescriptor>,
    /// The pin groups the node type declares: slots of inputs and outputs that repeat, each
    /// slot's ports beside `inputs` and `outputs` on a node, named `<group>.<index>.<pin>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pin_groups: Vec<crate::pin_group::PinGroupDescriptor>,
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
            conversion: false,
            inputs: Vec::new(),
            outputs: Vec::new(),
            pin_groups: Vec::new(),
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

    /// Declare that the node type converts what its one wired input takes into what its one
    /// output gives.
    pub fn stating_a_conversion(mut self) -> Self {
        self.conversion = true;
        self
    }

    /// Why a port's type, found through another input of the node type, cannot be found: a
    /// port following an input the node type does not declare, or whose type is chosen by an
    /// input that is not a pin type choice; `None` when every such port names one.
    pub fn refusal_of_a_port_whose_type_no_input_finds(&self) -> Option<String> {
        let the_input_named =
            |input_name: &str| self.inputs.iter().find(|input| input.name == input_name);
        self.inputs.iter().chain(&self.outputs).find_map(|port| {
            match &port.declared_pin()?.pin_type {
                DeclaredPinType::SameAsInput { input_port_name } => {
                    the_input_named(input_port_name).is_none().then(|| {
                        format!(
                            "port {:?} takes the type of input {input_port_name:?}, which the node \
                         does not declare",
                            port.name
                        )
                    })
                }
                DeclaredPinType::SetByInput { input_name } => {
                    let chooses_a_pin_type = the_input_named(input_name)
                        .and_then(PortDescriptor::declared_pin)
                        .is_some_and(|declared| {
                            matches!(
                                declared.pin_type,
                                DeclaredPinType::Is {
                                    pin_type: PinType::PinTypeChoice { .. }
                                }
                            )
                        });
                    (!chooses_a_pin_type).then(|| {
                        format!(
                            "port {:?} takes the type input {input_name:?} chooses, and the \
                             node declares no input {input_name:?} of a pin type choice",
                            port.name
                        )
                    })
                }
                DeclaredPinType::Is { .. }
                | DeclaredPinType::OneOf { .. }
                | DeclaredPinType::Any => None,
            }
        })
    }

    /// Why an output's frame rate, read from one of the node type's settings, cannot be read: it
    /// names a setting the node type does not declare, or one of a type that holds no frame
    /// rate; `None` when every such output, its own or one each slot of a pin group holds, names
    /// a setting that holds one.
    pub fn refusal_of_a_frame_rate_no_setting_holds(&self) -> Option<String> {
        let each_pin_groups_outputs = self.pin_groups.iter().flat_map(|group| &group.outputs);
        self.outputs.iter().chain(each_pin_groups_outputs).find_map(|output| {
            let declared = output.declared_pin()?;
            let Some(ImageOutputPortFrameRate::FromSetting { setting: config_key }) =
                declared.annotations.frame_rate.as_ref()
            else {
                return None;
            };
            let Some(setting) = self
                .inputs
                .iter()
                .find(|input| input.name == *config_key && input.is_a_setting())
            else {
                return Some(format!(
                    "output {:?} runs at the frame rate its node's setting {config_key:?} holds, \
                     and the node declares no setting {config_key:?}. To fix: declare a setting \
                     {config_key:?} holding the frame rate, or name one the node declares",
                    output.name
                ));
            };
            let setting_type = setting
                .declared_pin()
                .and_then(PinDeclaration::into_resolved)?;
            let holds_a_frame_rate = match &setting_type.pin_type {
                PinType::Integer { .. } => true,
                PinType::Bundle { members, .. } => {
                    let mut member_names: Vec<&str> = members
                        .iter()
                        .filter(|member| matches!(member.member_type, PinType::Integer { .. }))
                        .map(|member| member.name.as_str())
                        .collect();
                    member_names.sort_unstable();
                    members.len() == 2 && member_names == ["denominator", "numerator"]
                }
                _ => false,
            };
            (!holds_a_frame_rate).then(|| {
                format!(
                    "output {:?} runs at the frame rate its node's setting {config_key:?} holds, \
                     and that setting is a {}, which holds no frame rate. To fix: type \
                     {config_key:?} an Integer of frames per second, or a Bundle of an Integer \
                     `numerator` and an Integer `denominator`",
                    output.name, setting_type.pin_type
                )
            })
        })
    }

    pub fn with_input(mut self, port: PortDescriptor) -> Self {
        self.inputs.push(port);
        self
    }

    /// Declare each of the settings `S` as one of the node type's inputs, after those it
    /// declares already.
    pub fn with_the_inputs_of_its_settings<S: crate::NodeSettings>(mut self) -> Self {
        self.inputs.extend(S::setting_inputs());
        self
    }

    pub fn with_output(mut self, port: PortDescriptor) -> Self {
        self.outputs.push(port);
        self
    }

    /// Declare a pin group: a slot of inputs and outputs that repeats.
    pub fn with_pin_group(mut self, pin_group: crate::pin_group::PinGroupDescriptor) -> Self {
        self.pin_groups.push(pin_group);
        self
    }

    /// Each input a node of the type holds with `slot_counts` slots in its pin groups, in the
    /// order the groups are declared: the inputs it declares, the input setting the count of
    /// each group a person sets, then each slot's inputs.
    pub fn the_inputs_with_slots(&self, slot_counts: &[usize]) -> Vec<PortDescriptor> {
        let mut inputs = self.inputs.clone();
        inputs.extend(
            self.pin_groups
                .iter()
                .filter_map(crate::pin_group::PinGroupDescriptor::the_input_setting_its_slot_count),
        );
        for (group, count) in self.pin_groups.iter().zip(slot_counts) {
            for slot in 0..*count {
                inputs.extend(group.the_ports_of_slot(slot).0);
            }
        }
        inputs
    }

    /// Each output a node of the type holds with `slot_counts` slots in its pin groups: the
    /// outputs it declares, then each slot's outputs.
    pub fn the_outputs_with_slots(&self, slot_counts: &[usize]) -> Vec<PortDescriptor> {
        let mut outputs = self.outputs.clone();
        for (group, count) in self.pin_groups.iter().zip(slot_counts) {
            for slot in 0..*count {
                outputs.extend(group.the_ports_of_slot(slot).1);
            }
        }
        outputs
    }

    /// The node type with one slot in each of its pin groups, its slots' ports among its inputs
    /// and outputs: what the ports of a node just placed are judged by.
    pub fn with_one_slot_in_each_pin_group(&self) -> Self {
        let one_each = vec![1; self.pin_groups.len()];
        Self {
            inputs: self.the_inputs_with_slots(&one_each),
            outputs: self.the_outputs_with_slots(&one_each),
            ..self.clone()
        }
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
    /// default. Refuses a port that states a flow its pin type does not take, a Trigger input
    /// that states a delivery profile other than `ordered`, and a windowed input whose default
    /// would skip messages, each by name. An input with no pin keeps the delivery profile it
    /// states, or none.
    pub fn with_input_deliveries_defaulted_by_pin(mut self) -> Result<Self, String> {
        let node_type = &self.node_class_import_path;
        let pin_group_pins = self
            .pin_groups
            .iter()
            .flat_map(|group| group.inputs.iter().chain(&group.outputs));
        for port in self
            .inputs
            .iter()
            .chain(&self.outputs)
            .chain(pin_group_pins)
        {
            if let Some(refusal) = port
                .declared_pin()
                .and_then(|pin| pin.refusal_of_its_flow(&port.name))
            {
                return Err(format!("`{node_type}`: {refusal}"));
            }
        }
        let pin_group_inputs = self
            .pin_groups
            .iter_mut()
            .flat_map(|group| group.inputs.iter_mut());
        for input in self.inputs.iter_mut().chain(pin_group_inputs) {
            if let Some(stated) = &input.delivery_profile {
                if let Some(refusal) = input
                    .declared_pin()
                    .and_then(|pin| pin.refusal_of_its_stated_delivery_profile(&input.name, stated))
                {
                    return Err(format!("`{node_type}`: {refusal}"));
                }
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
