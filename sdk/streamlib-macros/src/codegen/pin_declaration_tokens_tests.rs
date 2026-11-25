// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_node_schema::pin_type::{FloatSize, PinFlow, PinType, TexturePixelFormat};

use super::pin_declaration_tokens::pin_declaration_tokens;

fn emitted(pin_type: Option<&PinType>) -> String {
    emitted_in(pin_type, None)
}

fn emitted_in(pin_type: Option<&PinType>, flow: Option<PinFlow>) -> String {
    pin_declaration_tokens(pin_type, None, flow, &[], false, &Default::default())
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

#[test]
fn the_names_a_port_had_and_that_it_may_be_unset_are_emitted_beside_its_flow() {
    let emitted: String = pin_declaration_tokens(
        Some(&PinType::Float { exact_size: None }),
        None,
        Some(PinFlow::Stream),
        &["level".to_owned(), "amount".to_owned()],
        true,
        &Default::default(),
    )
    .to_string()
    .split_whitespace()
    .collect();
    assert_eq!(
        emitted,
        "::std::option::Option::Some(__streamlib_sdk::descriptors::PinDeclaration{\
         flow:__streamlib_sdk::descriptors::PinFlow::Stream,\
         former_names:::std::vec![::std::string::String::from(\"level\"),\
         ::std::string::String::from(\"amount\")],\
         may_be_unset:true,\
         ..__streamlib_sdk::descriptors::PinDeclaration::exactly(\
         __streamlib_sdk::descriptors::PinType::Float{exact_size:::std::option::Option::None})})"
    );
}

#[test]
fn a_port_typed_by_a_setting_carries_the_type_it_chooses_as_a_value() {
    let emitted: String = pin_declaration_tokens(
        None,
        Some("bundle_type"),
        None,
        &[],
        false,
        &Default::default(),
    )
    .to_string()
    .split_whitespace()
    .collect();
    assert_eq!(
        emitted,
        "::std::option::Option::Some(__streamlib_sdk::descriptors::PinDeclaration{\
         pin_type:__streamlib_sdk::descriptors::DeclaredPinType::SetByInput{\
         input_name:::std::string::String::from(\"bundle_type\"),},\
         flow:__streamlib_sdk::descriptors::PinFlow::Value,\
         annotations:::std::default::Default::default(),\
         former_names:::std::vec![],may_be_unset:false,})"
    );
}
