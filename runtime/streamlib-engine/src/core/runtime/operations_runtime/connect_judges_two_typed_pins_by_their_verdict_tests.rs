// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `connect` between two typed pins makes the link the verdict connects, as it is or through a
//! free conversion, and refuses one that needs a convert node or does not connect, in the
//! verdict's words, naming the conversions the registered node types state.

use std::sync::Arc;

use serde_json::Value;

use super::{
    ALinkConnected, LiveGraphChangeOnOneStream, connect_impl, the_verdict_connect_takes_on_a_link,
};
use crate::core::compiler::Compiler;
use crate::core::descriptors::{
    FreePinConversion, NamedPinTypeImportPath, NodeClassImportPath, NodeClassShortName,
    NodeDescriptor, NodeInputMode, OutputPortDeclaredNames, PinDeclaration, PinLinkOutcome,
    PinLinkRefusal, PinType, PortDescriptor, PortFamily, PortFamilyDeclaration, SettingReadTime,
};
use crate::core::graph::{InputLinkPortRef, LinkUniqueId, NodeRunId, OutputLinkPortRef};
use crate::core::nodes::{NODE_REGISTRY, NodeSpec};
use crate::core::pubsub::LoadedStreamIdentity;
use crate::core::{Error, Result};

const FLOAT_SOURCE: &str = "VerdictCheckFloatSource";
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

fn connect_on_this_thread(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<LinkUniqueId> {
    connect_on_this_thread_as(compiler, from, to, ALinkConnected::MadeNow)
}

fn connect_on_this_thread_as(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
    a_link_connected: ALinkConnected,
) -> Result<LinkUniqueId> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime")
        .block_on(connect_impl(
            LiveGraphChangeOnOneStream {
                compiler: Arc::clone(compiler),
                live: None,
                stream: LoadedStreamIdentity {
                    runtime_id: "Rverdictcheck".to_string(),
                    stream_id: "main".to_string(),
                    stream_tag: crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1),
                },
            },
            from,
            to,
            a_link_connected,
        ))
}

fn the_links_in(compiler: &Compiler) -> usize {
    compiler.scope(|graph, _tx| graph.traversal().e(()).iter().count())
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

#[test]
fn a_typed_link_into_a_setting_is_refused_as_taking_no_wire() {
    register_the_verdict_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let sink = a_node_of(&compiler, class_path(NUMBER_SINK));

    let refusal = the_verdicts_refusal(
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(source, "value"),
            InputLinkPortRef::new(sink, "gain"),
        )
        .expect_err("a setting takes no wire"),
    );

    assert_eq!(
        refusal,
        PinLinkRefusal::InputTakesNoWire {
            node_name: NUMBER_SINK.to_lowercase(),
            input_name: "gain".to_owned(),
        }
    );
    assert_eq!(the_links_in(&compiler), 0);
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
        conversions_that_bridge,
        &[NodeClassImportPath::of_built_in_node(CELSIUS_TO_FAHRENHEIT)]
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
            "states that it is a conversion, and it has 1 inputs that take a wire and 2 outputs"
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
/// take it, its names, codec and formats with them; a link into a setting is refused still.
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

    let level = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let numbers = a_node_of(&compiler, class_path(NUMBER_SINK));
    let refusal = the_verdicts_refusal(
        connect_on_this_thread_as(
            &compiler,
            OutputLinkPortRef::new(level, "value"),
            InputLinkPortRef::new(numbers, "gain"),
            ALinkConnected::OfASavedGraph,
        )
        .expect_err("a setting takes no wire, saved or not"),
    );
    assert!(
        matches!(refusal, PinLinkRefusal::InputTakesNoWire { .. }),
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

/// The verdict connect takes on a link, made nowhere, is the one connect acts on: a Float into
/// an Integer connects for free, and each link connect refuses, a link a built-in conversion
/// bridges among them, is refused in connect's very words; no link is made by any check.
///
/// Mental-revert: a check judging by the verdict alone would answer the convert node connect
/// refuses, and one adding the link would leave it in the graph.
#[test]
fn the_verdict_connect_takes_on_a_link_is_what_connect_acts_on_and_makes_nothing() {
    register_the_verdict_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let float_source = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let number_sink = a_node_of(&compiler, class_path(NUMBER_SINK));
    let celsius_source = a_node_of(&compiler, class_path(CELSIUS_SOURCE));
    let fahrenheit_sink = a_node_of(&compiler, class_path(FAHRENHEIT_SINK));

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
            OutputLinkPortRef::new(float_source.clone(), "value"),
            InputLinkPortRef::new(number_sink.clone(), "gain"),
        ),
        (
            OutputLinkPortRef::new(celsius_source.clone(), "temperature"),
            InputLinkPortRef::new(fahrenheit_sink.clone(), "temperature"),
        ),
    ] {
        let checked = the_verdict_connect_takes_on_a_link(&compiler, output.clone(), input.clone())
            .expect_err("connect refuses the link");
        let refused =
            connect_on_this_thread(&compiler, output, input).expect_err("connect refuses the link");
        assert_eq!(checked.to_string(), refused.to_string());
    }
    assert_eq!(the_links_in(&compiler), 0);
}
