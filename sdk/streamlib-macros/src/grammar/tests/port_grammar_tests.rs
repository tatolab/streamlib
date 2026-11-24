// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use quote::quote;
use streamlib_node_schema::{
    ArrayOutputPortResidency, AudioWindowContractDeclaredValues, ChannelsOutputPortSampleRate,
    ChannelsOutputPortSampleType, ImageOutputPortFrameRate, NodeValueLifetime,
    OutputPortDeclaredNames, PortFamily,
};

use super::*;

#[test]
fn a_port_name_declared_twice_is_an_error() {
    let msg = parse_err(quote! {
        execution = manual,
        input("dup", pin_type = Texture, delivery_profile = "newest"),
        input("dup", pin_type = Texture, delivery_profile = "newest"),
    });
    assert!(
        msg.contains("the port name `dup` is declared more than once"),
        "got: {msg}"
    );
}

#[test]
fn an_input_and_an_output_sharing_a_name_is_an_error() {
    let msg = parse_err(quote! {
        execution = manual,
        input("video", pin_type = Texture, delivery_profile = "newest"),
        output("video", pin_type = Texture, formats = ["rgba8_unorm"]),
    });
    assert!(
        msg.contains("the port name `video` is declared more than once"),
        "got: {msg}"
    );
}

#[test]
fn two_ports_that_cast_alike_are_an_error_naming_both() {
    let msg = parse_err(quote! {
        execution = manual,
        output("videoOut", pin_type = Texture, formats = ["rgba8_unorm"]),
        output("video_out", pin_type = Texture, formats = ["rgba8_unorm"]),
        output("VideoOut", pin_type = Texture, formats = ["rgba8_unorm"]),
    });
    assert!(
        msg.contains("the ports `videoOut` and `VideoOut` both cast to `videoout`"),
        "got: {msg}"
    );
}

#[test]
fn a_port_is_declared_under_its_cast_and_keeps_its_spelling_for_the_marker() {
    let parsed = parse_ok(quote! {
        execution = manual,
        input("VidéoIn", pin_type = Texture, delivery_profile = "newest"),
        output("videoOut", pin_type = Texture, formats = ["rgba8_unorm"]),
    });
    assert_eq!(parsed.inputs[0].declared_marker_ident, "VidéoIn");
    assert_eq!(parsed.inputs[0].cast_name, "videoin");
    assert_eq!(parsed.outputs[0].declared_marker_ident, "videoOut");
    assert_eq!(parsed.outputs[0].cast_name, "videoout");

    let schema = parsed.to_node_schema();
    assert_eq!(schema.inputs[0].name, "videoin");
    assert_eq!(schema.outputs[0].name, "videoout");
}

#[test]
fn a_port_name_that_is_not_an_identifier_is_an_error() {
    for not_an_identifier in [
        "",
        "..",
        "video in",
        "video-out",
        "type",
        "r#type",
        " videoOut ",
        "videoOut /* c */",
    ] {
        let msg = parse_err(quote! {
            execution = manual,
            output(#not_an_identifier),
        });
        assert!(
            msg.contains(&format!(
                "port {not_an_identifier:?} is not a plain Rust identifier"
            )),
            "{not_an_identifier:?} got: {msg}"
        );
    }
}

/// A decomposed accent names the same identifier rustc's lexer composes it
/// into, so it is accepted as that identifier.
#[test]
fn a_decomposed_spelling_is_declared_as_its_composed_identifier() {
    let parsed = parse_ok(quote! {
        execution = manual,
        output("Vide\u{301}oOut", pin_type = Texture, formats = ["rgba8_unorm"]),
    });
    assert_eq!(parsed.outputs[0].declared_marker_ident, "Vid\u{e9}oOut");
    assert_eq!(parsed.outputs[0].cast_name, "videoout");
}

#[test]
fn a_port_name_casting_to_nothing_is_an_error() {
    let msg = parse_err(quote! {
        execution = manual,
        output("日本", pin_type = Texture, formats = ["rgba8_unorm"]),
    });
    assert!(msg.contains("cannot name anything"), "got: {msg}");
}

#[test]
fn output_delivery_profile_is_rejected() {
    // Regression: `delivery_profile` is a consumer-side setting on an
    // `output(...)`. It must be a spanned error, not silently nulled.
    // Mentally revert `reject_delivery_profile_on_output` and this parses
    // cleanly (bug) instead of erroring.
    let tokens: proc_macro2::TokenStream =
        "execution = manual, output(\"video\", delivery_profile = \"newest\")"
            .parse()
            .expect("token stream parses");
    let msg = parse_err(tokens);
    assert!(
        msg.contains("`delivery_profile` is a consumer-side setting"),
        "got: {msg}"
    );
}

#[test]
fn input_delivery_profile_is_accepted() {
    // The mirror of the rejection test: `delivery_profile` stays valid on
    // an `input(...)` and reaches the parsed port.
    let parsed = parse_ok(quote! {
        execution = manual,
        input("video_in", pin_type = Texture, delivery_profile = "ordered"),
    });
    assert_eq!(
        parsed.inputs[0].delivery_profile.as_deref(),
        Some("ordered")
    );
}

/// An input names how long a value set live on it lives, kept when it names none.
///
/// Mental-revert: drop the `lifetime` arm and the first declaration is refused as an unknown
/// port key; default the parsed port to reset and the second reads reset.
#[test]
fn an_input_declares_its_lifetime_and_is_kept_when_it_declares_none() {
    let parsed = parse_ok(quote! {
        execution = manual,
        input("trigger_level", pin_type = Float, delivery_profile = "newest", lifetime = reset),
        input("gain", pin_type = Float, delivery_profile = "newest"),
    });
    assert_eq!(parsed.inputs[0].lifetime, NodeValueLifetime::Reset);
    assert_eq!(parsed.inputs[1].lifetime, NodeValueLifetime::Kept);
    assert_eq!(
        parsed.to_node_schema().inputs[0].lifetime,
        NodeValueLifetime::Reset
    );
}

/// A lifetime is the input's and is one of the lifetimes there are.
#[test]
fn a_lifetime_on_an_output_or_naming_no_lifetime_is_an_error() {
    let on_an_output = parse_err(quote! {
        execution = manual,
        output("level", pin_type = Float, lifetime = reset),
    });
    assert!(
        on_an_output.contains("`lifetime` is declared by an input"),
        "{on_an_output}"
    );
    let no_such_lifetime = parse_err(quote! {
        execution = manual,
        input("gain", pin_type = Float, delivery_profile = "newest", lifetime = forever),
    });
    assert!(
        no_such_lifetime.contains("a lifetime is one of `kept`, `reset`"),
        "{no_such_lifetime}"
    );
}

/// A renamed port lists the names it had, each cast, and an input states it may be unset; a
/// picture port that states either carries its Texture so the declaration reaches the
/// descriptor.
///
/// Mental-revert: drop the `former_names` arm and the declaration is refused as an unknown port
/// key; keep a picture port's pin type `None` and its former name never reaches the schema's
/// pin.
#[test]
fn a_port_states_the_names_it_had_and_an_input_that_it_may_be_unset() {
    let parsed = parse_ok(quote! {
        execution = manual,
        input("gain", pin_type = Float, former_names = ["Level", "amount"], may_be_unset = true),
        input("frames", pin_type = Texture, delivery_profile = "newest", former_names = ["video"]),
        output("out", pin_type = Float),
    });
    assert_eq!(parsed.inputs[0].former_names, ["level", "amount"]);
    assert!(parsed.inputs[0].may_be_unset);
    assert!(parsed.inputs[1].pin_type.is_some());
    assert!(!parsed.inputs[1].may_be_unset);
    assert!(parsed.outputs[0].former_names.is_empty());
    let schema = parsed.to_node_schema();
    assert_eq!(schema.inputs[0].former_names, ["level", "amount"]);
    assert!(schema.inputs[0].may_be_unset);
}

/// `may_be_unset` is an input's; a port does not list
/// its own name, or one name twice, as a name it had.
#[test]
fn a_former_name_or_may_be_unset_that_cannot_hold_is_an_error() {
    let on_an_output = parse_err(quote! {
        execution = manual,
        output("level", pin_type = Float, may_be_unset = true),
    });
    assert!(
        on_an_output.contains("`may_be_unset` is declared by an input"),
        "{on_an_output}"
    );
    let its_own_name = parse_err(quote! {
        execution = manual,
        input("gain", pin_type = Float, former_names = ["Gain"]),
    });
    assert!(
        its_own_name.contains("lists its own name `Gain`"),
        "{its_own_name}"
    );
    let twice = parse_err(quote! {
        execution = manual,
        input("gain", pin_type = Float, former_names = ["level", "Level"]),
    });
    assert!(
        twice.contains("lists the name `Level` it had twice"),
        "{twice}"
    );
}

#[test]
fn input_without_a_delivery_profile_is_an_error() {
    let msg = parse_err(quote! {
        execution = manual,
        input("video_in", pin_type = from_setting("bundle_type"), flow = Stream),
    });
    assert!(
        msg.contains("input port `video_in` must declare a `delivery_profile`"),
        "got: {msg}"
    );
    assert!(
        msg.contains("newest") && msg.contains("ordered"),
        "the error must list the valid profiles: {msg}"
    );
}

#[test]
fn output_without_a_delivery_profile_stays_valid() {
    // The requirement is consumer-side only: an `output(...)` declaring no
    // profile is the correct shape, not a missing declaration.
    let parsed = parse_ok(quote! {
        execution = manual,
        output("video", pin_type = Texture, formats = ["rgba8_unorm"]),
    });
    assert_eq!(parsed.outputs[0].delivery_profile, None);
}

// ---- `audio_window`: the declaration, and every way it is refused ----
/// The unknown-port-key list is the author's map of the port grammar, so a
/// key the grammar accepts and the list omits is a key nobody finds.
#[test]
fn the_unknown_port_key_error_offers_every_key_a_port_accepts() {
    let msg = parse_err(quote! {
        execution = reactive,
        input("audio", pin_type = Audio, delivery_profile = "ordered", frobnicate = "yes"),
    });

    assert!(msg.contains("unknown port key `frobnicate`"), "got: {msg}");
    for key in ["delivery_profile", "description", "audio_window"] {
        assert!(msg.contains(key), "the list must offer `{key}`; got: {msg}");
    }
}

fn declared_contract(parsed: &ParsedNodeAttr) -> &AudioWindowContractDeclaredValues {
    match parsed.inputs[0]
        .audio_window
        .as_ref()
        .expect("the port declared a contract")
    {
        AudioWindowContract::Declaration(values) => values,
        other => panic!("expected declared values, got {other:?}"),
    }
}

#[test]
fn an_audio_window_declares_its_five_values() {
    let parsed = parse_ok(quote! {
        execution = reactive,
        input(
            "audio",
            pin_type = Audio,
            delivery_profile = "ordered",
            audio_window(
                sample_rate = 16_000,
                channels = 1,
                dtype = "f32",
                window_size = 512,
                hop = 512
            )
        ),
    });

    assert_eq!(
        declared_contract(&parsed),
        &AudioWindowContractDeclaredValues {
            sample_rate: 16_000,
            channels: Some(1),
            dtype: "f32".to_string(),
            window_size: 512,
            hop: 512,
        }
    );
}

/// `hop` is the one omittable field: leaving it out is how an author asks
/// for contiguous, non-overlapping windows.
#[test]
fn an_omitted_hop_defaults_to_the_window_size() {
    let parsed = parse_ok(quote! {
        execution = reactive,
        input(
            "audio",
            pin_type = Audio,
            delivery_profile = "ordered",
            audio_window(sample_rate = 16_000, channels = 1, dtype = "f32", window_size = 400)
        ),
    });

    assert_eq!(declared_contract(&parsed).hop, 400);
}

#[test]
fn the_sentinel_is_a_whole_contract() {
    let parsed = parse_ok(quote! {
        execution = manual,
        input("audio", pin_type = Audio, delivery_profile = "ordered", audio_window = match_device),
    });

    assert_eq!(
        parsed.inputs[0].audio_window,
        Some(AudioWindowContract::MatchDevice {})
    );
}

#[test]
fn a_port_declaring_no_contract_carries_none() {
    let parsed = parse_ok(quote! {
        execution = reactive,
        input("video_in", pin_type = Texture, delivery_profile = "newest"),
        output("video_out", pin_type = Texture, formats = ["rgba8_unorm"]),
    });

    assert_eq!(parsed.inputs[0].audio_window, None);
    assert_eq!(parsed.outputs[0].audio_window, None);
}

#[test]
fn a_partial_contract_is_refused_naming_the_missing_fields() {
    let msg = parse_err(quote! {
        execution = reactive,
        input(
            "audio",
            pin_type = Audio,
            delivery_profile = "ordered",
            audio_window(sample_rate = 16_000, window_size = 512)
        ),
    });

    assert!(
        msg.contains("partial") && msg.contains("`dtype`"),
        "the refusal must name the missing fields; got {msg}"
    );
}

#[test]
fn a_hop_above_the_window_size_is_refused_naming_both_numbers() {
    let msg = parse_err(quote! {
        execution = reactive,
        input(
            "audio",
            pin_type = Audio,
            delivery_profile = "ordered",
            audio_window(
                sample_rate = 16_000,
                channels = 1,
                dtype = "f32",
                window_size = 512,
                hop = 1024
            )
        ),
    });

    assert!(
        msg.contains("1024") && msg.contains("512"),
        "the refusal must name both numbers; got {msg}"
    );
}

#[test]
fn a_contract_beside_a_skipping_delivery_profile_is_refused_naming_both_knobs() {
    let msg = parse_err(quote! {
        execution = reactive,
        input(
            "audio",
            pin_type = Audio,
            delivery_profile = "newest",
            audio_window(sample_rate = 16_000, channels = 1, dtype = "f32", window_size = 512)
        ),
    });

    assert!(
        msg.contains("audio_window") && msg.contains("newest") && msg.contains("ordered"),
        "the refusal must name both knobs; got {msg}"
    );
}

/// The profile refusal is checked after the whole port body, so it cannot
/// depend on which key the author happened to write first.
#[test]
fn a_contract_declared_before_its_profile_is_refused_the_same() {
    let msg = parse_err(quote! {
        execution = reactive,
        input(
            "audio",
            pin_type = Audio,
            audio_window(sample_rate = 16_000, channels = 1, dtype = "f32", window_size = 512),
            delivery_profile = "newest"
        ),
    });

    assert!(
        msg.contains("audio_window") && msg.contains("ordered"),
        "key order must not change the refusal; got {msg}"
    );
}

/// The sentinel is a contract too, so it takes the same profile refusal.
#[test]
fn the_sentinel_beside_a_skipping_delivery_profile_is_refused_too() {
    let msg = parse_err(quote! {
        execution = manual,
        input("audio", pin_type = Audio, delivery_profile = "newest", audio_window = match_device),
    });

    assert!(
        msg.contains("ordered"),
        "the refusal must name the profile a contract requires; got {msg}"
    );
}

#[test]
fn a_zero_numeric_field_is_refused_naming_the_field_and_the_value() {
    let msg = parse_err(quote! {
        execution = reactive,
        input(
            "audio",
            pin_type = Audio,
            delivery_profile = "ordered",
            audio_window(sample_rate = 16_000, channels = 1, dtype = "f32", window_size = 0)
        ),
    });

    assert!(
        msg.contains("window_size") && msg.contains("is 0"),
        "the refusal must name the field and the value; got {msg}"
    );
}

/// The fields are unsigned, so a negative literal would otherwise reach the
/// author as syn's bare `expected integer literal`.
#[test]
fn a_negative_numeric_field_is_refused_by_name() {
    let msg = parse_err(quote! {
        execution = reactive,
        input(
            "audio",
            pin_type = Audio,
            delivery_profile = "ordered",
            audio_window(sample_rate = -1, channels = 1, dtype = "f32", window_size = 512)
        ),
    });

    assert!(
        msg.contains("negative") && msg.contains("-1"),
        "the refusal must name the value; got {msg}"
    );
}

#[test]
fn an_unknown_dtype_is_refused_listing_the_legal_values() {
    let msg = parse_err(quote! {
        execution = reactive,
        input(
            "audio",
            pin_type = Audio,
            delivery_profile = "ordered",
            audio_window(sample_rate = 16_000, channels = 1, dtype = "i16", window_size = 512)
        ),
    });

    assert!(
        msg.contains("i16") && msg.contains("f32") && msg.contains("f64"),
        "the refusal must name the value and the legal ones; got {msg}"
    );
}

#[test]
fn an_unknown_audio_window_field_is_refused_listing_the_five() {
    let msg = parse_err(quote! {
        execution = reactive,
        input(
            "audio",
            pin_type = Audio,
            delivery_profile = "ordered",
            audio_window(
                sample_rate = 16_000,
                channels = 1,
                dtype = "f32",
                window_size = 512,
                overlap = 128
            )
        ),
    });

    assert!(
        msg.contains("`overlap`") && msg.contains("window_size"),
        "the refusal must name the stray field and the legal ones; got {msg}"
    );
}

#[test]
fn an_unknown_sentinel_is_refused_naming_the_only_one() {
    let msg = parse_err(quote! {
        execution = manual,
        input("audio", pin_type = Audio, delivery_profile = "ordered", audio_window = match_whatever),
    });

    assert!(
        msg.contains("match_whatever") && msg.contains("match_device"),
        "the refusal must name what was written and the only sentinel; got {msg}"
    );
}

#[test]
fn an_audio_window_on_an_output_is_refused() {
    let msg = parse_err(quote! {
        execution = manual,
        output("audio_out", pin_type = Audio, audio_window = match_device),
    });

    assert!(
        msg.contains("consumer-side") && msg.contains("audio_out"),
        "the refusal must name the port and say whose setting it is; got {msg}"
    );
}

#[test]
fn a_ports_family_and_its_keys_reach_the_schema() {
    let parsed = parse_ok(quote! {
        execution = manual,
        input("signals", delivery_profile = "ordered", pin_type = Audio, requires = ["rpm"]),
        input("video", delivery_profile = "newest", pin_type = Texture, formats = ["rgba8_unorm"]),
        output(
            "telemetry",
            pin_type = Audio,
            names = from_setting("channel_names"),
            rate = 48000,
            dtype = "f32"
        ),
        output("tensor", pin_type = Tensor, names = ["boxes"], residency = "gpu"),
    });
    let schema = parsed.to_node_schema();

    let signals = &schema.inputs[0].family;
    assert_eq!(signals.family, PortFamily::Channels);
    assert_eq!(signals.requires, vec!["rpm".to_string()]);
    assert_eq!(
        schema.inputs[1].family.formats,
        vec!["rgba8_unorm".to_string()]
    );
    let telemetry = &schema.outputs[0].family;
    assert_eq!(
        telemetry.names,
        Some(OutputPortDeclaredNames::FromConfig {
            config_key: "channel_names".into()
        })
    );
    assert_eq!(
        telemetry.rate,
        Some(ChannelsOutputPortSampleRate::Stated(48_000))
    );
    assert_eq!(telemetry.dtype, Some(ChannelsOutputPortSampleType::F32));
    assert_eq!(
        schema.outputs[1].family.residency,
        Some(ArrayOutputPortResidency::Gpu)
    );
}

#[test]
fn a_family_key_the_family_does_not_take_is_refused_naming_the_port() {
    let msg = parse_err(quote! {
        execution = manual,
        output("video", pin_type = Texture, formats = ["rgba8_unorm"], names = ["x"]),
    });
    assert!(
        msg.contains("\"video\"") && msg.contains("`names=`"),
        "got {msg}"
    );
}

#[test]
fn a_family_key_its_pin_types_family_does_not_take_is_refused() {
    let msg = parse_err(quote! {
        execution = manual,
        output("level", pin_type = Float, formats = ["rgba8_unorm"]),
    });
    assert!(
        msg.contains("states `formats=`, which a Channels output does not take"),
        "got {msg}"
    );
}

#[test]
fn an_image_outputs_frame_rate_reaches_the_schema_stated_or_from_config() {
    let parsed = parse_ok(quote! {
        execution = manual,
        output("video", pin_type = Texture, formats = ["rgba8_unorm"], frame_rate = 30),
        output("preview", pin_type = Texture, formats = ["rgba8_unorm"], frame_rate = from_setting("frames_per_second")),
        output("bare", pin_type = Texture, formats = ["rgba8_unorm"]),
    });
    let schema = parsed.to_node_schema();

    assert_eq!(
        schema.outputs[0].family.frame_rate,
        Some(ImageOutputPortFrameRate::Stated(
            streamlib_node_schema::ImageStreamFrameRate {
                numerator: 30,
                denominator: 1
            }
        ))
    );
    assert_eq!(
        schema.outputs[1].family.frame_rate,
        Some(ImageOutputPortFrameRate::FromConfig {
            config_key: "frames_per_second".into()
        })
    );
    assert_eq!(schema.outputs[2].family.frame_rate, None);
}

#[test]
fn a_catalog_key_is_refused_as_unknown() {
    let msg = parse_err(quote! {
        execution = manual,
        output("text", pin_type = Texture, formats = ["rgba8_unorm"], catalog = TextEntry { format: "vtt" }),
    });

    assert!(msg.contains("unknown port key `catalog`"), "got {msg}");
}

#[test]
fn a_second_audio_window_on_one_port_is_refused() {
    let msg = parse_err(quote! {
        execution = manual,
        input(
            "audio",
            pin_type = Audio,
            delivery_profile = "ordered",
            audio_window = match_device,
            audio_window(sample_rate = 16_000, channels = 1, dtype = "f32", window_size = 512)
        ),
    });

    assert!(
        msg.contains("more than once"),
        "a second contract is refused; got {msg}"
    );
}

#[test]
fn the_projected_schema_carries_the_contract_the_port_declared() {
    let parsed = parse_ok(quote! {
        execution = reactive,
        input(
            "audio",
            pin_type = Audio,
            delivery_profile = "ordered",
            audio_window(
                sample_rate = 16_000,
                channels = 1,
                dtype = "f32",
                window_size = 512,
                hop = 160
            )
        ),
    });

    let schema = parsed.to_node_schema();
    assert_eq!(
        serde_json::to_value(&schema.inputs[0].audio_window).unwrap(),
        serde_json::json!({
            "resolved_from": "declaration",
            "sample_rate": 16_000,
            "channels": 1,
            "dtype": "f32",
            "window_size": 512,
            "hop": 160,
        })
    );
}

/// The count is the one value a port may leave to whatever is upstream of
/// it, so a graph can grow a source without every consumer being edited.
#[test]
fn a_contract_omitting_its_channel_count_parses_as_following_the_source() {
    let parsed = parse_ok(quote! {
        execution = reactive,
        input(
            "audio",
            pin_type = Audio,
            delivery_profile = "ordered",
            audio_window(
                sample_rate = 48_000,
                dtype = "f32",
                window_size = 960,
                hop = 960
            )
        ),
    });

    assert_eq!(
        declared_contract(&parsed),
        &AudioWindowContractDeclaredValues {
            sample_rate: 48_000,
            channels: None,
            dtype: "f32".to_string(),
            window_size: 960,
            hop: 960,
        }
    );
}

/// Relaxing the count relaxed nothing else: the refusal still names each
/// value the stage cannot run without.
#[test]
fn a_contract_missing_a_value_other_than_the_count_is_still_refused_by_name() {
    for (declaration, expected) in [
        (
            quote! { audio_window(dtype = "f32", window_size = 512) },
            "`sample_rate`",
        ),
        (
            quote! { audio_window(sample_rate = 16_000, window_size = 512) },
            "`dtype`",
        ),
        (
            quote! { audio_window(sample_rate = 16_000, dtype = "f32") },
            "`window_size`",
        ),
    ] {
        let msg = parse_err(quote! {
            execution = reactive,
            input("audio", pin_type = Audio, delivery_profile = "ordered", #declaration),
        });

        assert!(
            msg.contains("partial") && msg.contains(expected),
            "the refusal must name {expected}; got {msg}"
        );
    }
}

#[test]
fn a_port_following_the_source_projects_its_count_as_the_source() {
    let parsed = parse_ok(quote! {
        execution = reactive,
        input(
            "audio",
            pin_type = Audio,
            delivery_profile = "ordered",
            audio_window(sample_rate = 48_000, dtype = "f32", window_size = 960)
        ),
    });

    let schema = parsed.to_node_schema();
    assert_eq!(
        serde_json::to_value(&schema.inputs[0].audio_window).unwrap(),
        serde_json::json!({
            "resolved_from": "declaration",
            "sample_rate": 48_000,
            "channels": "source",
            "dtype": "f32",
            "window_size": 960,
            "hop": 960,
        })
    );
}

/// A contract-less port must project exactly what it always did — the
/// field is absent from the rendering, not present as a null.
#[test]
fn a_contract_less_ports_projection_carries_no_audio_window_key() {
    let parsed = parse_ok(quote! {
        execution = reactive,
        input("video_in", pin_type = Texture, delivery_profile = "newest"),
    });

    let schema = parsed.to_node_schema();
    let json = serde_json::to_value(&schema.inputs[0]).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "name": "video_in",
            "description": null,
            "delivery_profile": "newest",
            "family": {"family": "image"},
            "pin_type": {"type": "texture"},
        })
    );
}

#[test]
fn a_positional_port_schema_is_an_error() {
    // A port declares a name and keyed options only. The old
    // `input("name", pin_type = Texture, <schema>, ...)` form must fail to parse, not be
    // silently accepted and dropped.
    let msg = parse_err(quote! {
        execution = manual,
        input("in1", pin_type = Texture, "@tatolab/core/VideoFrame", delivery_profile = "newest"),
    });
    assert!(
        msg.contains("a port declares no type") && msg.contains("input(\"in1\", ...)"),
        "the error must name the removal and the offending port: {msg}"
    );
}

#[test]
fn a_bare_any_port_schema_is_an_error() {
    let msg = parse_err(quote! {
        execution = manual,
        output("out1", pin_type = Texture, formats = ["rgba8_unorm"], any),
    });
    assert!(
        msg.contains("a port declares no type") && msg.contains("output(\"out1\", ...)"),
        "the error must name the removal and the offending port: {msg}"
    );
}
