// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_node_schema::pin_type::{FloatSize, PinFlow, PinType, TexturePixelFormat};

use super::pin_declaration_tokens::pin_declaration_tokens;

fn emitted(pin_type: Option<&PinType>) -> String {
    emitted_in(pin_type, None)
}

fn emitted_in(pin_type: Option<&PinType>, flow: Option<PinFlow>) -> String {
    pin_declaration_tokens(pin_type, flow)
        .to_string()
        .split_whitespace()
        .collect()
}

#[test]
fn a_port_with_no_pin_type_emits_none() {
    assert_eq!(emitted(None), "::std::option::Option::None");
}

#[test]
fn a_pin_type_is_emitted_as_the_typed_value_it_is_with_no_parse_at_run_time() {
    let texture = emitted(Some(&PinType::Texture {
        pixel_formats: vec![TexturePixelFormat::Bgra8Unorm, TexturePixelFormat::Nv12],
    }));
    assert_eq!(
        texture,
        "::std::option::Option::Some(__streamlib_sdk::descriptors::PinDeclaration::exactly(\
         __streamlib_sdk::descriptors::PinType::Texture{pixel_formats:::std::vec![\
         __streamlib_sdk::descriptors::TexturePixelFormat::Bgra8Unorm,\
         __streamlib_sdk::descriptors::TexturePixelFormat::Nv12],}))"
    );

    let float = emitted(Some(&PinType::Float {
        exact_size: Some(FloatSize::Float32),
    }));
    assert!(
        float.contains(
            "PinType::Float{exact_size:::std::option::Option::Some(\
             __streamlib_sdk::descriptors::FloatSize::Float32)}"
        ),
        "{float}"
    );
    for emitted in [texture, float] {
        assert!(!emitted.contains("serde_json"), "{emitted}");
    }
}

#[test]
fn a_pin_type_the_grammar_never_declares_emits_a_compile_error_naming_it() {
    let menu = emitted(Some(&PinType::Menu {
        option_names: vec!["sine".into()],
    }));
    assert!(menu.contains("compile_error!"), "{menu}");
    assert!(menu.contains("Menu"), "{menu}");
}

#[test]
fn a_stated_flow_is_emitted_over_the_types_default() {
    assert_eq!(
        emitted_in(
            Some(&PinType::Float { exact_size: None }),
            Some(PinFlow::Stream)
        ),
        "::std::option::Option::Some(__streamlib_sdk::descriptors::PinDeclaration{\
         flow:__streamlib_sdk::descriptors::PinFlow::Stream,\
         ..__streamlib_sdk::descriptors::PinDeclaration::exactly(\
         __streamlib_sdk::descriptors::PinType::Float{exact_size:::std::option::Option::None})})"
    );
}
