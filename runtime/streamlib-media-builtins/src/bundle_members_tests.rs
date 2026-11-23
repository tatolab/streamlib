// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::descriptors::{
    DeclaredPinType, NodeClassImportPath, NodeDescriptor, NodeInputMode, PinType, PortDescriptor,
    SettingReadTime,
};
use streamlib::sdk::nodes::NODE_REGISTRY;

use crate::bundle_members::{BreakBundle, MakeBundle};

fn the_registered(node_class_import_path: &NodeClassImportPath) -> NodeDescriptor {
    crate::register_media_builtin_node_types();
    NODE_REGISTRY
        .descriptor(node_class_import_path)
        .expect("the built-in registers")
}

fn the_port<'descriptor>(
    ports: &'descriptor [PortDescriptor],
    name: &str,
) -> &'descriptor PortDescriptor {
    ports
        .iter()
        .find(|port| port.name == name)
        .unwrap_or_else(|| panic!("`{name}` is declared"))
}

/// Each node carries the bundle its `bundle_type` setting chooses on its one input and its one
/// output, and that setting chooses any pin type, holds none until it is set, and is read at
/// setup.
///
/// Mental-revert: a port stating a type of its own would carry it whatever `bundle_type` holds.
#[test]
fn both_nodes_carry_the_bundle_their_bundle_type_setting_chooses() {
    for (descriptor, input, output) in [
        (
            the_registered(&MakeBundle::node_class_import_path()),
            "members",
            "bundle",
        ),
        (
            the_registered(&BreakBundle::node_class_import_path()),
            "bundle",
            "members",
        ),
    ] {
        let typed_by_the_setting = DeclaredPinType::SetByInput {
            input_name: "bundle_type".to_owned(),
        };
        for port in [
            the_port(&descriptor.inputs, input),
            the_port(&descriptor.outputs, output),
        ] {
            let declared = port.pin_declaration.as_ref().expect("the port has a pin");
            assert_eq!(declared.pin_type, typed_by_the_setting, "{}", port.name);
        }
        let setting = the_port(&descriptor.inputs, "bundle_type");
        let declared = setting
            .pin_declaration
            .as_ref()
            .expect("the setting has a pin");
        assert_eq!(
            declared.pin_type,
            DeclaredPinType::Is {
                pin_type: PinType::PinTypeChoice { among: Vec::new() }
            }
        );
        assert!(declared.may_be_unset);
        assert_eq!(setting.pin_default, None);
        assert_eq!(
            setting.input_mode,
            Some(NodeInputMode::SettingOnly {
                read: SettingReadTime::AtSetup
            })
        );
    }
}
