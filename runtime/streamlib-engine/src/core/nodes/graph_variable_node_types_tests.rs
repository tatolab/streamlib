// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::graph_variable_node_types::the_settings_of_a_graph_variable_node;
use crate::core::descriptors::{DeclaredPinType, NodeClassImportPath, NodeInputMode, PinDeclaration, PinType, PortDescriptor, SettingReadTime};
use crate::core::graph::config_keys_of_a_node_type_holding_a_node_reference;
use crate::core::graph::graph_variable_resolution::{
    GET_VARIABLE_NODE_TYPE, SET_VARIABLE_NODE_TYPE,
};
use crate::core::nodes::NodeTypesOneStreamResolves;

fn node_type(import_path: &str) -> NodeClassImportPath {
    NodeClassImportPath::new(import_path.to_string()).expect("the import path is valid")
}

/// Each setting of `import_path`: its name, its pin type, whether it may be unset, and its mode.
fn the_settings_of(import_path: &str) -> Vec<(String, PinType, bool, Option<NodeInputMode>)> {
    the_settings_of_a_graph_variable_node(&node_type(import_path))
        .expect("a variable node type declares its settings")
        .iter()
        .map(|setting| {
            let declared = setting.declared_pin().expect("a setting declares its pin");
            let DeclaredPinType::Is { pin_type } = declared.pin_type else {
                panic!("`{}` declares exactly one type", setting.name);
            };
            (
                setting.name.clone(),
                pin_type,
                declared.may_be_unset,
                setting.input_mode,
            )
        })
        .collect()
}

const READ_AT_SETUP: Option<NodeInputMode> = Some(NodeInputMode::SettingOnly {
    read: SettingReadTime::AtSetup,
});

/// Mental-revert: reading `variable` and `node_reference` from a config, as before, declares no
/// setting, so neither type lists one.
#[test]
fn a_set_variable_and_a_get_variable_declare_their_settings_as_inputs_read_at_setup() {
    assert_eq!(
        the_settings_of(SET_VARIABLE_NODE_TYPE),
        [
            (
                "variable".to_string(),
                PinType::String {},
                false,
                READ_AT_SETUP
            ),
            (
                "node_reference".to_string(),
                PinType::NodeReference {
                    referenced_node_type: None,
                    one_referrer_per_referenced_node: false,
                },
                true,
                READ_AT_SETUP,
            ),
        ]
    );
    assert_eq!(
        the_settings_of(GET_VARIABLE_NODE_TYPE),
        [(
            "variable".to_string(),
            PinType::String {},
            false,
            READ_AT_SETUP
        )]
    );
    assert!(
        the_settings_of_a_graph_variable_node(&node_type("tatolab.stream:GraphInput")).is_none()
    );
}

/// Mental-revert: finding a `SetVariable`'s reference by a key named in code, as before, rather
/// than by its input of the `NodeReference` type, finds the same key; dropping the input's type
/// finds none.
#[test]
fn the_reference_a_set_variable_holds_is_found_through_its_node_reference_input() {
    let keys = config_keys_of_a_node_type_holding_a_node_reference(
        &NodeTypesOneStreamResolves::new(),
        &node_type(SET_VARIABLE_NODE_TYPE),
    );
    assert_eq!(
        keys.iter()
            .map(|key| key.config_key.as_str())
            .collect::<Vec<_>>(),
        ["node_reference"]
    );
    assert!(!keys[0].takes_a_value_other_than_a_node_reference);
    assert!(
        config_keys_of_a_node_type_holding_a_node_reference(
            &NodeTypesOneStreamResolves::new(),
            &node_type(GET_VARIABLE_NODE_TYPE),
        )
        .is_empty()
    );
}

/// Mental-revert: reading references only from the keys a config schema marks, as before, finds
/// none on a typed node whose input is a `NodeReference`, and keeps an input of another type out.
#[test]
fn an_input_of_the_node_reference_type_is_found_as_a_key_holding_a_reference() {
    let input = |name: &str, pin_type: PinType| {
        PortDescriptor::new(name, "", false, PinDeclaration::exactly(pin_type))
    };
    let keys = streamlib_node_schema::inputs_holding_a_node_reference(&[
        input("label", PinType::String {}),
        input(
            "gateway",
            PinType::NodeReference {
                referenced_node_type: Some(node_type("acme.net:Gateway")),
                one_referrer_per_referenced_node: true,
            },
        ),
    ]);
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].config_key, "gateway");
    assert_eq!(
        keys[0].referenced_node_type_import_path.as_deref(),
        Some("acme.net:Gateway")
    );
    assert!(keys[0].one_referrer_per_referenced_node);
}
