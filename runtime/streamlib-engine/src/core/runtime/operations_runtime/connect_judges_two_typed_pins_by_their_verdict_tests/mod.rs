// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `connect` between two typed pins makes the link the verdict connects, as it is or through a
//! free conversion, and refuses one that needs a convert node or does not connect, in the
//! verdict's words, naming the conversions the registered node types state.

use std::sync::Arc;

use serde_json::Value;

use super::connect_on_a_test_thread::{
    connect_on_this_thread, connect_on_this_thread_as, the_links_in,
};
use super::{ALinkConnected, the_verdict_connect_takes_on_a_link};
use crate::core::compiler::Compiler;
use crate::core::descriptors::{
    FreePinConversion, NamedPinTypeImportPath, NodeClassImportPath, NodeClassShortName,
    NodeDescriptor, NodeInputMode, NodeKind, OutputPortDeclaredNames, PinDeclaration, PinFlow,
    PinLinkOutcome, PinLinkRefusal, PinType, PortDescriptor, PortFamily, PortFamilyDeclaration,
    SettingReadTime, TexturePixelFormat,
};
use crate::core::graph::{
    InputLinkPortRef, NodeRunId, OutputLinkPortRef,
    refuse_a_config_update_that_a_wired_link_out_of_the_node_would_refuse,
};
use crate::core::nodes::{NODE_REGISTRY, NodeSpec};
use crate::core::{Error, Result};

mod bundle_frame_link_tests;
mod bundle_member_link_tests;
mod convert_node_held_settings_tests;
mod named_pin_type_tests;
mod node_types_port_tests;
mod placed_level_tests;
mod texture_formats_tests;

const FLOAT_SOURCE: &str = "VerdictCheckFloatSource";
const FLOAT_STREAM_SOURCE: &str = "VerdictCheckFloatStreamSource";
const FLOAT_SHADER_SOURCE: &str = "VerdictCheckFloatShaderSource";
const NUMBER_SINK: &str = "VerdictCheckNumberSink";
const CELSIUS_SOURCE: &str = "VerdictCheckCelsiusSource";
const FAHRENHEIT_SINK: &str = "VerdictCheckFahrenheitSink";
const KELVIN_SOURCE: &str = "VerdictCheckKelvinSource";
const RANKINE_SINK: &str = "VerdictCheckRankineSink";
const KELVIN_SINK: &str = "VerdictCheckKelvinSink";
const CELSIUS_TO_FAHRENHEIT: &str = "VerdictCheckCelsiusToFahrenheit";
const KELVIN_TO_RANKINE: &str = "VerdictCheckKelvinToRankine";
const DESCRIBED_ONLY_FAHRENHEIT_TO_CELSIUS: &str = "VerdictCheckDescribedOnlyFahrenheitToCelsius";
const ENCODER_CODEC_FROM_CONFIG: &str = "VerdictCheckEncoderCodecFromConfig";
const H264_SINK: &str = "VerdictCheckH264Sink";
const NAMED_RECORDS_SOURCE: &str = "VerdictCheckNamedRecordsSource";
const RECORDS_PASSTHROUGH: &str = "VerdictCheckRecordsPassthrough";
const RECORDS_NEEDING_X: &str = "VerdictCheckRecordsNeedingX";
const RECORDS_NEEDING_Z: &str = "VerdictCheckRecordsNeedingZ";
const RGBA_OR_NV12_TEXTURE_SOURCE: &str = "VerdictCheckRgbaOrNv12TextureSource";
const TEXTURE_PASSTHROUGH: &str = "VerdictCheckTexturePassthrough";
const RGBA_TEXTURE_SINK: &str = "VerdictCheckRgbaTextureSink";
const ANY_TEXTURE_SINK: &str = "VerdictCheckAnyTextureSink";
const UNTYPED_RGBA_PICTURE_SINK: &str = "VerdictCheckUntypedRgbaPictureSink";
const DELISLE_SOURCE: &str = "VerdictCheckDelisleSource";
const DELISLE_TO_NEWTON_NAMING_IMAGE: &str = "VerdictCheckDelisleToNewtonNamingImage";
const NEWTON_SINK_NEEDING_IMAGE: &str = "VerdictCheckNewtonSinkNeedingImage";
const NEWTON_SINK_NEEDING_DEPTH: &str = "VerdictCheckNewtonSinkNeedingDepth";

fn class_path(class_name: &str) -> NodeClassImportPath {
    NodeClassImportPath::new(format!("{}::{class_name}", module_path!())).unwrap()
}

/// A temperature on a named pin type of this module, which no other test's ports carry.
fn temperature(scale: &str) -> PinType {
    PinType::Named {
        named_pin_type: NamedPinTypeImportPath::new(format!("verdict_check:{scale}")).unwrap(),
    }
}

fn float() -> PinType {
    PinType::Float { exact_size: None }
}

fn a_typed_port(name: &str, pin_type: PinType) -> PortDescriptor {
    let family = PortFamily::carrying(&pin_type).unwrap_or(PortFamily::Records);
    PortDescriptor::new(name, name, true, PortFamilyDeclaration::of_family(family))
        .with_pin_declaration(PinDeclaration::exactly(pin_type))
}

/// A port carrying `pin_type` that flows as a stream of values, never as a held value.
fn a_streamed_port(name: &str, pin_type: PinType) -> PortDescriptor {
    let mut port = a_typed_port(name, pin_type);
    if let Some(pin) = port.pin_declaration.as_mut() {
        pin.flow = PinFlow::Stream;
    }
    port
}

/// A Texture port stating `pixel_formats`, by its pin type and by its family as a declaration of
/// an Image port states both.
fn a_texture_port(name: &str, pixel_formats: &[TexturePixelFormat]) -> PortDescriptor {
    PortDescriptor {
        family: PortFamilyDeclaration {
            formats: pixel_formats
                .iter()
                .map(|format| format.wire_name().to_owned())
                .collect(),
            ..PortFamilyDeclaration::of_family(PortFamily::Image)
        },
        ..a_typed_port(
            name,
            PinType::Texture {
                pixel_formats: pixel_formats.to_vec(),
            },
        )
    }
}

fn register(descriptor: NodeDescriptor) {
    NODE_REGISTRY
        .register_descriptor_only(descriptor)
        .expect("register the fixture descriptor");
}

/// Register `descriptor` with a constructor, as a conversion is offered only by a node type
/// whose nodes can be created.
fn register_with_a_constructor(descriptor: NodeDescriptor) {
    NODE_REGISTRY
        .register_with_a_constructor_for_a_test(
            descriptor,
            Box::new(|_node| Err(Error::NotSupported("a fixture runs no node".to_owned()))),
        )
        .expect("register the fixture node type");
}

fn a_node_type(
    node_class_import_path: NodeClassImportPath,
    inputs: Vec<PortDescriptor>,
    outputs: Vec<PortDescriptor>,
) -> NodeDescriptor {
    let short_name = node_class_import_path
        .as_str()
        .rsplit([':', '/'])
        .next()
        .expect("a path names a class")
        .to_owned();
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short_name).unwrap(),
        node_class_import_path,
        "a fixture",
    );
    descriptor.inputs = inputs;
    descriptor.outputs = outputs;
    descriptor
}

fn register_the_verdict_fixture_node_types() {
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        register(a_node_type(
            class_path(FLOAT_SOURCE),
            Vec::new(),
            vec![a_typed_port("value", float())],
        ));
        register(a_node_type(
            class_path(FLOAT_STREAM_SOURCE),
            Vec::new(),
            vec![a_streamed_port("value", float())],
        ));
        let mut shader_source = a_node_type(
            class_path(FLOAT_SHADER_SOURCE),
            Vec::new(),
            vec![a_typed_port("value", float())],
        );
        shader_source.node_kind = NodeKind::Shader;
        register(shader_source);
        register(a_node_type(
            class_path(NUMBER_SINK),
            vec![
                a_typed_port("count", PinType::Integer { exact_size: None }),
                a_typed_port("label", PinType::String {}),
                a_typed_port("gain", float()).with_input_mode(NodeInputMode::SettingOnly {
                    read: SettingReadTime::AtSetup,
                }),
            ],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(CELSIUS_SOURCE),
            Vec::new(),
            vec![a_typed_port("temperature", temperature("Celsius"))],
        ));
        register(a_node_type(
            class_path(FAHRENHEIT_SINK),
            vec![a_typed_port("temperature", temperature("Fahrenheit"))],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(KELVIN_SOURCE),
            Vec::new(),
            vec![a_typed_port("temperature", temperature("Kelvin"))],
        ));
        register(a_node_type(
            class_path(RANKINE_SINK),
            vec![a_typed_port("temperature", temperature("Rankine"))],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(KELVIN_SINK),
            vec![a_typed_port("temperature", temperature("Kelvin"))],
            Vec::new(),
        ));
        register_with_a_constructor(
            a_node_type(
                NodeClassImportPath::of_built_in_node(CELSIUS_TO_FAHRENHEIT),
                vec![a_typed_port("celsius", temperature("Celsius"))],
                vec![a_typed_port("fahrenheit", temperature("Fahrenheit"))],
            )
            .stating_a_conversion(),
        );
        register_with_a_constructor(
            a_node_type(
                class_path(KELVIN_TO_RANKINE),
                vec![a_typed_port("kelvin", temperature("Kelvin"))],
                vec![a_typed_port("rankine", temperature("Rankine"))],
            )
            .stating_a_conversion(),
        );
        let encoded_video = || PinType::EncodedVideo { codecs: Vec::new() };
        register(a_node_type(
            class_path(ENCODER_CODEC_FROM_CONFIG),
            Vec::new(),
            vec![PortDescriptor {
                family: PortFamilyDeclaration {
                    codec_from_config_key: Some("codec".to_owned()),
                    ..PortFamilyDeclaration::of_family(PortFamily::Packets)
                },
                ..a_typed_port("packets", encoded_video())
            }],
        ));
        register(a_node_type(
            class_path(H264_SINK),
            vec![PortDescriptor {
                family: PortFamilyDeclaration {
                    requires_codec: vec!["h264".to_owned()],
                    ..PortFamilyDeclaration::of_family(PortFamily::Packets)
                },
                ..a_typed_port("packets", encoded_video())
            }],
            Vec::new(),
        ));
        let records = |name: &str, family: PortFamilyDeclaration| PortDescriptor {
            family,
            ..a_typed_port(name, PinType::String {})
        };
        let of_records = || PortFamilyDeclaration::of_family(PortFamily::Records);
        register(a_node_type(
            class_path(NAMED_RECORDS_SOURCE),
            Vec::new(),
            vec![records(
                "records",
                PortFamilyDeclaration {
                    names: Some(OutputPortDeclaredNames::Stated(vec![
                        "x".to_owned(),
                        "y".to_owned(),
                    ])),
                    ..of_records()
                },
            )],
        ));
        register(a_node_type(
            class_path(RECORDS_PASSTHROUGH),
            vec![records("in", of_records())],
            vec![records(
                "out",
                PortFamilyDeclaration {
                    family_follows_input_port: Some("in".to_owned()),
                    ..of_records()
                },
            )],
        ));
        for (sink, required) in [(RECORDS_NEEDING_X, "x"), (RECORDS_NEEDING_Z, "z")] {
            register(a_node_type(
                class_path(sink),
                vec![records(
                    "records",
                    PortFamilyDeclaration {
                        requires: vec![required.to_owned()],
                        ..of_records()
                    },
                )],
                Vec::new(),
            ));
        }
        use TexturePixelFormat::{Nv12, Rgba8Unorm};
        register(a_node_type(
            class_path(RGBA_OR_NV12_TEXTURE_SOURCE),
            Vec::new(),
            vec![a_texture_port("video", &[Rgba8Unorm, Nv12])],
        ));
        register(a_node_type(
            class_path(TEXTURE_PASSTHROUGH),
            vec![a_texture_port("in", &[])],
            vec![PortDescriptor {
                family: PortFamilyDeclaration {
                    family_follows_input_port: Some("in".to_owned()),
                    ..PortFamilyDeclaration::of_family(PortFamily::Image)
                },
                ..a_texture_port("out", &[])
            }],
        ));
        register(a_node_type(
            class_path(RGBA_TEXTURE_SINK),
            vec![a_texture_port("video", &[Rgba8Unorm])],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(UNTYPED_RGBA_PICTURE_SINK),
            vec![PortDescriptor::new(
                "video",
                "video",
                true,
                PortFamilyDeclaration {
                    formats: vec![Rgba8Unorm.wire_name().to_owned()],
                    also_takes_families: vec![PortFamily::Records],
                    ..PortFamilyDeclaration::of_family(PortFamily::Image)
                },
            )],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(ANY_TEXTURE_SINK),
            vec![a_texture_port("video", &[])],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(DELISLE_SOURCE),
            Vec::new(),
            vec![a_typed_port("temperature", temperature("Delisle"))],
        ));
        register_with_a_constructor(
            a_node_type(
                NodeClassImportPath::of_built_in_node(DELISLE_TO_NEWTON_NAMING_IMAGE),
                vec![a_typed_port("delisle", temperature("Delisle"))],
                vec![PortDescriptor {
                    family: PortFamilyDeclaration {
                        names: Some(OutputPortDeclaredNames::Stated(vec!["image".to_owned()])),
                        ..of_records()
                    },
                    ..a_typed_port("newton", temperature("Newton"))
                }],
            )
            .stating_a_conversion(),
        );
        for (sink, required) in [
            (NEWTON_SINK_NEEDING_IMAGE, "image"),
            (NEWTON_SINK_NEEDING_DEPTH, "depth"),
        ] {
            register(a_node_type(
                class_path(sink),
                vec![PortDescriptor {
                    family: PortFamilyDeclaration {
                        requires: vec![required.to_owned()],
                        ..of_records()
                    },
                    ..a_typed_port("temperature", temperature("Newton"))
                }],
                Vec::new(),
            ));
        }
        register(
            a_node_type(
                NodeClassImportPath::of_built_in_node(DESCRIBED_ONLY_FAHRENHEIT_TO_CELSIUS),
                vec![a_typed_port("fahrenheit", temperature("Fahrenheit"))],
                vec![a_typed_port("celsius", temperature("Celsius"))],
            )
            .stating_a_conversion(),
        );
    });
}

fn a_node_of(compiler: &Compiler, node_type: NodeClassImportPath) -> NodeRunId {
    a_node_configured(compiler, node_type, Value::Null)
}

fn a_node_configured(
    compiler: &Compiler,
    node_type: NodeClassImportPath,
    config: Value,
) -> NodeRunId {
    compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .add_v(NodeSpec::new(node_type, config))
            .expect("the node is named")
            .first()
            .expect("the node is created")
            .id
            .clone()
    })
}

/// The refusal `refused` is when the verdict refuses the link; panics on any other error.
fn the_verdicts_refusal(refused: Error) -> PinLinkRefusal {
    match refused {
        Error::PinLinkRefused(refusal) => *refusal,
        other => panic!("a refusal in the verdict's words, not {other:?}"),
    }
}

#[test]
fn a_float_into_an_integer_connects_with_a_free_conversion() {
    register_the_verdict_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let sink = a_node_of(&compiler, class_path(NUMBER_SINK));

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "value"),
        InputLinkPortRef::new(sink, "count"),
    )
    .expect("a Float is read as an Integer for free");
    assert_eq!(the_links_in(&compiler), 1);
}

/// Mental-revert: judging the pair by its families alone refuses it as Channels into Records
/// with the family check's words, never as a refusal by pin types naming Float and String.
#[test]
fn two_typed_pins_no_rule_connects_are_refused_naming_both_types() {
    register_the_verdict_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let sink = a_node_of(&compiler, class_path(NUMBER_SINK));

    let refusal = the_verdicts_refusal(
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(source, "value"),
            InputLinkPortRef::new(sink, "label"),
        )
        .expect_err("a Float does not connect to a String"),
    );

    let words = refusal.to_string();
    assert!(
        words.contains(&format!("`{}.value` (Float)", FLOAT_SOURCE.to_lowercase())),
        "{words}"
    );
    assert!(
        words.contains(&format!("`{}.label` (String)", NUMBER_SINK.to_lowercase())),
        "{words}"
    );
    assert_eq!(
        the_links_in(&compiler),
        0,
        "the refused link is never added"
    );
}

/// A Value wire into a setting read at setup connects as its types do, and a stream of values
/// into it is refused, naming the node that samples it as the fix.
///
/// Mental-revert: refusing every link into a setting refuses the Value wire too.
#[test]
fn a_value_wire_into_a_setting_connects_and_a_stream_into_it_is_refused() {
    register_the_verdict_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let streamed = a_node_of(&compiler, class_path(FLOAT_STREAM_SOURCE));
    let sink = a_node_of(&compiler, class_path(NUMBER_SINK));

    let refusal = the_verdicts_refusal(
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(streamed, "value"),
            InputLinkPortRef::new(sink.clone(), "gain"),
        )
        .expect_err("a setting takes a Value wire only"),
    );
    let PinLinkRefusal::StreamIntoASetting {
        output_flow,
        node_name,
        input_name,
        ..
    } = &refusal
    else {
        panic!("a stream into a setting is refused as one: {refusal:?}");
    };
    assert_eq!(
        (output_flow, node_name.as_str(), input_name.as_str()),
        (
            &PinFlow::Stream,
            NUMBER_SINK.to_lowercase().as_str(),
            "gain"
        )
    );
    assert!(refusal.to_string().contains("samples"), "{refusal}");
    assert_eq!(the_links_in(&compiler), 0);

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "value"),
        InputLinkPortRef::new(sink, "gain"),
    )
    .expect("a Value wire into a setting connects as its types do");
    assert_eq!(the_links_in(&compiler), 1);
}

/// A Value wire from a shader node into a setting read at setup is refused in the verdict's
/// words, naming both nodes: the shader's value is worked out inside the read of each node it
/// feeds, and no run reads the setting. Into an input read each run, the same output connects.
///
/// Mental-revert: without the refusal of a link from a function or shader node into a setting,
/// the wire connects.
#[test]
fn a_value_wire_from_a_shader_node_into_a_setting_is_refused_naming_both_nodes() {
    register_the_verdict_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let shader = a_node_of(&compiler, class_path(FLOAT_SHADER_SOURCE));
    let sink = a_node_of(&compiler, class_path(NUMBER_SINK));

    let refusal = the_verdicts_refusal(
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(shader.clone(), "value"),
            InputLinkPortRef::new(sink.clone(), "gain"),
        )
        .expect_err("a shader node's value feeds no setting read at setup"),
    );
    assert_eq!(
        refusal,
        PinLinkRefusal::ComputedValueIntoASetting {
            output_node_name: FLOAT_SHADER_SOURCE.to_lowercase(),
            output_node_kind: NodeKind::Shader,
            node_name: NUMBER_SINK.to_lowercase(),
            input_name: "gain".to_owned(),
        }
    );
    assert!(refusal.to_string().contains("shader"), "{refusal}");
    assert_eq!(the_links_in(&compiler), 0);

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(shader, "value"),
        InputLinkPortRef::new(sink, "count"),
    )
    .expect("a shader node's value feeds an input read each run");
    assert_eq!(the_links_in(&compiler), 1);
}

/// A link a built-in conversion bridges is refused by the engine's own connect, which never
/// places a node, naming the conversion: it is found by the node type stating it, with no list
/// kept beside it.
///
/// Mental-revert: passing the verdict no conversions answers "no node type converts between
/// them".
#[test]
fn a_link_a_built_in_conversion_bridges_is_refused_naming_that_conversion() {
    register_the_verdict_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(CELSIUS_SOURCE));
    let sink = a_node_of(&compiler, class_path(FAHRENHEIT_SINK));

    let refusal = the_verdicts_refusal(
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(source, "temperature"),
            InputLinkPortRef::new(sink, "temperature"),
        )
        .expect_err("the engine's connect never places a convert node"),
    );

    let PinLinkRefusal::PinTypesDoNotConnect {
        conversions_that_bridge,
        ..
    } = &refusal
    else {
        panic!("a refusal by types: {refusal:?}");
    };
    assert_eq!(
        conversions_that_bridge
            .iter()
            .map(|conversion| &conversion.node_type)
            .collect::<Vec<_>>(),
        [&NodeClassImportPath::of_built_in_node(
            CELSIUS_TO_FAHRENHEIT
        )]
    );
    assert!(
        refusal.to_string().contains(&format!(
            "place a `tatolab.stream:{CELSIUS_TO_FAHRENHEIT}` node between them"
        )),
        "{refusal}"
    );
    assert_eq!(the_links_in(&compiler), 0);
}

#[test]
fn a_link_only_a_module_conversion_bridges_is_refused_naming_it() {
    register_the_verdict_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(KELVIN_SOURCE));
    let sink = a_node_of(&compiler, class_path(RANKINE_SINK));

    let refusal = the_verdicts_refusal(
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(source, "temperature"),
            InputLinkPortRef::new(sink, "temperature"),
        )
        .expect_err("a module's conversion is never placed"),
    );

    assert!(
        refusal
            .to_string()
            .contains(&format!("`{}`", class_path(KELVIN_TO_RANKINE))),
        "{refusal}"
    );
}

#[test]
fn a_conversion_of_another_shape_is_refused_at_registration() {
    let refusal = NODE_REGISTRY
        .register_descriptor_only(
            a_node_type(
                class_path("VerdictCheckConversionWithTwoOutputs"),
                vec![a_typed_port("in", float())],
                vec![a_typed_port("a", float()), a_typed_port("b", float())],
            )
            .stating_a_conversion(),
        )
        .expect_err("a conversion has one output")
        .to_string();
    assert!(
        refusal.contains(
            "states that it is a conversion, and it has 1 inputs besides its settings and 2 outputs"
        ),
        "{refusal}"
    );
    assert!(
        NODE_REGISTRY
            .descriptor(&class_path("VerdictCheckConversionWithTwoOutputs"))
            .is_none(),
        "the refused node type is not registered"
    );
}

/// A node type registered with no constructor is never offered as a conversion: no node of it
/// could be placed.
#[test]
fn a_conversion_no_node_of_which_can_be_created_is_never_offered() {
    register_the_verdict_fixture_node_types();
    let offered: Vec<NodeClassImportPath> = NODE_REGISTRY
        .pin_conversions()
        .iter()
        .map(|conversion| conversion.node_type.clone())
        .collect();
    assert!(
        offered.contains(&NodeClassImportPath::of_built_in_node(
            CELSIUS_TO_FAHRENHEIT
        )),
        "{offered:?}"
    );
    assert!(
        !offered.contains(&NodeClassImportPath::of_built_in_node(
            DESCRIBED_ONLY_FAHRENHEIT_TO_CELSIUS
        )),
        "{offered:?}"
    );
}

/// An output whose codec its node's config names is judged by that codec, typed or not: into an
/// input requiring h264, an h264 encoder connects and an h265 one is refused.
///
/// Mental-revert: judging the two typed ends by the verdict alone connects both.
#[test]
fn a_typed_output_whose_config_names_its_codec_is_judged_by_that_codec() {
    register_the_verdict_fixture_node_types();
    for (codec, connects) in [("h264", true), ("h265", false)] {
        let compiler = Arc::new(Compiler::new());
        let source = a_node_configured(
            &compiler,
            class_path(ENCODER_CODEC_FROM_CONFIG),
            serde_json::json!({ "codec": codec }),
        );
        let sink = a_node_of(&compiler, class_path(H264_SINK));
        let connected = connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(source, "packets"),
            InputLinkPortRef::new(sink, "packets"),
        );
        match (connected, connects) {
            (Ok(_), true) => {}
            (Err(Error::InvalidLink(words)), false) => {
                assert!(words.contains("requires a codec in"), "{words}");
            }
            (outcome, _) => panic!("an {codec} encoder into an h264 input: {outcome:?}"),
        }
    }
}

/// A typed output whose family follows an input carries the names of what arrives there, so it
/// is judged on the family check: the upstream names `x` and `y` reach an input requiring `x`,
/// and not one requiring `z`.
///
/// Mental-revert: reading the following output's own declaration refuses both, as stating no
/// names.
#[test]
fn a_typed_output_whose_family_follows_an_input_is_judged_by_what_it_carries_on() {
    register_the_verdict_fixture_node_types();
    for (sink_type, connects) in [(RECORDS_NEEDING_X, true), (RECORDS_NEEDING_Z, false)] {
        let compiler = Arc::new(Compiler::new());
        let source = a_node_of(&compiler, class_path(NAMED_RECORDS_SOURCE));
        let passthrough = a_node_of(&compiler, class_path(RECORDS_PASSTHROUGH));
        let sink = a_node_of(&compiler, class_path(sink_type));
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(source, "records"),
            InputLinkPortRef::new(passthrough.clone(), "in"),
        )
        .expect("the source feeds the passthrough");
        let connected = connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(passthrough, "out"),
            InputLinkPortRef::new(sink, "records"),
        );
        match (connected, connects) {
            (Ok(_), true) => {}
            (Err(Error::InvalidLink(words)), false) => {
                assert!(words.contains("\"z\""), "{words}");
            }
            (outcome, _) => panic!("the passthrough into {sink_type}: {outcome:?}"),
        }
    }
}

/// A link a saved graph holds whose pin types alone refuse it is kept when its ports' families
/// take it, its names, codec and formats with them; a stream into a setting is refused still.
///
/// Mental-revert: judging a saved graph's link as one made now refuses the temperatures.
#[test]
fn a_saved_graphs_link_its_pin_types_alone_refuse_is_kept_when_its_families_take_it() {
    register_the_verdict_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(KELVIN_SOURCE));
    let sink = a_node_of(&compiler, class_path(RANKINE_SINK));

    connect_on_this_thread_as(
        &compiler,
        OutputLinkPortRef::new(source, "temperature"),
        InputLinkPortRef::new(sink, "temperature"),
        ALinkConnected::OfASavedGraph,
    )
    .expect("a saved link both ends' families take is kept");
    assert_eq!(the_links_in(&compiler), 1);

    let level = a_node_of(&compiler, class_path(FLOAT_STREAM_SOURCE));
    let numbers = a_node_of(&compiler, class_path(NUMBER_SINK));
    let refusal = the_verdicts_refusal(
        connect_on_this_thread_as(
            &compiler,
            OutputLinkPortRef::new(level, "value"),
            InputLinkPortRef::new(numbers, "gain"),
            ALinkConnected::OfASavedGraph,
        )
        .expect_err("a setting takes no stream, saved or not"),
    );
    assert!(
        matches!(refusal, PinLinkRefusal::StreamIntoASetting { .. }),
        "{refusal:?}"
    );
    assert_eq!(the_links_in(&compiler), 1);
}

/// A conversion an installed module declares is one the stream's verdict finds: a refusal it
/// bridges names it.
///
/// Mental-revert: finding conversions among the natively compiled node types alone answers "no
/// node type converts between them".
#[test]
fn a_link_a_conversion_an_installed_module_declares_bridges_is_refused_naming_it() {
    use crate::core::graph::input_values::LiveInputValuesOfAStream;
    use crate::core::nodes::{
        HowANodeOfADescribedTypeRuns, InstalledModuleOfANodeType, NodeTypesOneStreamResolves,
    };
    register_the_verdict_fixture_node_types();
    let installed_conversion =
        NodeClassImportPath::new("verdict_check.installed:CelsiusToKelvin").unwrap();
    let node_types = Arc::new(NodeTypesOneStreamResolves::new());
    node_types
        .register_a_type_an_installed_module_declares(
            a_node_type(
                installed_conversion.clone(),
                vec![a_typed_port("celsius", temperature("Celsius"))],
                vec![a_typed_port("kelvin", temperature("Kelvin"))],
            )
            .stating_a_conversion(),
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(Box::new(|_node| {
                Err(Error::NotSupported("a fixture runs no node".to_owned()))
            })),
            InstalledModuleOfANodeType {
                distribution: "verdict-check-temperatures".to_owned(),
                distribution_version: "1.0.0".to_owned(),
                entry_point: "temperatures".to_owned(),
            },
        )
        .expect("an installed module may state a conversion");
    let compiler = Arc::new(Compiler::new_of_a_stream(
        node_types,
        LiveInputValuesOfAStream::default(),
    ));
    let source = a_node_of(&compiler, class_path(CELSIUS_SOURCE));
    let sink = a_node_of(&compiler, class_path(KELVIN_SINK));

    let refusal = the_verdicts_refusal(
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(source, "temperature"),
            InputLinkPortRef::new(sink, "temperature"),
        )
        .expect_err("an installed module's conversion is never placed"),
    );

    assert!(
        refusal.to_string().contains(&format!(
            "place a `{installed_conversion}` node between them"
        )),
        "{refusal}"
    );
}

/// The verdict a transaction's connect takes on a link, made nowhere, is the one connect acts
/// on: a Float into an Integer connects for free, and each link connect refuses is refused in
/// connect's very words; but a link a built-in conversion bridges, which connect itself refuses,
/// answers the convert node a transaction's connect places. No link is made by any check.
///
/// Mental-revert: refusing the link a built-in conversion bridges, as connect does, leaves a
/// transaction's connect no node to place.
#[test]
fn the_verdict_connect_takes_on_a_link_is_what_connect_acts_on_and_makes_nothing() {
    register_the_verdict_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let float_source = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let number_sink = a_node_of(&compiler, class_path(NUMBER_SINK));
    let celsius_source = a_node_of(&compiler, class_path(CELSIUS_SOURCE));
    let fahrenheit_sink = a_node_of(&compiler, class_path(FAHRENHEIT_SINK));
    let float_stream_source = a_node_of(&compiler, class_path(FLOAT_STREAM_SOURCE));

    let verdict = the_verdict_connect_takes_on_a_link(
        &compiler,
        OutputLinkPortRef::new(float_source.clone(), "value"),
        InputLinkPortRef::new(number_sink.clone(), "count"),
    )
    .expect("a Float is read as an Integer for free")
    .expect("two typed ends have a verdict");
    assert_eq!(
        verdict.outcome,
        PinLinkOutcome::ConnectsWithFreeConversion(FreePinConversion::BetweenNumbers)
    );

    for (output, input) in [
        (
            OutputLinkPortRef::new(float_source.clone(), "value"),
            InputLinkPortRef::new(number_sink.clone(), "label"),
        ),
        (
            OutputLinkPortRef::new(float_stream_source, "value"),
            InputLinkPortRef::new(number_sink.clone(), "gain"),
        ),
    ] {
        let checked = the_verdict_connect_takes_on_a_link(&compiler, output.clone(), input.clone())
            .expect_err("connect refuses the link");
        let refused =
            connect_on_this_thread(&compiler, output, input).expect_err("connect refuses the link");
        assert_eq!(checked.to_string(), refused.to_string());
    }

    let celsius_into_fahrenheit = (
        OutputLinkPortRef::new(celsius_source.clone(), "temperature"),
        InputLinkPortRef::new(fahrenheit_sink.clone(), "temperature"),
    );
    let verdict = the_verdict_connect_takes_on_a_link(
        &compiler,
        celsius_into_fahrenheit.0.clone(),
        celsius_into_fahrenheit.1.clone(),
    )
    .expect("a transaction's connect places the conversion")
    .expect("two typed ends have a verdict");
    let PinLinkOutcome::ConnectsThroughConvertNode(convert_node) = &verdict.outcome else {
        panic!("a built-in conversion bridges the ends: {verdict:?}");
    };
    assert_eq!(
        convert_node.conversion.node_type,
        NodeClassImportPath::of_built_in_node(CELSIUS_TO_FAHRENHEIT)
    );
    connect_on_this_thread(
        &compiler,
        celsius_into_fahrenheit.0,
        celsius_into_fahrenheit.1,
    )
    .expect_err("connect itself never places a convert node");
    assert_eq!(the_links_in(&compiler), 0);
}

/// A link a built-in conversion bridges answers its convert node only when each of the two
/// links through it carries the names the end it feeds requires, as connect judges each once
/// the node is made: the conversion's output states `image`, so an input requiring `image` is
/// fed through it, and one requiring `depth` is refused naming the missing name.
///
/// Mental-revert: judging the two links through the convert node by their islands alone answers
/// the node for the input requiring `depth`, which the transaction's own connect then refuses.
#[test]
fn a_convert_node_is_answered_only_when_both_its_links_carry_the_names_required() {
    register_the_verdict_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let delisle_source = a_node_of(&compiler, class_path(DELISLE_SOURCE));
    let needing_image = a_node_of(&compiler, class_path(NEWTON_SINK_NEEDING_IMAGE));
    let needing_depth = a_node_of(&compiler, class_path(NEWTON_SINK_NEEDING_DEPTH));

    let verdict = the_verdict_connect_takes_on_a_link(
        &compiler,
        OutputLinkPortRef::new(delisle_source.clone(), "temperature"),
        InputLinkPortRef::new(needing_image, "temperature"),
    )
    .expect("the conversion's output states the `image` the input requires")
    .expect("two typed ends have a verdict");
    let PinLinkOutcome::ConnectsThroughConvertNode(convert_node) = &verdict.outcome else {
        panic!("a built-in conversion bridges the ends: {verdict:?}");
    };
    assert_eq!(
        convert_node.conversion.node_type,
        NodeClassImportPath::of_built_in_node(DELISLE_TO_NEWTON_NAMING_IMAGE)
    );

    match the_verdict_connect_takes_on_a_link(
        &compiler,
        OutputLinkPortRef::new(delisle_source, "temperature"),
        InputLinkPortRef::new(needing_depth, "temperature"),
    ) {
        Err(Error::InvalidLink(words)) => {
            assert!(words.contains("requires [\"depth\"]"), "{words}");
            assert!(words.contains(DELISLE_TO_NEWTON_NAMING_IMAGE), "{words}");
        }
        other => panic!("the link out of the convert node lacks `depth`: {other:?}"),
    }
    assert_eq!(the_links_in(&compiler), 0);
}
