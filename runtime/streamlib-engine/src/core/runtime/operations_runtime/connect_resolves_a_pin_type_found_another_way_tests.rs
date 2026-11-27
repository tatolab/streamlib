// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `connect` judges a port whose type is found another way than stated exactly by the type its
//! graph resolves: one of several by the best of them, any type as taking every typed output,
//! the type linked into an input by that link, and the type a setting chooses by that choice,
//! which marks a wired link that would not fit as differing rather than refusing the choice,
//! through every output following an input on downstream.

use std::sync::Arc;

use serde_json::{Value, json};

use super::connect_on_a_test_thread::connect_on_this_thread_waiting_for;
use super::{InputValueChangesAConnectWaitsFor, the_verdict_connect_takes_on_a_link};
use crate::core::compiler::Compiler;
use crate::core::descriptors::{
    DeclaredPinType, FreePinConversion, NodeClassImportPath, NodeClassShortName, NodeDescriptor,
    NodeInputMode, PacketsStreamFacts, PinDeclaration, PinFlow, PinLinkOutcome, PinType,
    PortDescriptor, SettingReadTime,
};
use crate::core::graph::{
    GraphEdgeWithComponents, InputLinkPortRef, LinkPinTypesDifferComponent, LinkUniqueId,
    NodeRunId, OutputLinkPortRef,
};
use crate::core::nodes::{NODE_REGISTRY, NodeSpec};
use crate::core::{Error, Result};

const FLOAT_SOURCE: &str = "FoundAnotherWayFloatSource";
const STRING_SOURCE: &str = "FoundAnotherWayStringSource";
const AUDIO_SOURCE: &str = "FoundAnotherWayAudioSource";
const FLOAT_SINK: &str = "FoundAnotherWayFloatSink";
const STRING_SINK: &str = "FoundAnotherWayStringSink";
const ANY_SINK: &str = "FoundAnotherWayAnySink";
/// A node whose input `value` states no pin type.
const UNTYPED_SINK: &str = "FoundAnotherWayUntypedSink";
const STRING_OR_BOOLEAN_SINK: &str = "FoundAnotherWayStringOrBooleanSink";
const PASS_ON: &str = "FoundAnotherWayPassOn";
const CHOSEN_SOURCE: &str = "FoundAnotherWayChosenSource";
const CHOSEN_BY_DEFAULT_SOURCE: &str = "FoundAnotherWayChosenByDefaultSource";
const CHANNELS_FLOAT_SINK: &str = "FoundAnotherWayChannelsFloatSink";
/// A node whose output `value`, typed by its setting `kind` choosing H.264, Opus or a named
/// `Note` and choosing the `Note` by default, carries the type `kind` holds, declared as a
/// module declares an `OutputPin[Annotated[H264 | OPUS | Note, PinTypeFromSetting("kind")]]`, as
/// a stream.
const CODEC_CHOSEN_SOURCE: &str = "FoundAnotherWayCodecChosenSource";
const H264_SINK: &str = "FoundAnotherWayH264Sink";
/// A node whose input `value`, typed by its setting `kind`, takes a wire or a value.
const CHOSEN_SINK_TAKING_A_VALUE: &str = "FoundAnotherWayChosenSinkTakingAValue";
/// The output of a `PASS_ON` node, which takes the type of its input `value`.
const PASSED_ON: &str = "passed_on";

fn class_path(class_name: &str) -> NodeClassImportPath {
    NodeClassImportPath::new(format!("{}::{class_name}", module_path!())).unwrap()
}

fn float() -> PinType {
    PinType::Float { exact_size: None }
}

fn encoded_video(codec: &str) -> PinType {
    PinType::EncodedVideo {
        codecs: vec![codec.to_owned()],
    }
}

fn a_note() -> PinType {
    serde_json::from_value(json!({"type": "named", "named_pin_type": "notes:Note"}))
        .expect("a named type")
}

/// A port declaring `pin_type`.
fn a_port(name: &str, pin_type: DeclaredPinType) -> PortDescriptor {
    let exactly = match &pin_type {
        DeclaredPinType::Is { pin_type } => PinDeclaration::exactly(pin_type.clone()),
        _ => PinDeclaration::exactly(PinType::String {}),
    };
    PortDescriptor::new(
        name,
        name,
        true,
        PinDeclaration {
            pin_type,
            ..exactly
        },
    )
}

fn is(pin_type: PinType) -> DeclaredPinType {
    DeclaredPinType::Is { pin_type }
}

fn a_node_type(
    class_name: &str,
    inputs: Vec<PortDescriptor>,
    outputs: Vec<PortDescriptor>,
) -> NodeDescriptor {
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new(class_name).unwrap(),
        class_path(class_name),
        "a fixture",
    );
    descriptor.inputs = inputs;
    descriptor.outputs = outputs;
    descriptor
}

fn register_the_fixture_node_types() {
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        let source = |class_name, pin_type| {
            a_node_type(class_name, Vec::new(), vec![a_port("value", is(pin_type))])
        };
        let sink = |class_name, pin_type| {
            a_node_type(class_name, vec![a_port("value", pin_type)], Vec::new())
        };
        let a_type_choice = || {
            a_port("kind", is(PinType::a_choice_of_any())).with_input_mode(
                NodeInputMode::SettingOnly {
                    read: SettingReadTime::AtSetup,
                },
            )
        };
        let chosen_by = |class_name, kind: PortDescriptor| {
            a_node_type(
                class_name,
                vec![kind],
                vec![a_port(
                    "value",
                    DeclaredPinType::SetByInput {
                        input_name: "kind".to_owned(),
                    },
                )],
            )
        };
        for descriptor in [
            source(FLOAT_SOURCE, float()),
            source(STRING_SOURCE, PinType::String {}),
            source(AUDIO_SOURCE, PinType::Audio {}),
            sink(FLOAT_SINK, is(float())),
            sink(STRING_SINK, is(PinType::String {})),
            sink(ANY_SINK, DeclaredPinType::Any),
            a_node_type(
                UNTYPED_SINK,
                vec![PortDescriptor {
                    pin_declaration: None,
                    ..a_port("value", is(PinType::String {}))
                }],
                Vec::new(),
            ),
            sink(
                STRING_OR_BOOLEAN_SINK,
                DeclaredPinType::OneOf {
                    pin_types: vec![PinType::String {}, PinType::Boolean {}],
                },
            ),
            a_node_type(
                PASS_ON,
                vec![a_port("value", DeclaredPinType::Any)],
                vec![a_port(
                    PASSED_ON,
                    DeclaredPinType::SameAsInput {
                        input_port_name: "value".to_owned(),
                    },
                )],
            ),
            chosen_by(CHOSEN_SOURCE, a_type_choice()),
            chosen_by(
                CHOSEN_BY_DEFAULT_SOURCE,
                PortDescriptor {
                    pin_default: Some(json!(PinType::String {})),
                    ..a_type_choice()
                },
            ),
            a_node_type(
                CHOSEN_SINK_TAKING_A_VALUE,
                vec![
                    a_type_choice(),
                    a_port(
                        "value",
                        DeclaredPinType::SetByInput {
                            input_name: "kind".to_owned(),
                        },
                    )
                    .with_input_mode(NodeInputMode::WireOrValue),
                ],
                Vec::new(),
            ),
            a_node_type(
                CHANNELS_FLOAT_SINK,
                vec![PortDescriptor::new(
                    "value",
                    "value",
                    true,
                    PinDeclaration::exactly(float()),
                )],
                Vec::new(),
            ),
            a_node_type(
                CODEC_CHOSEN_SOURCE,
                vec![
                    a_port(
                        "kind",
                        is(PinType::PinTypeChoice {
                            among: vec![
                                encoded_video("h264"),
                                PinType::EncodedAudio {
                                    codecs: vec!["opus".to_owned()],
                                },
                                a_note(),
                            ],
                            among_the_bundles_its_stream_declares: false,
                        }),
                    )
                    .with_input_mode(NodeInputMode::SettingOnly {
                        read: SettingReadTime::AtSetup,
                    })
                    .with_pin_default(json!(a_note())),
                ],
                vec![PortDescriptor::new(
                    "value",
                    "value",
                    true,
                    PinDeclaration {
                        pin_type: DeclaredPinType::SetByInput {
                            input_name: "kind".to_owned(),
                        },
                        flow: PinFlow::Stream,
                        ..PinDeclaration::exactly(PinType::String {})
                    },
                )],
            ),
            a_node_type(
                H264_SINK,
                vec![PortDescriptor::new(
                    "value",
                    "value",
                    true,
                    PinDeclaration::exactly(encoded_video("h264")),
                )],
                Vec::new(),
            ),
        ] {
            NODE_REGISTRY
                .register_descriptor_only(descriptor)
                .expect("register the fixture descriptor");
        }
    });
}

fn a_node_of(compiler: &Compiler, class_name: &str) -> NodeRunId {
    a_node_holding(compiler, class_name, serde_json::Map::new())
}

fn a_node_holding(
    compiler: &Compiler,
    class_name: &str,
    pins: serde_json::Map<String, Value>,
) -> NodeRunId {
    compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .add_v(
                NodeSpec::new(class_path(class_name), Value::Null)
                    .with_input_values_and_studio_fields(pins, serde_json::Map::new()),
            )
            .expect("the node is named")
            .first()
            .expect("the node is created")
            .id
            .clone()
    })
}

fn choosing(pin_type: PinType) -> serde_json::Map<String, Value> {
    json!({ "kind": pin_type }).as_object().unwrap().clone()
}

/// The output a link out of `node` starts at: `passed_on` on a node passing its input on, else
/// `value`.
fn the_output_of(compiler: &Compiler, node: &NodeRunId) -> &'static str {
    let is_a_pass_on = compiler.scope(|graph, _tx| {
        graph
            .traversal()
            .v(node)
            .first()
            .is_some_and(|node| node.node_type == class_path(PASS_ON))
    });
    if is_a_pass_on { PASSED_ON } else { "value" }
}

fn connect(compiler: &Arc<Compiler>, from: &NodeRunId, to: &NodeRunId) -> Result<LinkUniqueId> {
    connect_holding_input_value_changes_by(compiler, from, to, Default::default())
}

/// [`connect`], its link made while `one_input_value_change_at_a_time` is free.
fn connect_holding_input_value_changes_by(
    compiler: &Arc<Compiler>,
    from: &NodeRunId,
    to: &NodeRunId,
    one_input_value_change_at_a_time: Arc<parking_lot::Mutex<()>>,
) -> Result<LinkUniqueId> {
    let output = the_output_of(compiler, from);
    connect_on_this_thread_waiting_for(
        compiler,
        OutputLinkPortRef::new(from.clone(), output),
        InputLinkPortRef::new(to.clone(), "value"),
        InputValueChangesAConnectWaitsFor::TheStreamsOne(one_input_value_change_at_a_time),
    )
}

fn the_outcome_connect_takes(
    compiler: &Arc<Compiler>,
    from: &NodeRunId,
    to: &NodeRunId,
) -> Result<PinLinkOutcome> {
    the_verdict_connect_takes_on_a_link(
        compiler,
        OutputLinkPortRef::new(from.clone(), the_output_of(compiler, from)),
        InputLinkPortRef::new(to.clone(), "value"),
    )
    .map(|verdict| {
        verdict
            .expect("two typed ends are judged by their verdict")
            .outcome
    })
}

#[test]
fn an_input_of_any_type_takes_every_typed_output_as_it_is() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let sink = a_node_of(&compiler, ANY_SINK);

    for source_type in [FLOAT_SOURCE, STRING_SOURCE, AUDIO_SOURCE] {
        let source = a_node_of(&compiler, source_type);
        assert_eq!(
            the_outcome_connect_takes(&compiler, &source, &sink).expect("an Any input takes it"),
            PinLinkOutcome::Connects,
            "{source_type}"
        );
    }
}

#[test]
fn an_input_of_several_types_connects_as_the_best_of_them_and_refuses_what_none_takes() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let sink = a_node_of(&compiler, STRING_OR_BOOLEAN_SINK);

    let string = a_node_of(&compiler, STRING_SOURCE);
    assert_eq!(
        the_outcome_connect_takes(&compiler, &string, &sink).expect("a String is one it takes"),
        PinLinkOutcome::Connects
    );
    let float = a_node_of(&compiler, FLOAT_SOURCE);
    assert_eq!(
        the_outcome_connect_takes(&compiler, &float, &sink).expect("a Float reads as a Boolean"),
        PinLinkOutcome::ConnectsWithFreeConversion(FreePinConversion::BetweenNumbers)
    );
    let audio = a_node_of(&compiler, AUDIO_SOURCE);
    let refused = connect(&compiler, &audio, &sink).expect_err("Audio is neither");
    assert!(
        matches!(refused, Error::PinLinkRefused(_)),
        "refused by the verdict, not {refused:?}"
    );
}

#[test]
fn an_output_of_the_type_of_an_input_takes_the_type_linked_into_it_and_links_by_it() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let float = a_node_of(&compiler, FLOAT_SOURCE);
    let pass_on = a_node_of(&compiler, PASS_ON);
    connect(&compiler, &float, &pass_on).expect("an Any input takes a Float");

    let float_sink = a_node_of(&compiler, FLOAT_SINK);
    assert_eq!(
        the_outcome_connect_takes(&compiler, &pass_on, &float_sink).expect("a Float to a Float"),
        PinLinkOutcome::Connects
    );
    let string_sink = a_node_of(&compiler, STRING_SINK);
    let refused = connect(&compiler, &pass_on, &string_sink)
        .expect_err("what is passed on is a Float, which a String does not take");
    let words = refused.to_string();
    assert!(
        words.contains("Float") && words.contains("String"),
        "{words}"
    );
}

#[test]
fn a_link_into_an_input_an_output_takes_the_type_of_is_refused_when_that_outputs_link_would_not_fit()
 {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let pass_on = a_node_of(&compiler, PASS_ON);
    let string_sink = a_node_of(&compiler, STRING_SINK);
    connect(&compiler, &pass_on, &string_sink)
        .expect("with nothing linked in, nothing judges the link out yet");

    let float = a_node_of(&compiler, FLOAT_SOURCE);
    let refused = connect(&compiler, &float, &pass_on)
        .expect_err("a Float passed on would reach an input taking a String");
    let words = refused.to_string();
    assert!(
        words.contains("takes the type linked into `value`") && words.contains("String"),
        "{words}"
    );
    let string = a_node_of(&compiler, STRING_SOURCE);
    connect(&compiler, &string, &pass_on).expect("a String passed on fits");
}

#[test]
fn an_output_whose_type_a_setting_chooses_links_by_that_choice_and_graph_serves_it() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let chosen = a_node_holding(&compiler, CHOSEN_SOURCE, choosing(PinType::String {}));

    let string_sink = a_node_of(&compiler, STRING_SINK);
    connect(&compiler, &chosen, &string_sink).expect("the setting chose a String");
    let float_sink = a_node_of(&compiler, FLOAT_SINK);
    assert!(matches!(
        connect(&compiler, &chosen, &float_sink),
        Err(Error::PinLinkRefused(_))
    ));

    let served = compiler.scope(|graph, _tx| graph.to_graph_response("a machine".to_owned()));
    let chosen_uuid = compiler.scope(|graph, _tx| {
        graph
            .traversal()
            .v(&chosen)
            .first()
            .expect("the node is held")
            .node_uuid
            .to_string()
    });
    let served_node = served
        .nodes
        .iter()
        .find(|node| node.id == chosen_uuid)
        .expect("graph serves the node");
    assert_eq!(
        served_node.ports.outputs[0]
            .pin
            .as_ref()
            .map(|pin| &pin.pin_type),
        Some(&PinType::String {})
    );
}

/// Set the setting `kind` of `chosen` to `pin_type` in `compiler`'s graph and answer each link
/// the choice reaches whose ends then differ, as it is marked.
fn choose_and_judge(
    compiler: &Compiler,
    chosen: &NodeRunId,
    pin_type: PinType,
) -> Vec<LinkPinTypesDifferComponent> {
    compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .v(chosen)
            .first_mut()
            .expect("the node is held")
            .input_values
            .insert("kind".to_owned(), json!(pin_type));
        let judged_again = graph.judge_again_each_link_the_type_choices_of_the_nodes_reach(
            std::slice::from_ref(chosen),
        );
        graph.mark_each_link_judged_again(&judged_again);
        judged_again
            .differing
            .into_iter()
            .map(|(_, differing)| differing)
            .collect()
    })
}

/// The pin type `graph` serves on the first output of `node` in `compiler`'s graph.
fn the_type_served_on_the_output_of(compiler: &Compiler, node: &NodeRunId) -> Option<PinType> {
    let served = compiler.scope(|graph, _tx| graph.to_graph_response("a machine".to_owned()));
    let node_uuid = compiler.scope(|graph, _tx| {
        graph
            .traversal()
            .v(node)
            .first()
            .expect("the node is held")
            .node_uuid
            .to_string()
    });
    served
        .nodes
        .iter()
        .find(|served_node| served_node.id == node_uuid)
        .expect("graph serves the node")
        .ports
        .outputs[0]
        .pin
        .as_ref()
        .map(|pin| pin.pin_type.clone())
}

/// A choice a wired link does not take marks the link as differing, naming both pin types, and
/// choosing back what it takes takes the mark off.
///
/// Mental-revert: judging the choice only as a link is made leaves the wired link carrying a
/// type its input does not read, unmarked.
#[test]
fn a_setting_choosing_a_type_a_wired_link_does_not_take_marks_the_link_naming_both_types() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let chosen = a_node_holding(&compiler, CHOSEN_SOURCE, choosing(PinType::String {}));
    let string_sink = a_node_of(&compiler, STRING_SINK);
    connect(&compiler, &chosen, &string_sink).expect("the setting chose a String");

    let differing = choose_and_judge(&compiler, &chosen, PinType::Audio {});

    assert_eq!(differing.len(), 1, "{differing:?}");
    assert!(
        differing[0].output_pin_type.contains("Audio")
            && differing[0].input_pin_type.contains("String"),
        "{differing:?}"
    );
    assert!(
        choose_and_judge(&compiler, &chosen, PinType::String {}).is_empty(),
        "a String fits again"
    );
}

/// A bundle of one Float member for each of `member_names`.
fn a_bundle_of_floats(member_names: &[&str]) -> PinType {
    PinType::Bundle {
        name: None,
        members: member_names
            .iter()
            .map(|name| crate::core::descriptors::BundleMember {
                name: (*name).to_owned(),
                member_type: float(),
                meaning: None,
                default_value: None,
            })
            .collect(),
    }
}

/// A new choice keeps a wire whose member it still holds as that member's type, and marks one
/// whose member it no longer holds as differing, naming the member.
///
/// Mental-revert: a check reading only links between two typed ends passes over the wire at a
/// member the bundle no longer holds, and the wire is left dangling, unmarked.
#[test]
fn a_setting_choosing_a_bundle_without_a_wired_member_marks_the_wire() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let chosen = a_node_holding(
        &compiler,
        CHOSEN_SOURCE,
        choosing(a_bundle_of_floats(&["level", "pan"])),
    );
    let float_sink = a_node_of(&compiler, FLOAT_SINK);
    connect_on_this_thread_waiting_for(
        &compiler,
        OutputLinkPortRef::new(chosen.clone(), "value").at_member(Some("pan".to_owned())),
        InputLinkPortRef::new(float_sink.clone(), "value"),
        InputValueChangesAConnectWaitsFor::TheStreamsOne(Default::default()),
    )
    .expect("the bundle chosen holds a Float `pan`");

    assert!(
        choose_and_judge(&compiler, &chosen, a_bundle_of_floats(&["pan", "width"])).is_empty(),
        "the bundle still holds `pan`"
    );
    let differing = choose_and_judge(&compiler, &chosen, a_bundle_of_floats(&["level"]));

    assert_eq!(differing.len(), 1, "{differing:?}");
    let words = &differing[0].why;
    assert!(words.contains("pan"), "{words}");
    assert!(
        !words.contains(&chosen.to_string()) && !words.contains(&float_sink.to_string()),
        "the wire is named by its nodes' names, not their run ids: {words}"
    );
    assert!(
        !words.contains("To fix:"),
        "the fix is named where the link is reported: {words}"
    );
}

/// A wire starting at a member of a port whose setting chooses a bundle is judged by the
/// member's type, not by the type of the whole port it is a member of.
///
/// Mental-revert: judging the whole port refuses a Float member of a bundle wired to a Float
/// input, though the member and the input carry one type.
#[test]
fn a_wire_at_a_member_of_a_port_a_setting_types_is_judged_by_the_member_not_the_whole_port() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let chosen = a_node_holding(
        &compiler,
        CHOSEN_SOURCE,
        choosing(a_bundle_of_floats(&["level"])),
    );
    let channels_float_sink = a_node_of(&compiler, CHANNELS_FLOAT_SINK);

    connect_on_this_thread_waiting_for(
        &compiler,
        OutputLinkPortRef::new(chosen.clone(), "value").at_member(Some("level".to_owned())),
        InputLinkPortRef::new(channels_float_sink.clone(), "value"),
        InputValueChangesAConnectWaitsFor::TheStreamsOne(Default::default()),
    )
    .expect("the member `level` and the input both carry a Float");
    let refused = connect(&compiler, &chosen, &channels_float_sink)
        .expect_err("the whole bundle does not feed a Float input");
    assert!(
        matches!(refused, Error::InvalidLink(_) | Error::PinLinkRefused(_)),
        "{refused:?}"
    );
}

#[test]
fn an_output_whose_setting_chooses_no_type_yet_is_judged_once_it_does() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let chosen = a_node_of(&compiler, CHOSEN_SOURCE);
    let float_sink = a_node_of(&compiler, FLOAT_SINK);

    let verdict = the_verdict_connect_takes_on_a_link(
        &compiler,
        OutputLinkPortRef::new(chosen, "value"),
        InputLinkPortRef::new(float_sink, "value"),
    )
    .expect("an end not typed yet takes the link");

    assert_eq!(
        verdict, None,
        "no verdict judges an end its graph types not"
    );
}

#[test]
fn a_link_into_the_first_of_a_chain_of_pass_ons_is_refused_when_the_last_ones_link_would_not_fit() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let first = a_node_of(&compiler, PASS_ON);
    let second = a_node_of(&compiler, PASS_ON);
    let string_sink = a_node_of(&compiler, STRING_SINK);
    connect(&compiler, &second, &string_sink).expect("nothing typed is passed on yet");
    connect(&compiler, &first, &second).expect("nothing typed is passed on yet");

    let float = a_node_of(&compiler, FLOAT_SOURCE);
    let words = connect(&compiler, &float, &first)
        .expect_err("a Float passed on twice would reach an input taking a String")
        .to_string();

    assert!(
        words.contains("takes the type linked into `value`") && words.contains("String"),
        "{words}"
    );
    let string = a_node_of(&compiler, STRING_SOURCE);
    connect(&compiler, &string, &first).expect("a String passed on twice fits");
}

#[test]
fn a_setting_choosing_a_type_a_link_past_a_pass_on_does_not_take_marks_that_link() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let chosen = a_node_holding(&compiler, CHOSEN_SOURCE, choosing(PinType::String {}));
    let pass_on = a_node_of(&compiler, PASS_ON);
    let string_sink = a_node_of(&compiler, STRING_SINK);
    connect(&compiler, &chosen, &pass_on).expect("an Any input takes a String");
    connect(&compiler, &pass_on, &string_sink).expect("a String passed on fits");

    let differing = choose_and_judge(&compiler, &chosen, PinType::Audio {});

    assert_eq!(
        differing.len(),
        1,
        "Audio passed on reaches the String input wired past the pass-on: {differing:?}"
    );
    assert!(
        differing[0].output_pin_type.contains("Audio")
            && differing[0].input_pin_type.contains("String"),
        "{differing:?}"
    );
    assert!(
        choose_and_judge(&compiler, &chosen, PinType::String {}).is_empty(),
        "a String still fits"
    );
}

#[test]
fn a_setting_holding_no_saved_type_chooses_the_type_it_declares_by_default() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let chosen = a_node_of(&compiler, CHOSEN_BY_DEFAULT_SOURCE);

    assert_eq!(
        the_type_served_on_the_output_of(&compiler, &chosen),
        Some(PinType::String {})
    );
    let float_sink = a_node_of(&compiler, FLOAT_SINK);
    assert!(matches!(
        connect(&compiler, &chosen, &float_sink),
        Err(Error::PinLinkRefused(_))
    ));
    let string_sink = a_node_of(&compiler, STRING_SINK);
    connect(&compiler, &chosen, &string_sink).expect("the default chose a String");
}

/// Whether output `value` of `node` carries stream facts of Packets now, which only an encoded
/// output takes.
fn carries_packets_stream_facts(compiler: &Compiler, node: &NodeRunId) -> bool {
    let facts = compiler.scope(|graph, _tx| {
        std::sync::Arc::clone(
            graph
                .traversal()
                .v(node)
                .first()
                .expect("the node is held")
                .stream_facts_of_output_ports(),
        )
    });
    facts
        .replace_facts_of("value", PacketsStreamFacts::default().into())
        .is_ok()
}

#[test]
fn an_output_whose_choice_sets_its_type_carries_the_codec_of_the_type_chosen() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());

    let chose_h264 = a_node_holding(
        &compiler,
        CODEC_CHOSEN_SOURCE,
        choosing(encoded_video("h264")),
    );
    connect(&compiler, &chose_h264, &a_node_of(&compiler, H264_SINK))
        .expect("a choice of H.264 carries Packets of H.264");
    assert!(carries_packets_stream_facts(&compiler, &chose_h264));

    let chose_opus = a_node_holding(
        &compiler,
        CODEC_CHOSEN_SOURCE,
        choosing(PinType::EncodedAudio {
            codecs: vec!["opus".to_owned()],
        }),
    );
    let refused = connect(&compiler, &chose_opus, &a_node_of(&compiler, H264_SINK))
        .expect_err("a choice of Opus does not feed an H.264 input");
    assert!(
        matches!(&refused, Error::PinLinkRefused(_)) && refused.to_string().contains("opus"),
        "refused naming the codec chosen, not {refused:?}"
    );
}

#[test]
fn an_output_whose_choice_holds_no_value_carries_the_type_of_its_default() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let chose_by_default = a_node_holding(&compiler, CODEC_CHOSEN_SOURCE, serde_json::Map::new());
    let chose_a_note = a_node_holding(&compiler, CODEC_CHOSEN_SOURCE, choosing(a_note()));

    for (node, held) in [(chose_by_default, "by default"), (chose_a_note, "saved")] {
        let refused = connect(&compiler, &node, &a_node_of(&compiler, H264_SINK))
            .expect_err("a `Note` chosen feeds no Encoded video input");
        assert!(
            matches!(&refused, Error::PinLinkRefused(_)),
            "a `Note` chosen {held} is refused by its pin type, not {refused:?}"
        );
        assert!(
            !carries_packets_stream_facts(&compiler, &node),
            "a `Note` chosen {held} carries no Packets facts, though H.264 is declared first"
        );
    }
}

#[test]
fn a_loop_of_pass_ons_resolves_untyped_rather_than_following_itself() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let first = a_node_of(&compiler, PASS_ON);
    let second = a_node_of(&compiler, PASS_ON);
    connect(&compiler, &first, &second).expect("nothing typed is passed on");
    connect(&compiler, &second, &first).expect("nothing typed is passed on round the loop");

    assert_eq!(the_type_served_on_the_output_of(&compiler, &first), None);
    assert_eq!(the_type_served_on_the_output_of(&compiler, &second), None);
}

#[test]
fn a_connect_waits_while_an_input_value_change_is_made() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let string = a_node_of(&compiler, STRING_SOURCE);
    let string_sink = a_node_of(&compiler, STRING_SINK);
    let one_input_value_change_at_a_time = Arc::new(parking_lot::Mutex::new(()));

    let change_made = one_input_value_change_at_a_time.lock();
    let connecting = std::thread::spawn({
        let compiler = Arc::clone(&compiler);
        let one_input_value_change_at_a_time = Arc::clone(&one_input_value_change_at_a_time);
        move || {
            connect_holding_input_value_changes_by(
                &compiler,
                &string,
                &string_sink,
                one_input_value_change_at_a_time,
            )
        }
    });
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(
        !connecting.is_finished(),
        "the connect waits for the change"
    );
    drop(change_made);

    connecting
        .join()
        .expect("the connect does not panic")
        .expect("a String to a String connects once the change is made");
}

/// A value set on an input whose setting chooses a bundle is handed to its node as that bundle,
/// each member it leaves out reading its own default.
///
/// Mental-revert: reading the input by the pin it states, which is none, hands its node nothing,
/// so MakeBundle's set `label` read as its default on the rig.
#[test]
fn a_value_set_on_an_input_a_setting_types_as_a_bundle_is_handed_to_its_node() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let reading = PinType::Bundle {
        name: None,
        members: vec![
            crate::core::descriptors::BundleMember {
                name: "level".to_owned(),
                member_type: float(),
                meaning: None,
                default_value: None,
            },
            crate::core::descriptors::BundleMember {
                name: "label".to_owned(),
                member_type: PinType::String {},
                meaning: None,
                default_value: Some(json!("unnamed")),
            },
        ],
    };
    let mut pins = choosing(reading);
    pins.insert("value".to_owned(), json!({"level": 0.5, "label": "set"}));
    let sink = a_node_holding(&compiler, CHOSEN_SINK_TAKING_A_VALUE, pins);

    let handed = compiler.scope(|graph, _tx| {
        let node = graph
            .traversal()
            .v(&sink)
            .first()
            .expect("the node is held");
        crate::core::graph::input_values::the_unlinked_input_values(graph, node)
    });

    assert_eq!(
        handed.get("value"),
        Some(&json!({"level": 0.5, "label": "set"}))
    );
}

/// Judging a choice again changes nothing in the graph until the compile marks what it found, so
/// a batch refused before it begins changing what runs leaves no link marked.
///
/// Mental-revert: a judge marking as it judges leaves a refused batch's link held as differing
/// while it still carries what it did.
#[test]
fn judging_a_choice_again_marks_no_link_until_the_marks_are_made() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let chosen = a_node_holding(&compiler, CHOSEN_SOURCE, choosing(PinType::String {}));
    let string_sink = a_node_of(&compiler, STRING_SINK);
    let link = connect(&compiler, &chosen, &string_sink).expect("the setting chose a String");

    let marked = compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .v(&chosen)
            .first_mut()
            .expect("the node is held")
            .input_values
            .insert("kind".to_owned(), json!(PinType::Audio {}));
        let judged_again = graph.judge_again_each_link_the_type_choices_of_the_nodes_reach(
            std::slice::from_ref(&chosen),
        );
        assert_eq!(judged_again.differing.len(), 1, "{judged_again:?}");
        graph.the_pin_types_of_the_link_differ(&link)
    });

    assert!(!marked, "judging alone marks nothing");
}

/// A link past a pass-on that a choice upstream left differing connects again once the link
/// feeding the pass-on is taken off, judged again as a marked link whatever node changed.
///
/// Mental-revert: judging only the links a changed node's choices reach leaves the link held
/// unwired once the chooser no longer reaches it.
#[test]
fn a_marked_link_past_a_pass_on_connects_again_once_the_link_feeding_the_pass_on_is_taken_off() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let chosen = a_node_holding(&compiler, CHOSEN_SOURCE, choosing(PinType::String {}));
    let pass_on = a_node_of(&compiler, PASS_ON);
    let string_sink = a_node_of(&compiler, STRING_SINK);
    let into_the_pass_on = connect(&compiler, &chosen, &pass_on).expect("an Any input takes it");
    let past_the_pass_on =
        connect(&compiler, &pass_on, &string_sink).expect("a String passed on fits");
    assert_eq!(
        choose_and_judge(&compiler, &chosen, PinType::Audio {}).len(),
        1
    );

    let judged_again = compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .e(&into_the_pass_on)
            .first_mut()
            .expect("the link is held")
            .insert(crate::core::graph::PendingDeletionComponent);
        graph.judge_again_each_link_the_type_choices_of_the_nodes_reach(&[])
    });

    assert_eq!(judged_again.connecting_again, vec![past_the_pass_on]);
}

/// A saved graph's link past a pass-on, whose ends a choice upstream left differing, is kept for
/// the compile to hold unwired rather than refusing the graph it was saved in; made now, the
/// same link is refused.
///
/// Mental-revert: keeping only a saved link a choice of one of its own ends types refuses the
/// whole saved graph.
#[test]
fn a_saved_link_past_a_pass_on_a_choice_upstream_left_differing_is_kept() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let chosen = a_node_holding(&compiler, CHOSEN_SOURCE, choosing(PinType::Audio {}));
    let pass_on = a_node_of(&compiler, PASS_ON);
    let string_sink = a_node_of(&compiler, STRING_SINK);
    connect(&compiler, &chosen, &pass_on).expect("an Any input takes Audio");

    compiler.scope(|graph, _tx| {
        let from = OutputLinkPortRef::new(pass_on.clone(), PASSED_ON);
        let to = InputLinkPortRef::new(string_sink.clone(), "value");
        crate::core::graph::refuse_a_link_its_pin_types_refuse(
            graph,
            &from,
            &to,
            crate::core::graph::ALinkConnected::MadeNow,
        )
        .expect_err("Audio passed on does not connect to a String input");
        assert_eq!(
            crate::core::graph::refuse_a_link_its_pin_types_refuse(
                graph,
                &from,
                &to,
                crate::core::graph::ALinkConnected::OfASavedGraph,
            )
            .expect("a saved link a choice left differing is kept"),
            None
        );
        assert_eq!(
            crate::core::graph::the_nodes_whose_type_choices_reach_the_link(graph, &from, &to),
            vec![chosen.clone()]
        );
    });
}

/// A choice leaving a named type at an output whose link enters an input stating no type marks
/// the link, which no named type's bytes reach; choosing a type the input takes again connects it.
///
/// Mental-revert: judging only the links whose two ends are typed leaves the link wired, the
/// input reading bytes of a named type it cannot read.
#[test]
fn a_named_type_chosen_into_an_input_stating_no_type_marks_the_link() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let chosen = a_node_holding(&compiler, CHOSEN_SOURCE, choosing(PinType::String {}));
    let untyped_sink = a_node_of(&compiler, UNTYPED_SINK);
    let link = connect(&compiler, &chosen, &untyped_sink).expect("an untyped input takes a String");

    let differing = choose_and_judge(&compiler, &chosen, a_note());

    assert_eq!(differing.len(), 1, "{differing:?}");
    assert!(differing[0].why.contains("named type"), "{differing:?}");
    assert!(compiler.scope(|graph, _tx| graph.the_pin_types_of_the_link_differ(&link)));

    let differing = choose_and_judge(&compiler, &chosen, PinType::String {});

    assert!(differing.is_empty(), "{differing:?}");
    assert!(!compiler.scope(|graph, _tx| graph.the_pin_types_of_the_link_differ(&link)));
}
