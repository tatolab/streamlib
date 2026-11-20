// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node type for tests that reads three settings once, at setup, and records each setup it
//! runs: what a test of a node rebuilt to take a setting read at setup runs; and a node type
//! whose Integer output, which flows as a value, a test writes, to wire into one of them, with
//! a setting read at setup of its own that another source's wire may feed.

use std::collections::HashMap;
use std::sync::{Arc, Once};

use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::descriptors::{
    NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeInputMode, PinDeclaration,
    PinType, PortDescriptor, PortFamily, PortFamilyDeclaration, SettingReadTime,
};
use crate::core::error::{Error, Result};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::GraphNode;
use crate::core::nodes::{DynGeneratedNode, NODE_REGISTRY};

/// The kernel size the fixture node cannot build with.
pub const A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH: i64 = 0;

/// The kernel size the fixture node refuses to be handed at all, before its setup.
pub const A_KERNEL_SIZE_IT_IS_NOT_HANDED: i64 = -1;

/// Each setup the fixture node ran, by node: the settings it read.
static SETUPS_RUN: Mutex<Vec<(Uuid, Map<String, Value>)>> = Mutex::new(Vec::new());

/// Each setup and teardown the fixture node ran, by node, in the order run.
static SETUPS_AND_TEARDOWNS_RUN: Mutex<Vec<(Uuid, WhatANodeRan)>> = Mutex::new(Vec::new());

/// What the fixture node ran: its setup or its teardown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhatANodeRan {
    Setup,
    Teardown,
}

/// The output writer of each source node a test writes, by node.
static SOURCE_OUTPUTS: Mutex<Option<HashMap<Uuid, Arc<crate::iceoryx2::OutputWriterInner>>>> =
    Mutex::new(None);

/// The value each source node writes as it sets up, by node.
static VALUES_WRITTEN_AT_SETUP: Mutex<Option<HashMap<Uuid, i64>>> = Mutex::new(None);

/// Each source node whose setup fails.
static SOURCES_WHOSE_SETUP_FAILS: Mutex<Vec<Uuid>> = Mutex::new(Vec::new());

/// The setting of the source node type, read at setup, that a wire from another source feeds.
pub const THE_SOURCES_SETTING: &str = "offset";

/// The output of the source node type, an Integer that flows as a value.
pub const THE_SOURCES_OUTPUT: &str = "integer";

fn the_fixture_node_type() -> NodeClassImportPath {
    NodeClassImportPath::new(format!("{}::NodeReadingItsSettingsAtSetup", module_path!()))
        .expect("a module path and a type name make an import path")
}

fn a_setting_read_at_setup(name: &str, pin_type: PinType) -> PortDescriptor {
    PortDescriptor::new(
        name,
        name,
        true,
        PortFamilyDeclaration::of_family(PortFamily::Records),
    )
    .with_pin_declaration(PinDeclaration::exactly(pin_type))
    .with_input_mode(NodeInputMode::SettingOnly {
        read: SettingReadTime::AtSetup,
    })
}

/// A node reading `kernel_size`, `width` and `height` once, at setup, refusing to build with the
/// kernel size [`A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH`].
struct NodeReadingItsSettingsAtSetup {
    node_uuid: Uuid,
    descriptor: NodeDescriptor,
    settings: Map<String, Value>,
}

impl DynGeneratedNode for NodeReadingItsSettingsAtSetup {
    fn __generated_setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        SETUPS_RUN
            .lock()
            .push((self.node_uuid, self.settings.clone()));
        SETUPS_AND_TEARDOWNS_RUN
            .lock()
            .push((self.node_uuid, WhatANodeRan::Setup));
        if self.settings.get("kernel_size") == Some(&json!(A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH)) {
            return Err(Error::Configuration(format!(
                "a kernel of size {A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH} blurs nothing"
            )));
        }
        Ok(())
    }

    fn __generated_teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        SETUPS_AND_TEARDOWNS_RUN
            .lock()
            .push((self.node_uuid, WhatANodeRan::Teardown));
        Ok(())
    }

    fn __generated_on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn __generated_on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn name(&self) -> &str {
        "NodeReadingItsSettingsAtSetup"
    }

    fn descriptor(&self) -> Option<NodeDescriptor> {
        Some(self.descriptor.clone())
    }

    fn execution_config(&self) -> ExecutionConfig {
        ExecutionConfig::new(ProcessExecution::Manual)
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        false
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        false
    }

    fn set_iceoryx2_resources(
        &mut self,
        _output_writer: Option<crate::iceoryx2::OutputWriter>,
        _input_mailboxes: Option<crate::iceoryx2::InputMailboxes>,
    ) -> Result<()> {
        Ok(())
    }

    fn iceoryx2_output_writer_inner(&self) -> Option<Arc<crate::iceoryx2::OutputWriterInner>> {
        None
    }

    fn iceoryx2_input_mailboxes_inner(&self) -> Option<Arc<crate::iceoryx2::InputMailboxesInner>> {
        None
    }

    fn apply_config_json(&mut self, _config_json: &Value) -> Result<()> {
        Ok(())
    }

    fn set_unlinked_input_values(&mut self, input_values: &Map<String, Value>) -> Result<()> {
        if input_values.get("kernel_size") == Some(&json!(A_KERNEL_SIZE_IT_IS_NOT_HANDED)) {
            return Err(Error::Configuration(format!(
                "a kernel of size {A_KERNEL_SIZE_IT_IS_NOT_HANDED} is not taken"
            )));
        }
        self.settings = input_values.clone();
        Ok(())
    }

    fn to_runtime_json(&self) -> Value {
        Value::Null
    }

    fn config_json(&self) -> Value {
        Value::Null
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn the_source_node_type() -> NodeClassImportPath {
    NodeClassImportPath::new(format!("{}::IntegerSourceATestWrites", module_path!()))
        .expect("a module path and a type name make an import path")
}

/// A node whose one output, an Integer that flows as a value, a test writes with
/// [`write_from_the_source`]; it runs nothing of its own.
struct IntegerSourceATestWrites {
    node_uuid: Uuid,
    descriptor: NodeDescriptor,
    output: Option<Arc<crate::iceoryx2::OutputWriterInner>>,
}

impl DynGeneratedNode for IntegerSourceATestWrites {
    fn __generated_setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if let Some(output) = &self.output {
            SOURCE_OUTPUTS
                .lock()
                .get_or_insert_with(HashMap::new)
                .insert(self.node_uuid, Arc::clone(output));
        }
        if SOURCES_WHOSE_SETUP_FAILS.lock().contains(&self.node_uuid) {
            return Err(Error::Configuration(
                "the source was told to fail its setup".to_owned(),
            ));
        }
        let written_at_setup = VALUES_WRITTEN_AT_SETUP
            .lock()
            .as_ref()
            .and_then(|values| values.get(&self.node_uuid).copied());
        match written_at_setup {
            Some(value) => write_from_the_source(self.node_uuid, value),
            None => Ok(()),
        }
    }

    fn __generated_teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn __generated_on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn __generated_on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn name(&self) -> &str {
        "IntegerSourceATestWrites"
    }

    fn descriptor(&self) -> Option<NodeDescriptor> {
        Some(self.descriptor.clone())
    }

    fn execution_config(&self) -> ExecutionConfig {
        ExecutionConfig::new(ProcessExecution::Manual)
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        true
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        false
    }

    fn set_iceoryx2_resources(
        &mut self,
        output_writer: Option<crate::iceoryx2::OutputWriter>,
        _input_mailboxes: Option<crate::iceoryx2::InputMailboxes>,
    ) -> Result<()> {
        self.output = output_writer.and_then(|writer| writer.inner_arc());
        Ok(())
    }

    fn iceoryx2_output_writer_inner(&self) -> Option<Arc<crate::iceoryx2::OutputWriterInner>> {
        self.output.clone()
    }

    fn iceoryx2_input_mailboxes_inner(&self) -> Option<Arc<crate::iceoryx2::InputMailboxesInner>> {
        None
    }

    fn apply_config_json(&mut self, _config_json: &Value) -> Result<()> {
        Ok(())
    }

    fn to_runtime_json(&self) -> Value {
        Value::Null
    }

    fn config_json(&self) -> Value {
        Value::Null
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn register_the_source_node_type_once() {
    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(|| {
        let integer = PinType::Integer { exact_size: None };
        let mut descriptor = NodeDescriptor::new(
            NodeClassShortName::new("IntegerSourceATestWrites")
                .expect("a type name is a short name"),
            the_source_node_type(),
            "a node whose Integer output a test writes",
        );
        descriptor.outputs = vec![
            PortDescriptor::new(
                THE_SOURCES_OUTPUT,
                THE_SOURCES_OUTPUT,
                false,
                PortFamilyDeclaration::of_family(
                    PortFamily::carrying(&integer).unwrap_or(PortFamily::Records),
                ),
            )
            .with_pin_declaration(PinDeclaration::exactly(integer.clone())),
        ];
        descriptor.inputs = vec![a_setting_read_at_setup(THE_SOURCES_SETTING, integer)];
        let described = descriptor.clone();
        NODE_REGISTRY
            .register_with_a_constructor_for_a_test(
                descriptor,
                Box::new(move |node: &GraphNode| {
                    Ok(Box::new(IntegerSourceATestWrites {
                        node_uuid: node.node_uuid,
                        descriptor: described.clone(),
                        output: None,
                    }))
                }),
            )
            .expect("register the source node type");
    });
}

/// The node type whose Integer output a test writes, registered once per process.
pub fn the_node_type_whose_output_a_test_writes() -> NodeClassImportPath {
    register_the_source_node_type_once();
    the_source_node_type()
}

/// Have the source node `node` write `value` as it sets up.
pub fn write_from_the_source_at_its_setup(node: Uuid, value: i64) {
    VALUES_WRITTEN_AT_SETUP
        .lock()
        .get_or_insert_with(HashMap::new)
        .insert(node, value);
}

/// Have the setup of the source node `node` fail.
pub fn fail_the_setup_of_the_source(node: Uuid) {
    SOURCES_WHOSE_SETUP_FAILS.lock().push(node);
}

/// Write `value` on the output of the source node `node`, set up in a running stream.
pub fn write_from_the_source(node: Uuid, value: i64) -> Result<()> {
    let output = SOURCE_OUTPUTS
        .lock()
        .as_ref()
        .and_then(|outputs| outputs.get(&node).cloned())
        .ok_or_else(|| Error::NodeNotFound(format!("no source node `{node}` is set up")))?;
    let packed = crate::core::pin_value_packing::pack_pin_value(
        &PinType::Integer { exact_size: None },
        &json!(value),
        0,
    )
    .map_err(Error::Runtime)?;
    output.write_bytes_described_by(
        THE_SOURCES_OUTPUT,
        &packed.payload,
        0,
        crate::iceoryx2::MessageGroupBoundary::StartsGroup,
        packed.payload_descriptor,
    )
}

fn register_the_node_type_once() {
    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(|| {
        let node_type = the_fixture_node_type();
        let mut descriptor = NodeDescriptor::new(
            NodeClassShortName::new("NodeReadingItsSettingsAtSetup")
                .expect("a type name is a short name"),
            node_type,
            "a node reading its settings once, at setup",
        );
        descriptor.inputs = vec![
            a_setting_read_at_setup("kernel_size", PinType::Integer { exact_size: None }),
            a_setting_read_at_setup("width", PinType::Integer { exact_size: None }),
            a_setting_read_at_setup("height", PinType::Integer { exact_size: None }),
        ];
        let described = descriptor.clone();
        NODE_REGISTRY
            .register_with_a_constructor_for_a_test(
                descriptor,
                Box::new(move |node: &GraphNode| {
                    Ok(Box::new(NodeReadingItsSettingsAtSetup {
                        node_uuid: node.node_uuid,
                        descriptor: described.clone(),
                        settings: Map::new(),
                    }))
                }),
            )
            .expect("register the fixture node type");
    });
}

/// The node type reading `kernel_size`, `width` and `height` once, at setup, registered once per
/// process.
pub fn the_node_type_reading_its_settings_at_setup() -> NodeClassImportPath {
    register_the_node_type_once();
    the_fixture_node_type()
}

/// Forget each setup a node of the type ran, for a test starting afresh.
pub fn forget_every_setup_run() {
    SETUPS_RUN.lock().clear();
    SETUPS_AND_TEARDOWNS_RUN.lock().clear();
}

/// Each setup and teardown a node of the type ran, by node, in the order run.
pub fn each_setup_and_teardown_run() -> Vec<(Uuid, WhatANodeRan)> {
    SETUPS_AND_TEARDOWNS_RUN.lock().clone()
}

/// The settings each setup the node `node` ran read, in the order run.
pub fn the_settings_each_setup_read(node: Uuid) -> Vec<Map<String, Value>> {
    SETUPS_RUN
        .lock()
        .iter()
        .filter(|(set_up, _)| *set_up == node)
        .map(|(_, settings)| settings.clone())
        .collect()
}
