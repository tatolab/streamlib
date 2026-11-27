// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use quote::quote;

use super::*;

#[test]
fn an_input_of_a_value_is_a_value_input_and_no_port() {
    let parsed = parse_ok(quote! {
        execution = reactive,
        input("video", pin_type = Texture),
        input("gain", description = "How loud.", value = f32, default = 0.5),
    });
    assert_eq!(parsed.inputs.len(), 1);
    assert_eq!(parsed.inputs[0].cast_name, "video");
    assert_eq!(parsed.value_inputs.len(), 1);
    let gain = &parsed.value_inputs[0];
    assert_eq!(gain.declared_ident, "gain");
    assert_eq!(gain.cast_name, "gain");
    let value_type = &gain.value_type;
    let default = &gain.default;
    assert_eq!(quote! { #value_type }.to_string(), "f32");
    assert_eq!(quote! { #default }.to_string(), "0.5");
    assert_eq!(parsed.to_node_schema().inputs.len(), 1);
}

/// A value input chosen from a list states its options, and one choosing a bundle its stream
/// declares states so; one stating neither is chosen from any value of its type.
///
/// Mental-revert: options dropped by the grammar declare a String the studio offers no list
/// for.
#[test]
fn a_value_input_states_what_its_value_is_chosen_from() {
    let parsed = parse_ok(quote! {
        execution = reactive,
        input("format", value = String, default = "a", options = ["a", "b"]),
        input("bundle_type", value = Option<PinType>, default = None,
              among_the_bundles_its_stream_declares = true),
        input("gain", value = f32, default = 0.5),
    });
    let ValueInputChosenFrom::Options(options) = &parsed.value_inputs[0].chosen_from else {
        panic!("`format` is chosen from its options");
    };
    assert_eq!(
        options.iter().map(syn::LitStr::value).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(matches!(
        parsed.value_inputs[1].chosen_from,
        ValueInputChosenFrom::TheBundlesItsStreamDeclares
    ));
    assert!(matches!(
        parsed.value_inputs[2].chosen_from,
        ValueInputChosenFrom::AnyValueOfItsType
    ));
}

#[test]
fn a_value_input_chosen_from_options_and_the_bundles_is_an_error() {
    let msg = parse_err(quote! {
        execution = reactive,
        input("kind", value = Option<PinType>, default = None, options = ["a"],
              among_the_bundles_its_stream_declares = true),
    });
    assert!(
        msg.contains("value input `kind` is chosen from `options = […]` or among the bundles"),
        "got: {msg}"
    );
}

#[test]
fn a_value_input_listing_no_options_is_an_error() {
    let msg = parse_err(quote! {
        execution = reactive,
        input("format", value = String, default = "a", options = []),
    });
    assert!(
        msg.contains(
            "`options` lists the names value input `format` is chosen from, and lists none"
        ),
        "got: {msg}"
    );
}

#[test]
fn options_on_a_port_are_an_error() {
    let msg = parse_err(quote! {
        execution = reactive,
        input("format", pin_type = String, options = ["a"]),
    });
    assert!(
        msg.contains("port `format` declares `options`, which a value input states"),
        "got: {msg}"
    );
}

#[test]
fn options_on_an_output_are_an_error() {
    let msg = parse_err(quote! {
        execution = reactive,
        output("format", pin_type = String, options = ["a"]),
    });
    assert!(
        msg.contains("`options` is declared by a value input, not `output(\"format\", ...)`"),
        "got: {msg}"
    );
}

#[test]
fn a_value_input_without_a_default_is_an_error() {
    let msg = parse_err(quote! {
        execution = reactive,
        input("gain", value = f32),
    });
    assert!(
        msg.contains("value input `gain` declares no `default = …`"),
        "got: {msg}"
    );
}

#[test]
fn a_default_without_a_value_type_is_an_error() {
    let msg = parse_err(quote! {
        execution = reactive,
        input("gain", default = 0.5),
    });
    assert!(
        msg.contains("input `gain` declares a `default` and no `value = <type>`"),
        "got: {msg}"
    );
}

#[test]
fn a_value_input_stating_a_port_key_is_an_error() {
    let msg = parse_err(quote! {
        execution = reactive,
        input("gain", value = f32, default = 0.5, pin_type = Float),
    });
    assert!(
        msg.contains("value input `gain` declares `pin_type`, which a port takes"),
        "got: {msg}"
    );
}

#[test]
fn a_value_on_an_output_is_an_error() {
    let msg = parse_err(quote! {
        execution = reactive,
        output("level", value = f32),
    });
    assert!(
        msg.contains("`value` is declared by a value input, not `output(\"level\", ...)`"),
        "got: {msg}"
    );
}

#[test]
fn a_value_input_and_a_port_sharing_a_name_is_an_error() {
    let msg = parse_err(quote! {
        execution = reactive,
        input("gain", pin_type = Float),
        input("gain", value = f32, default = 0.5),
    });
    assert!(
        msg.contains("the port name `gain` is declared more than once"),
        "got: {msg}"
    );
}

#[test]
fn a_port_stating_a_default_is_told_a_port_takes_none() {
    let msg = parse_err(quote! {
        execution = reactive,
        input("gain", pin_type = Float, default = 0.5),
    });
    assert!(msg.contains("a port takes no `default`"), "got: {msg}");
}

#[test]
fn a_bundle_choice_on_a_port_is_an_error_even_when_false() {
    let msg = parse_err(quote! {
        execution = reactive,
        input("x", pin_type = Float, among_the_bundles_its_stream_declares = false),
    });
    assert!(
        msg.contains("port `x` declares `among_the_bundles_its_stream_declares`"),
        "got: {msg}"
    );
}
