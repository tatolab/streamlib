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

/// Each node carries the bundle its `bundle_type` input chooses on its one input and its one
/// output, and that input chooses one of the bundles its stream's node types declare, holds
/// none until it is set, and is read each run.
///
/// Mental-revert: a port stating a type of its own would carry it whatever `bundle_type` holds,
/// and an input unmarked chooses among any pin type.
#[test]
fn both_nodes_carry_the_bundle_their_bundle_type_input_chooses() {
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
                pin_type: PinType::a_choice_among_the_bundles_its_stream_declares()
            }
        );
        assert!(declared.may_be_unset);
        assert_eq!(setting.pin_default, None);
        assert_eq!(
            setting.input_mode,
            Some(NodeInputMode::SettingOnly {
                read: SettingReadTime::EachRun
            })
        );
    }
}

/// `MakeBundle`'s `members` takes a value set on it as well as a wire, for each member no wire
/// reaches, and `BreakBundle`'s `bundle` takes a wire alone.
///
/// Mental-revert: an input stating no mode is read off its Channels alone, so a value set on the
/// Records `members` is refused.
#[test]
fn make_bundles_members_takes_a_value_and_break_bundles_bundle_takes_only_a_wire() {
    let make = the_registered(&MakeBundle::node_class_import_path());
    let break_ = the_registered(&BreakBundle::node_class_import_path());

    assert_eq!(
        the_port(&make.inputs, "members").input_mode,
        Some(NodeInputMode::WireOrValue)
    );
    assert_eq!(the_port(&break_.inputs, "bundle").input_mode, None);
}
