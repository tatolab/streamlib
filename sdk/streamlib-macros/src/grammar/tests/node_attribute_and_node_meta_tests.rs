// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use quote::quote;
use streamlib_node_schema::{NodeSchemaExecution, OutputPortDeclaredNames};

use super::*;

#[test]
fn full_execution_and_ports() {
    let parsed = parse_ok(quote! {
        execution = manual,
        scheduling = high,
        input("video_in", family = Image, delivery_profile = "newest"),
        output("video", family = Image),
    });
    assert_eq!(parsed.execution, NodeSchemaExecution::Manual);
    assert_eq!(parsed.scheduling, Some(ThreadPriority::High));
    assert_eq!(parsed.inputs.len(), 1);
    assert_eq!(parsed.inputs[0].cast_name, "video_in");
    assert_eq!(parsed.inputs[0].delivery_profile.as_deref(), Some("newest"));
    assert_eq!(parsed.outputs.len(), 1);
    assert_eq!(parsed.outputs[0].cast_name, "video");
    // Output ports never carry a delivery profile.
    assert_eq!(parsed.outputs[0].delivery_profile, None);
}

#[test]
fn node_and_port_descriptions_parse() {
    let parsed = parse_ok_with_meta(
        quote! { #[meta(description = "Captures video from cameras")] },
        quote! {
            execution = manual,
            input(
                "video_in",
                family = Image,
                delivery_profile = "newest",
                description = "Frames to convert"
            ),
            output("video", family = Image, description = "Live video frames"),
        },
    );
    let literal_text_of = |description: &Option<DeclaredDescription>| {
        description
            .as_ref()
            .and_then(DeclaredDescription::literal_text)
    };
    assert_eq!(
        literal_text_of(&parsed.description).as_deref(),
        Some("Captures video from cameras")
    );
    assert_eq!(
        literal_text_of(&parsed.inputs[0].description).as_deref(),
        Some("Frames to convert")
    );
    assert_eq!(
        literal_text_of(&parsed.outputs[0].description).as_deref(),
        Some("Live video frames")
    );
}

#[test]
fn a_description_may_name_a_const_on_the_node_and_on_a_port() {
    let parsed = parse_ok_with_meta(
        quote! { #[meta(description = crate::descriptions::CAMERA_NODE_DESCRIPTION)] },
        quote! {
            execution = manual,
            output("video", family = Image, description = VIDEO_PORT_DESCRIPTION),
        },
    );
    assert!(matches!(
        &parsed.description,
        Some(DeclaredDescription::Constant(path))
            if quote!(#path).to_string() == "crate :: descriptions :: CAMERA_NODE_DESCRIPTION"
    ));
    assert!(matches!(
        &parsed.outputs[0].description,
        Some(DeclaredDescription::Constant(path)) if path.is_ident("VIDEO_PORT_DESCRIPTION")
    ));
    assert_eq!(parsed.to_node_schema().description, None);
}

#[test]
fn a_description_that_is_neither_a_literal_nor_a_const_path_is_refused() {
    let msg = node_meta_err(quote! { #[meta(description = include_str!("README.md"))] });
    assert!(
        msg.contains("string literal or the path of a `const`"),
        "got: {msg}"
    );
}

#[test]
fn a_node_meta_declares_a_category_and_a_description_in_one_attribute() {
    let parsed = parse_ok_with_meta(
        quote! { #[meta(category = "Sources/Cameras", description = "Captures a camera.")] },
        quote! { execution = manual },
    );
    assert_eq!(parsed.category.as_deref(), Some("Sources/Cameras"));
    assert_eq!(
        parsed
            .description
            .as_ref()
            .and_then(DeclaredDescription::literal_text)
            .as_deref(),
        Some("Captures a camera.")
    );
}

#[test]
fn a_node_with_no_meta_has_no_category_and_no_description() {
    let parsed = parse_ok(quote! { execution = manual });
    assert_eq!(parsed.category, None);
    assert!(parsed.description.is_none());
    assert!(!parsed.conversion);
}

#[test]
fn a_node_meta_states_a_conversion_with_a_bool() {
    let parsed = parse_ok_with_meta(
        quote! { #[meta(conversion = true)] },
        quote! { execution = manual },
    );
    assert!(parsed.conversion);
    let stated_false = parse_ok_with_meta(
        quote! { #[meta(conversion = false)] },
        quote! { execution = manual },
    );
    assert!(!stated_false.conversion);
}

#[test]
fn a_conversion_that_is_not_a_bool_is_refused() {
    let msg = node_meta_err(quote! { #[meta(conversion = "yes")] });
    assert!(
        msg.contains("`conversion` takes `true` or `false`"),
        "got: {msg}"
    );
}

#[test]
fn a_node_meta_may_be_written_as_one_attribute_per_key() {
    let declared = node_meta_ok(quote! {
        #[meta(category = "Effects")]
        #[meta(description = "Tints a frame.")]
    });
    assert_eq!(declared.category.as_deref(), Some("Effects"));
    assert!(declared.description.is_some());
}

#[test]
fn reading_a_node_meta_takes_it_off_the_struct_and_leaves_every_other_attribute() {
    let mut node_struct = node_struct_with(quote! {
        #[derive(Debug)]
        #[meta(category = "Effects")]
        #[allow(dead_code)]
    });
    DeclaredNodeMeta::take_from_attributes(&mut node_struct.attrs)
        .expect("the node's meta should read");
    let left_on_the_struct: Vec<String> = node_struct
        .attrs
        .iter()
        .map(|attribute| {
            attribute
                .path()
                .get_ident()
                .map(Ident::to_string)
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(left_on_the_struct, ["derive", "allow"]);
}

#[test]
fn a_node_meta_key_declared_twice_is_refused_naming_the_key() {
    let msg = node_meta_err(quote! {
        #[meta(category = "Effects")]
        #[meta(category = "Sources")]
    });
    assert!(
        msg.contains("`category` is declared more than once"),
        "got: {msg}"
    );
}

#[test]
fn every_node_meta_that_is_refused_is_reported_and_none_is_left_on_the_struct() {
    let mut node_struct = node_struct_with(quote! {
        #[meta(tags = "video")]
        #[allow(dead_code)]
        #[meta(category = "")]
    });
    let refusals: Vec<String> = DeclaredNodeMeta::take_from_attributes(&mut node_struct.attrs)
        .err()
        .expect("both metas are refused")
        .into_iter()
        .map(|refusal| refusal.to_string())
        .collect();
    assert_eq!(refusals.len(), 2, "got: {refusals:?}");
    assert_eq!(node_struct.attrs.len(), 1);
}

#[test]
fn a_node_meta_that_declares_nothing_is_refused() {
    let msg = node_meta_err(quote! { #[meta()] });
    assert!(msg.contains("declares nothing"), "got: {msg}");
}

#[test]
fn an_unknown_node_meta_key_is_refused_naming_it_and_the_keys_there_are() {
    let msg = node_meta_err(quote! { #[meta(tags = "video")] });
    assert!(
        msg.contains("unknown `#[meta(...)]` key `tags`"),
        "got: {msg}"
    );
    assert!(
        msg.contains("`category`, `description` or `conversion`"),
        "got: {msg}"
    );
}

#[test]
fn a_category_with_a_level_that_has_no_name_is_refused() {
    for category in [
        "",
        "  ",
        "/Effects",
        "Effects/",
        "Effects//Stylize",
        "Effects/ /Blur",
    ] {
        let msg = node_meta_err(quote! { #[meta(category = #category)] });
        assert!(
            msg.contains("names a group with no name"),
            "category {category:?} got: {msg}"
        );
    }
}

#[test]
fn a_category_that_is_not_a_string_literal_is_refused() {
    let msg = node_meta_err(quote! { #[meta(category = EFFECTS_CATEGORY)] });
    assert!(
        msg.contains("`category` takes a string literal"),
        "got: {msg}"
    );
}

#[test]
fn a_meta_key_written_in_the_node_attribute_is_refused_naming_where_it_goes() {
    for meta_key in ["description", "category"] {
        let key = ident(meta_key);
        let msg = parse_err(quote! { #key = "Captures a camera.", execution = manual });
        assert!(
            msg.contains(&format!("#[meta({meta_key} = \"…\")]")),
            "key {meta_key} got: {msg}"
        );
    }
}

#[test]
fn continuous_with_interval() {
    let parsed = parse_ok(quote! {
        execution = continuous(interval_ms = 10),
    });
    assert_eq!(
        parsed.execution,
        NodeSchemaExecution::Continuous { interval_ms: 10 }
    );
}

#[test]
fn continuous_without_interval_defaults_to_zero() {
    let parsed = parse_ok(quote! {
        execution = continuous,
    });
    assert_eq!(
        parsed.execution,
        NodeSchemaExecution::Continuous { interval_ms: 0 }
    );
}

#[test]
fn a_config_key_binds_the_type_and_the_default_field_name() {
    let parsed = parse_ok(quote! {
        execution = manual,
        config = crate::camera_config::CameraConfig,
    });
    assert!(parsed.config_type.is_some());
    assert_eq!(parsed.config_field_name, "config");
}

#[test]
fn a_node_declaring_no_config_binds_no_config_type() {
    let parsed = parse_ok(quote! {
        execution = manual,
    });
    assert!(parsed.config_type.is_none());
}

#[test]
fn a_settings_key_binds_the_settings_type_and_no_config() {
    let parsed = parse_ok(quote! {
        execution = reactive,
        settings = crate::blur::BlurSettings,
        output("blurred", pin_type = Audio, names = from_setting("channel_names")),
    });
    assert!(parsed.settings_type.is_some());
    assert!(parsed.config_type.is_none());
    assert_eq!(
        parsed.outputs[0].family.names,
        Some(OutputPortDeclaredNames::FromConfig {
            config_key: "channel_names".to_string()
        })
    );
}

#[test]
fn settings_beside_a_config_are_refused() {
    for beside in [
        quote! { config = crate::BlurConfig },
        quote! { config_field = "blur" },
    ] {
        let msg = parse_err(quote! {
            execution = reactive,
            settings = crate::blur::BlurSettings,
            #beside,
        });
        assert!(
            msg.contains("declares no `config` or `config_field`"),
            "{beside}: {msg}"
        );
    }
}

#[test]
fn the_retired_config_schema_key_is_refused_by_name_with_the_keys_that_remain() {
    let msg = parse_err(quote! {
        execution = reactive,
        config = crate::BufferRechunkerConfig,
        config_schema = "com.tatolab.buffer_rechunker.config@1.0.0",
    });
    assert!(
        msg.contains("`config_schema`") && msg.contains("`config`"),
        "the error must name the retired key and list the ones that remain: {msg}"
    );
}

#[test]
fn the_class_short_name_is_the_authored_struct_ident() {
    // The display-name default's only carrier. It is read off the item the
    // attribute is attached to — never authored, and never recovered by
    // splitting the import path.
    let parsed = parse2(
        quote! { execution = reactive },
        DeclaredNodeMeta::default(),
        &ident("MyLocalNode"),
    )
    .expect("a bare node should parse");
    assert_eq!(parsed.node_class_short_name, "MyLocalNode");
    assert_eq!(parsed.to_node_schema().name, "MyLocalNode");
}

#[test]
fn unsafe_send_flag() {
    let parsed = parse_ok(quote! {
        execution = manual,
        unsafe_send,
    });
    assert!(parsed.unsafe_send);
}

#[test]
fn paced_by_real_time_flag_reaches_the_schema() {
    let parsed = parse_ok(quote! {
        execution = manual,
        paced_by_real_time,
    });
    assert!(parsed.paced_by_real_time);
    assert!(parsed.to_node_schema().paced_by_real_time);
    assert!(!parse_ok(quote! { execution = manual }).paced_by_real_time);
}

#[test]
fn built_in_node_flag() {
    assert!(
        parse_ok(quote! {
            execution = manual,
            built_in_node,
        })
        .built_in_node
    );
    assert!(!parse_ok(quote! { execution = manual }).built_in_node);
}

// ---- error cases ----

#[test]
fn missing_execution_is_an_error() {
    let msg = parse_err(quote! { scheduling = high });
    assert!(msg.contains("missing required `execution`"), "got: {msg}");
}

#[test]
fn unknown_key_is_an_error() {
    let msg = parse_err(quote! {
        execution = manual,
        frobnicate = "yes",
    });
    assert!(
        msg.contains("unknown `#[node(...)]` key `frobnicate`"),
        "got: {msg}"
    );
}

#[test]
fn unknown_execution_mode_is_an_error() {
    let msg = parse_err(quote! {
        execution = sideways,
    });
    assert!(
        msg.contains("unknown execution mode `sideways`"),
        "got: {msg}"
    );
}

#[test]
fn a_positional_identity_is_refused_naming_the_class_path_rule() {
    // Every spelling the deleted grammar accepted lands on one refusal —
    // including the ones it used to reject for its own reasons, which must
    // not leak a message about a grammar that no longer exists. Mental-
    // revert guard: restore the positional `LitStr` parse and these parse
    // clean instead of erroring.
    for identity in [
        "\"@tatolab/camera/Camera\", execution = manual",
        "\"@tatolab/camera/Camera@1.0.0\", execution = manual",
        "\"tatolab/camera/Camera\", execution = manual",
        "\"@tatolab/camera\", execution = manual",
    ] {
        let tokens: proc_macro2::TokenStream = identity.parse().expect("token stream parses");
        let msg = parse_err(tokens);
        assert!(
            msg.contains("declares no identity") && msg.contains("import path"),
            "got: {msg}"
        );
    }
}

#[test]
fn a_type_override_is_refused_naming_the_class_path_rule() {
    let msg = parse_err(quote! { execution = reactive, type = "CustomName" });
    assert!(msg.contains("declares no identity"), "got: {msg}");
}

#[test]
fn type_is_not_offered_as_a_valid_key() {
    // The unknown-key list is the author's map of the grammar; leaving
    // `type` on it would point them at a key that now errors.
    let msg = parse_err(quote! { execution = reactive, bogus = "x" });
    assert!(msg.contains("unknown `#[node(...)]` key"), "got: {msg}");
    assert!(!msg.contains("`type`"), "got: {msg}");
}

#[test]
fn continuous_unknown_key_is_an_error() {
    let msg = parse_err(quote! {
        execution = continuous(period = 5),
    });
    assert!(msg.contains("expected `interval_ms`"), "got: {msg}");
}

#[test]
fn quote_placeholder_keeps_quote_in_scope() {
    // Guards the test-only `quote` import stays wired.
    let _ = quote! {};
}
