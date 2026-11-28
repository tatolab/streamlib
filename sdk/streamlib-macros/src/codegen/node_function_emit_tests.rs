// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `#[node(…)]` on a function: each parameter read by its type, the per-frame function's type
//! made `Send + 'static`, each value input declared on the descriptor, and each form the
//! function does not take refused naming the fix.

use quote::quote;

use super::node_function_emit::NODE_WRITTEN_AS_ONE_FUNCTION;
use super::{generate_from_a_node_function, refusal_of_a_node_not_written_as_a_function};
use crate::grammar::{DeclaredNodeMeta, parse2};

/// The expansion of `function` under `attribute`, without whitespace.
fn expanded(attribute: proc_macro2::TokenStream, function: proc_macro2::TokenStream) -> String {
    expanded_with_its_meta(DeclaredNodeMeta::default(), attribute, function)
}

/// The expansion of `function` under `attribute` and the `#[meta(...)]` that declares `meta`,
/// without whitespace.
fn expanded_with_its_meta(
    meta: DeclaredNodeMeta,
    attribute: proc_macro2::TokenStream,
    function: proc_macro2::TokenStream,
) -> String {
    let function: syn::ItemFn = syn::parse2(function).expect("the function parses");
    let parsed = parse2(
        attribute,
        meta,
        &proc_macro2::Ident::new("ANode", proc_macro2::Span::call_site()),
    )
    .expect("the attribute parses");
    generate_from_a_node_function(function, &parsed, quote! { ::streamlib::sdk })
        .to_string()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

fn a_gain_and_a_video_port() -> proc_macro2::TokenStream {
    quote! {
        execution = reactive,
        input("video", pin_type = Texture),
        input("gain", description = "How loud.", value = f32, default = 0.5),
        output("out", pin_type = Texture, formats = ["rgba8_unorm"]),
    }
}

#[test]
fn the_body_is_called_with_each_parameter_its_type_names() {
    let rendered = expanded(
        a_gain_and_a_video_port(),
        quote! {
            fn a_node(
                outputs: OutputWriter,
                gain: Input<f32>,
                ctx: &RuntimeContextFullAccess<'_>,
                inputs: InputMailboxes,
            ) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
                move |_ctx| Ok(())
            }
        },
    );
    assert!(
        rendered.contains("::std::boxed::Box::new(super::a_node(outputs,handles.gain,ctx,inputs))"),
        "{rendered}"
    );
    assert!(rendered.contains("modANode{"), "{rendered}");
}

#[test]
fn the_per_frame_function_returned_is_made_send_and_static() {
    let rendered = expanded(
        a_gain_and_a_video_port(),
        quote! {
            fn a_node(gain: Input<f32>) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
                move |_ctx| Ok(())
            }
        },
    );
    assert!(
        rendered.contains("->Result<()>+::core::marker::Send+'static{"),
        "{rendered}"
    );
}

#[test]
fn a_body_that_can_fail_is_called_with_its_error_handed_on() {
    let rendered = expanded(
        a_gain_and_a_video_port(),
        quote! {
            fn a_node(
                gain: Input<f32>,
            ) -> Result<impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> + Send> {
                Ok(move |_ctx| Ok(()))
            }
        },
    );
    assert!(
        rendered.contains("::std::boxed::Box::new(super::a_node(handles.gain)?)"),
        "{rendered}"
    );
    assert!(
        rendered.contains("->Result<()>+Send+'static>{"),
        "a `Send` the author wrote is not written again: {rendered}"
    );
}

#[test]
fn each_value_input_is_declared_on_the_descriptor_with_its_default() {
    let rendered = expanded(
        a_gain_and_a_video_port(),
        quote! {
            fn a_node(gain: Input<f32>) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
                move |_ctx| Ok(())
            }
        },
    );
    assert!(
        rendered.contains(
            "the_input_taking_no_wire(\"gain\",\"Howloud.\",<f32as__streamlib_sdk::descriptors::ValueInputPinType>::value_input_pin_type(),__streamlib_sdk::serde_json::to_value(<_as__streamlib_sdk::nodes::__generated_private::TheDefaultOfAValueInput<f32>>::into_the_default(0.5))"
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains("gain:__streamlib_sdk::nodes::Input<f32>,"),
        "{rendered}"
    );
}

/// A value input chosen from its options is declared a Menu of them, and one choosing a bundle
/// its stream declares is declared that choice, each checked against the type it holds.
///
/// Mental-revert: declaring each by its type alone serves `format` as a plain String and
/// `bundle_type` as a choice of any pin type.
#[test]
fn a_value_input_chosen_from_a_list_is_declared_with_the_list() {
    let rendered = expanded(
        quote! {
            execution = reactive,
            input("format", value = String, default = "a", options = ["a", "b"]),
            input("bundle_type", value = Option<PinType>, default = None,
                  among_the_bundles_its_stream_declares = true),
        },
        quote! {
            fn a_node(
                format: Input<String>,
                bundle_type: Input<Option<PinType>>,
            ) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
                move |_ctx| Ok(())
            }
        },
    );
    assert!(
        rendered.contains(
            "the_input_taking_no_wire(\"format\",\"\",__streamlib_sdk::descriptors::the_menu_of_a_value_input::<String>(&[\"a\",\"b\"],),"
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "the_input_taking_no_wire(\"bundle_type\",\"\",__streamlib_sdk::descriptors::the_choice_among_the_bundles_its_stream_declares::<Option<PinType>,>(),"
        ),
        "{rendered}"
    );
}

fn refusal_of(attribute: proc_macro2::TokenStream, function: proc_macro2::TokenStream) -> String {
    let rendered = expanded(attribute, function);
    let start = rendered
        .find("compile_error!")
        .unwrap_or_else(|| panic!("the expansion refuses the function: {rendered}"));
    rendered[start..].to_owned()
}

fn a_function_of_its_gain() -> proc_macro2::TokenStream {
    quote! {
        fn a_node(gain: Input<f32>) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
            move |_ctx| Ok(())
        }
    }
}

fn a_gain_on_its_own_clock() -> proc_macro2::TokenStream {
    quote! { execution = manual, input("gain", value = f32, default = 0.5) }
}

#[test]
fn a_node_pacing_itself_returns_nothing_and_has_nothing_to_start_or_stop() {
    let rendered = expanded(
        a_gain_on_its_own_clock(),
        quote! {
            fn a_node(gain: Input<f32>) {
                let _published = computed(move |_ctx| Ok::<_, String>(*gain.get()));
            }
        },
    );
    assert!(
        rendered.contains("||{super::a_node(handles.gain);Ok(::std::option::Option::None)}"),
        "{rendered}"
    );
    assert!(rendered.contains("fnstart("), "{rendered}");
    assert!(rendered.contains("fnstop("), "{rendered}");
}

#[test]
fn a_node_pacing_itself_written_returning_unit_returns_nothing() {
    let rendered = expanded(
        a_gain_on_its_own_clock(),
        quote! {
            fn a_node(gain: Input<f32>) -> () {}
        },
    );
    assert!(
        rendered.contains("||{super::a_node(handles.gain);Ok(::std::option::Option::None)}"),
        "{rendered}"
    );
}

#[test]
fn a_node_pacing_itself_whose_body_can_fail_hands_its_error_on() {
    let rendered = expanded(
        a_gain_on_its_own_clock(),
        quote! {
            fn a_node(gain: Input<f32>) -> Result<()> {
                Ok(())
            }
        },
    );
    assert!(
        rendered.contains("||{super::a_node(handles.gain)?;Ok(::std::option::Option::None)}"),
        "{rendered}"
    );
}

#[test]
fn a_node_pacing_itself_that_returns_a_per_frame_function_is_refused() {
    let refusal = refusal_of(a_gain_on_its_own_clock(), a_function_of_its_gain());
    assert!(refusal.contains("returnsnoper-framefunction"), "{refusal}");
}

#[test]
fn a_frame_driven_node_has_no_start_or_stop_of_its_own() {
    let rendered = expanded(a_gain_and_a_video_port(), a_function_of_its_gain());
    assert!(!rendered.contains("fnstart("), "{rendered}");
    assert!(!rendered.contains("fnstop("), "{rendered}");
}

#[test]
fn a_parameter_reading_an_undeclared_value_input_is_refused() {
    let refusal = refusal_of(
        quote! { execution = reactive, input("gain", value = f32, default = 0.5) },
        quote! {
            fn a_node(gain: Input<f32>, level: Input<f32>)
                -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()>
            {
                move |_ctx| Ok(())
            }
        },
    );
    assert!(
        refusal.contains("parameter`level`readsavalueinputitsnodedoesnotdeclare"),
        "{refusal}"
    );
}

#[test]
fn a_value_input_no_parameter_reads_is_refused() {
    let refusal = refusal_of(
        quote! { execution = reactive, input("gain", value = f32, default = 0.5) },
        quote! {
            fn a_node() -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
                move |_ctx| Ok(())
            }
        },
    );
    assert!(
        refusal.contains("valueinput`gain`isreadbynoparameter"),
        "{refusal}"
    );
}

#[test]
fn a_parameter_of_a_type_a_node_function_does_not_take_is_refused() {
    let refusal = refusal_of(
        quote! { execution = reactive, input("gain", value = f32, default = 0.5) },
        quote! {
            fn a_node(gain: Input<f32>, ctx: &RuntimeContextLimitedAccess<'_>)
                -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()>
            {
                move |_ctx| Ok(())
            }
        },
    );
    assert!(
        refusal.contains("parameter`ctx`ofanodefunctionisnotoneanodefunctiontakes"),
        "{refusal}"
    );
}

#[test]
fn input_mailboxes_of_a_node_with_no_stream_input_are_refused() {
    let refusal = refusal_of(
        quote! { execution = reactive, input("gain", value = f32, default = 0.5) },
        quote! {
            fn a_node(gain: Input<f32>, inputs: InputMailboxes)
                -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()>
            {
                move |_ctx| Ok(())
            }
        },
    );
    assert!(refusal.contains("declaresnostreaminput"), "{refusal}");
}

#[test]
fn a_function_returning_no_per_frame_function_is_refused() {
    let refusal = refusal_of(
        quote! { execution = reactive, input("gain", value = f32, default = 0.5) },
        quote! {
            fn a_node(gain: Input<f32>) -> Result<()> {
                Ok(())
            }
        },
    );
    assert!(
        refusal.contains("anodefunctionreturnsitsper-framefunction"),
        "{refusal}"
    );
}

#[test]
fn a_context_taken_mutably_is_refused() {
    let refusal = refusal_of(
        quote! { execution = reactive, input("gain", value = f32, default = 0.5) },
        quote! {
            fn a_node(gain: Input<f32>, ctx: &mut RuntimeContextFullAccess<'_>)
                -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()>
            {
                move |_ctx| Ok(())
            }
        },
    );
    assert!(
        refusal.contains("parameter`ctx`ofanodefunctionisnotoneanodefunctiontakes"),
        "{refusal}"
    );
}

#[test]
fn a_function_whose_name_makes_no_node_name_is_refused() {
    let refusal = refusal_of(
        quote! { execution = reactive, input("gain", value = f32, default = 0.5) },
        quote! {
            fn _1(gain: Input<f32>) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
                move |_ctx| Ok(())
            }
        },
    );
    assert!(refusal.contains("namesnonode"), "{refusal}");
}

fn a_function_of_its_format() -> proc_macro2::TokenStream {
    quote! {
        fn a_node(format: Input<Option<PinType>>)
            -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()>
        {
            move |_ctx| Ok(())
        }
    }
}

#[test]
fn an_output_typed_by_a_value_input_declares_the_type_that_input_chooses() {
    let rendered = expanded(
        quote! {
            execution = reactive,
            input("format", value = Option<PinType>, default = None),
            output("out", pin_type = from_input(format)),
        },
        a_function_of_its_format(),
    );
    assert!(
        rendered.contains(
            "pin_type:__streamlib_sdk::descriptors::DeclaredPinType::SetByInput{input_name:::std::string::String::from(\"format\"),}"
        ),
        "{rendered}"
    );
    assert!(!rendered.contains("compile_error!"), "{rendered}");
}

#[test]
fn a_port_typed_by_an_undeclared_value_input_is_refused() {
    let refusal = refusal_of(
        quote! {
            execution = reactive,
            input("format", value = Option<PinType>, default = None),
            output("out", pin_type = from_input(shape)),
        },
        a_function_of_its_format(),
    );
    assert!(
        refusal
            .contains("takesitspintype`from_input(shape)`,andthenodedeclaresnovalueinput`shape`"),
        "{refusal}"
    );
}

#[test]
fn a_port_typed_by_a_value_input_holding_no_pin_type_is_refused() {
    let refusal = refusal_of(
        quote! {
            execution = reactive,
            input("gain", value = f32, default = 0.5),
            output("out", pin_type = from_input(gain)),
        },
        a_function_of_its_gain(),
    );
    assert!(
        refusal.contains("holdsthechoiceofone:`value=Option<PinType>`"),
        "{refusal}"
    );
}

#[test]
fn a_node_written_as_a_struct_is_refused_showing_the_node_written_as_one_function() {
    let refusal =
        refusal_of_a_node_not_written_as_a_function(proc_macro2::Span::call_site()).to_string();
    assert!(
        refusal.contains(
            "a node written as a struct, with a trait impl holding its lifecycle, is retired"
        ) && refusal.contains("pub fn gain_node(")
            && refusal.contains("gain: Input<f32>,")
            && refusal.contains("computed(move |ctx|"),
        "{refusal}"
    );
}

#[test]
fn the_node_the_refusal_of_a_struct_shows_is_one_the_macro_takes() {
    let example: String = NODE_WRITTEN_AS_ONE_FUNCTION
        .lines()
        .filter_map(|line| line.strip_prefix("    "))
        .collect::<Vec<_>>()
        .join("\n");
    let mut function: syn::ItemFn = syn::parse_str(&example).expect("the example parses");
    let attribute = function.attrs.remove(0);
    let syn::Meta::List(attribute) = attribute.meta else {
        panic!("the example's attribute is `#[node(...)]`");
    };
    let node_name = super::node_name_of_a_node_function(&function.sig.ident).expect("a node name");
    let parsed = parse2(attribute.tokens, DeclaredNodeMeta::default(), &node_name)
        .expect("the example's attribute parses");
    let rendered =
        generate_from_a_node_function(function, &parsed, quote! { ::streamlib::sdk }).to_string();
    assert!(!rendered.contains("compile_error"), "{rendered}");
}

#[test]
fn a_frame_rate_read_from_an_undeclared_value_input_is_refused() {
    let refusal = refusal_of(
        quote! {
            execution = reactive,
            input("gain", value = f32, default = 0.5),
            output("out", pin_type = Texture, formats = ["rgba8_unorm"], frame_rate = from_input(fps)),
        },
        a_function_of_its_gain(),
    );
    assert!(
        refusal
            .contains("readsits`frame_rate``from_input(fps)`,andthenodedeclaresnovalueinput`fps`"),
        "{refusal}"
    );
}

#[test]
fn a_sample_rate_read_from_an_undeclared_value_input_is_refused() {
    let refusal = refusal_of(
        quote! {
            execution = reactive,
            input("gain", value = f32, default = 0.5),
            output("out", pin_type = Audio, rate = from_input(sample_rate)),
        },
        a_function_of_its_gain(),
    );
    assert!(
        refusal.contains("readsits`rate``from_input(sample_rate)`"),
        "{refusal}"
    );
}

#[test]
fn a_frame_rate_read_from_a_declared_value_input_is_taken() {
    let rendered = expanded(
        quote! {
            execution = reactive,
            input("fps", value = u32, default = 30),
            output("out", pin_type = Texture, formats = ["rgba8_unorm"], frame_rate = from_input(fps)),
        },
        quote! {
            fn a_node(fps: Input<u32>, outputs: OutputWriter) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
                move |_ctx| Ok(())
            }
        },
    );
    assert!(!rendered.contains("compile_error"), "{rendered}");
}

fn a_node_of_its_gain_with(attribute: proc_macro2::TokenStream) -> String {
    expanded(
        quote! { #attribute input("gain", value = f32, default = 0.5), },
        a_function_of_its_gain(),
    )
}

#[test]
fn only_a_node_paced_by_real_time_declares_it_on_its_descriptor() {
    assert!(
        a_node_of_its_gain_with(quote! { execution = reactive, paced_by_real_time, })
            .contains(".paced_by_real_time()")
    );
    assert!(
        !a_node_of_its_gain_with(quote! { execution = reactive, }).contains("paced_by_real_time")
    );
}

#[test]
fn only_a_node_that_declares_a_category_sets_one_on_its_descriptor() {
    let rendered_for = |category: Option<&str>| {
        expanded_with_its_meta(
            DeclaredNodeMeta {
                category: category.map(str::to_owned),
                ..DeclaredNodeMeta::default()
            },
            quote! { execution = reactive, input("gain", value = f32, default = 0.5), },
            a_function_of_its_gain(),
        )
    };
    assert!(rendered_for(Some("Effects/Stylize")).contains(".with_category(\"Effects/Stylize\")"));
    assert!(!rendered_for(None).contains("with_category"));
}

#[test]
fn only_a_node_that_states_a_conversion_says_so_on_its_descriptor() {
    let rendered_for = |conversion: Option<bool>| {
        expanded_with_its_meta(
            DeclaredNodeMeta {
                conversion,
                ..DeclaredNodeMeta::default()
            },
            a_gain_and_a_video_port(),
            quote! {
                fn a_node(gain: Input<f32>, inputs: InputMailboxes, outputs: OutputWriter)
                    -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
                    move |_ctx| Ok(())
                }
            },
        )
    };
    assert!(rendered_for(Some(true)).contains(".stating_a_conversion()"));
    assert!(!rendered_for(None).contains("stating_a_conversion"));
}

#[test]
fn the_descriptor_captures_its_identity_with_module_path_once() {
    let rendered = a_node_of_its_gain_with(quote! { execution = reactive, });
    assert_eq!(
        rendered.matches("module_path!()").count(),
        1,
        "identity is captured in exactly one place: {rendered}"
    );
    assert!(
        rendered.contains("Node::node_class_import_path()"),
        "the descriptor takes its identity from that one capture: {rendered}"
    );
}

/// A built-in's type is its class in `tatolab.stream`, written whole at expansion, so moving its
/// Rust module never changes it.
#[test]
fn a_built_in_nodes_identity_is_its_class_in_the_stream_package() {
    let rendered = a_node_of_its_gain_with(quote! { execution = reactive, built_in_node, });
    assert!(
        rendered.contains("NodeClassImportPath::of_built_in_node(\"ANode\""),
        "{rendered}"
    );
    assert!(!rendered.contains("module_path!()"), "{rendered}");
}

#[test]
fn the_expansion_never_reaches_for_type_name() {
    let rendered = a_node_of_its_gain_with(quote! { execution = reactive, });
    assert!(
        !rendered.contains("type_name"),
        "identity is never reflected at runtime: {rendered}"
    );
}

/// The marker keeps the author's spelling, so `OutputLink::videoOut` still names it, while its
/// `PORT_NAME` is the cast the descriptor registers.
#[test]
fn a_port_marker_is_named_by_its_spelling_and_carries_its_cast() {
    let rendered = expanded(
        quote! {
            execution = reactive,
            output("videoOut", pin_type = Texture, formats = ["rgba8_unorm"]),
        },
        quote! {
            pub fn a_node(outputs: OutputWriter) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
                move |_ctx| Ok(())
            }
        },
    );
    assert!(rendered.contains("pubmodOutputLink"), "{rendered}");
    assert!(rendered.contains("pubstructvideoOut"), "{rendered}");
    assert!(
        rendered.contains("OutputPortMarkerforvideoOut"),
        "{rendered}"
    );
    assert!(
        rendered.contains("constPORT_NAME:&'staticstr=\"videoout\""),
        "{rendered}"
    );
}

#[test]
fn a_crate_visible_node_generates_crate_visible_items_only() {
    let rendered = expanded(
        quote! { execution = reactive, input("gain", value = f32, default = 0.5), },
        quote! {
            pub(crate) fn a_node(gain: Input<f32>) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
                move |_ctx| Ok(())
            }
        },
    );
    for crate_visible_item in [
        "pub(crate)modANode",
        "pub(crate)fnnode(",
        "pub(crate)structNode",
        "pub(crate)fnnode_class_import_path(",
    ] {
        assert!(
            rendered.contains(crate_visible_item),
            "expected `{crate_visible_item}` in: {rendered}"
        );
    }
    assert!(
        !rendered.contains("pubmod")
            && !rendered.contains("pubstruct")
            && !rendered.contains("pubfn"),
        "a pub(crate) node generates no public item: {rendered}"
    );
}

#[test]
fn the_authors_visibility_is_written_as_seen_from_each_nested_module() {
    use std::num::NonZeroUsize;

    use super::author_visibility::author_visibility_written_from_a_nested_module;

    let written_from = |visibility: syn::Visibility, depth: usize| -> String {
        author_visibility_written_from_a_nested_module(
            &visibility,
            NonZeroUsize::new(depth).expect("every case names a nested module"),
        )
        .to_string()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
    };
    for (visibility, depth, expected) in [
        (syn::parse_quote! { pub }, 1, "pub"),
        (syn::parse_quote! { pub }, 2, "pub"),
        (syn::parse_quote! { pub(crate) }, 2, "pub(crate)"),
        (syn::Visibility::Inherited, 1, "pub(insuper)"),
        (syn::Visibility::Inherited, 2, "pub(insuper::super)"),
        (syn::parse_quote! { pub(self) }, 1, "pub(insuper)"),
        (syn::parse_quote! { pub(super) }, 1, "pub(insuper::super)"),
        (
            syn::parse_quote! { pub(super) },
            2,
            "pub(insuper::super::super)",
        ),
        (syn::parse_quote! { pub(crate) }, 1, "pub(crate)"),
        (
            syn::parse_quote! { pub(in super::super) },
            1,
            "pub(insuper::super::super)",
        ),
        (
            syn::parse_quote! { pub(in super::nodes) },
            2,
            "pub(insuper::super::super::nodes)",
        ),
        (
            syn::parse_quote! { pub(in super::nodes) },
            1,
            "pub(insuper::super::nodes)",
        ),
        (
            syn::parse_quote! { pub(in crate::nodes) },
            2,
            "pub(incrate::nodes)",
        ),
    ] {
        assert_eq!(written_from(visibility, depth), expected);
    }
}
