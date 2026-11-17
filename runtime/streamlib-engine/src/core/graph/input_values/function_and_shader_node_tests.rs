// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The input values of a function node and a shader node, each of a type its stream's node
//! interpreter describes, are checked as a process node's are: a typed input takes what its pin
//! type takes within its declared range, an input that takes only a link refuses a value, and a
//! name that is none of its inputs is not found.

use serde_json::json;

use super::*;
use crate::core::compiler::compiler_ops::function_node_island::tests::a_function_node_type;
use crate::core::compiler::compiler_ops::shader_node_island_program::tests::shader_program;
use crate::core::descriptors::{
    NodeClassShortName, NodeDescriptor, PinDeclaration, PinType, PortDescriptor,
    PortFamilyDeclaration,
};
use crate::core::error::Error;
use crate::core::nodes::HowANodeOfADescribedTypeRuns;

const SCALE: &str = "test_ops:scale";

const TINT: &str = "test_shaders:tint";

/// The node types `scale`, a function node type whose `x` is a Float and whose `y` names no pin
/// type, and `tint`, a shader node type taking an Image `image` and a Float `amount` from 0 to 1.
fn node_types_resolving_a_function_and_a_shader_node_type() -> NodeTypesOneStreamResolves {
    let node_types = NodeTypesOneStreamResolves::new();
    let mut scale = a_function_node_type(SCALE, &["y"], &["result"]);
    scale = scale.with_input(
        PortDescriptor::new(
            "x",
            "",
            true,
            PortFamilyDeclaration::of_family(PortFamily::Channels),
        )
        .with_delivery_profile("newest")
        .with_pin_declaration(PinDeclaration::exactly(PinType::Float { exact_size: None })),
    );
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            scale,
            HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                node_once_promoted_to_its_own_process: Box::new(|node| {
                    Err(Error::Configuration(format!(
                        "`{}` builds no node in this test",
                        node.id
                    )))
                }),
            },
        )
        .unwrap();

    let mut tint = NodeDescriptor::new(
        NodeClassShortName::new("tint").unwrap(),
        NodeClassImportPath::new(TINT).unwrap(),
        "a shader node type for a test",
    );
    tint.node_kind = NodeKind::Shader;
    let mut amount = PinDeclaration::exactly(PinType::Float { exact_size: None });
    amount.annotations.range = Some(PinNumberRange {
        minimum: Some(0.0),
        maximum: Some(1.0),
    });
    tint = tint
        .with_input(
            PortDescriptor::new(
                "image",
                "",
                true,
                PortFamilyDeclaration::of_family(PortFamily::Image),
            )
            .with_delivery_profile("newest"),
        )
        .with_input(
            PortDescriptor::new(
                "amount",
                "",
                true,
                PortFamilyDeclaration::of_family(PortFamily::Channels),
            )
            .with_delivery_profile("newest")
            .with_pin_declaration(amount),
        )
        .with_output(PortDescriptor::new(
            "result",
            "",
            true,
            PortFamilyDeclaration::of_family(PortFamily::Image),
        ));
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            tint,
            HowANodeOfADescribedTypeRuns::OnTheNativeShaderEvaluatorOfItsIsland {
                shader_node_program: shader_program(
                    "tint",
                    "return image.pixel * amount;",
                    &["image"],
                    &["amount"],
                ),
                evaluator_of_the_island_it_hosts: Box::new(|node| {
                    Err(Error::Configuration(format!(
                        "`{}` builds no node in this test",
                        node.id
                    )))
                }),
            },
        )
        .unwrap();
    node_types
}

/// The refusal, if any, of `value` set on input `input` of a node of `node_type` named `node`.
fn the_refusal_of(
    node_types: &NodeTypesOneStreamResolves,
    node_type: &str,
    input: &str,
    value: Option<Value>,
) -> Result<(), InputValueRefusal> {
    let mut input_values_after_the_set = Map::new();
    if let Some(value) = &value {
        input_values_after_the_set.insert(input.to_owned(), value.clone());
    }
    refuse_an_input_value_its_input_does_not_take(
        node_types,
        &NodeAnInputValueIsSetOn {
            name: "node",
            node_type: &NodeClassImportPath::new(node_type).unwrap(),
            input_values_after_the_set: &input_values_after_the_set,
        },
        input,
        value.as_ref(),
    )
}

fn the_code_of(refusal: Result<(), InputValueRefusal>) -> InputValueRefusalCode {
    refusal.expect_err("the value is refused").code
}

#[test]
fn a_function_nodes_input_values_are_checked() {
    let node_types = node_types_resolving_a_function_and_a_shader_node_type();
    assert_eq!(
        the_refusal_of(&node_types, SCALE, "x", Some(json!(0.5))),
        Ok(())
    );
    assert_eq!(the_refusal_of(&node_types, SCALE, "x", None), Ok(()));
    assert_eq!(
        the_code_of(the_refusal_of(&node_types, SCALE, "x", Some(json!("loud")))),
        InputValueRefusalCode::InputValueNotValid
    );
    assert_eq!(
        the_refusal_of(&node_types, SCALE, "y", Some(json!("any value"))),
        Ok(()),
        "an input naming no pin type takes any value a function reads"
    );
    assert_eq!(
        the_code_of(the_refusal_of(&node_types, SCALE, "z", Some(json!(1)))),
        InputValueRefusalCode::PortNotFound
    );
    assert!(!node_type_takes_a_config(
        &node_types,
        &NodeClassImportPath::new(SCALE).unwrap()
    ));
}

#[test]
fn a_shader_nodes_input_values_are_checked() {
    let node_types = node_types_resolving_a_function_and_a_shader_node_type();
    assert_eq!(
        the_refusal_of(&node_types, TINT, "amount", Some(json!(0.5))),
        Ok(())
    );
    assert_eq!(
        the_code_of(the_refusal_of(
            &node_types,
            TINT,
            "amount",
            Some(json!(2.0))
        )),
        InputValueRefusalCode::InputValueNotValid,
        "2 lies outside the range and is never clamped"
    );
    assert_eq!(
        the_code_of(the_refusal_of(
            &node_types,
            TINT,
            "amount",
            Some(json!(true))
        )),
        InputValueRefusalCode::InputValueNotValid
    );
    assert_eq!(
        the_code_of(the_refusal_of(&node_types, TINT, "image", Some(json!(1)))),
        InputValueRefusalCode::InputTakesNoValue
    );
    assert_eq!(
        the_code_of(the_refusal_of(
            &node_types,
            TINT,
            "brightness",
            Some(json!(1))
        )),
        InputValueRefusalCode::PortNotFound
    );
}
