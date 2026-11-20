// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node type for tests that reads three settings once, at setup, and records each setup it
//! runs: what a test of a node rebuilt to take a setting read at setup runs.

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

/// Each setup the fixture node ran, by node: the settings it read.
static SETUPS_RUN: Mutex<Vec<(Uuid, Map<String, Value>)>> = Mutex::new(Vec::new());

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
        if self.settings.get("kernel_size") == Some(&json!(A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH)) {
            return Err(Error::Configuration(format!(
                "a kernel of size {A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH} blurs nothing"
            )));
        }
        Ok(())
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
