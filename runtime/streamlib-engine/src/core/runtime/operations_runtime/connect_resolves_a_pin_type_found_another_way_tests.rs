// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `connect` judges a port whose type is found another way than stated exactly by the type its
//! graph resolves: one of several by the best of them, any type as taking every typed output,
//! the type linked into an input by that link, and the type a setting chooses by that choice,
//! which a wired link that would not fit refuses.

use std::sync::Arc;

use serde_json::{Value, json};

use super::{
    ALinkConnected, LiveGraphChangeOnOneStream, connect_impl, the_verdict_connect_takes_on_a_link,
};
use crate::core::compiler::Compiler;
use crate::core::descriptors::{
    DeclaredPinType, FreePinConversion, NodeClassImportPath, NodeClassShortName, NodeDescriptor,
    NodeInputMode, PinDeclaration, PinLinkOutcome, PinType, PortDescriptor, PortFamily,
    PortFamilyDeclaration, SettingReadTime,
};
use crate::core::graph::{
    InputLinkPortRef, LinkUniqueId, NodeRunId, OutputLinkPortRef,
    refuse_a_type_choice_a_wired_link_of_the_node_does_not_fit,
};
use crate::core::nodes::{NODE_REGISTRY, NodeSpec};
use crate::core::pubsub::LoadedStreamIdentity;
use crate::core::{Error, Result};

const FLOAT_SOURCE: &str = "FoundAnotherWayFloatSource";
const STRING_SOURCE: &str = "FoundAnotherWayStringSource";
const AUDIO_SOURCE: &str = "FoundAnotherWayAudioSource";
const FLOAT_SINK: &str = "FoundAnotherWayFloatSink";
const STRING_SINK: &str = "FoundAnotherWayStringSink";
const ANY_SINK: &str = "FoundAnotherWayAnySink";
const STRING_OR_BOOLEAN_SINK: &str = "FoundAnotherWayStringOrBooleanSink";
const PASS_ON: &str = "FoundAnotherWayPassOn";
const CHOSEN_SOURCE: &str = "FoundAnotherWayChosenSource";
/// The output of a `PASS_ON` node, which takes the type of its input `value`.
const PASSED_ON: &str = "passed_on";

fn class_path(class_name: &str) -> NodeClassImportPath {
    NodeClassImportPath::new(format!("{}::{class_name}", module_path!())).unwrap()
}

fn float() -> PinType {
    PinType::Float { exact_size: None }
}

/// A port declaring `pin_type`, of a family no test here judges a link by.
fn a_port(name: &str, pin_type: DeclaredPinType) -> PortDescriptor {
    let exactly = match &pin_type {
        DeclaredPinType::Is { pin_type } => PinDeclaration::exactly(pin_type.clone()),
        _ => PinDeclaration::exactly(PinType::String {}),
    };
    PortDescriptor::new(
        name,
        name,
        true,
        PortFamilyDeclaration::of_family(PortFamily::Records),
    )
    .with_pin_declaration(PinDeclaration {
        pin_type,
        ..exactly
    })
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
        for descriptor in [
            source(FLOAT_SOURCE, float()),
            source(STRING_SOURCE, PinType::String {}),
            source(AUDIO_SOURCE, PinType::Audio {}),
            sink(FLOAT_SINK, is(float())),
            sink(STRING_SINK, is(PinType::String {})),
            sink(ANY_SINK, DeclaredPinType::Any),
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
            a_node_type(
                CHOSEN_SOURCE,
                vec![
                    a_port("kind", is(PinType::PinTypeChoice { among: Vec::new() }))
                        .with_input_mode(NodeInputMode::SettingOnly {
                            read: SettingReadTime::AtSetup,
                        }),
                ],
                vec![a_port(
                    "value",
                    DeclaredPinType::SetByInput {
                        input_name: "kind".to_owned(),
                    },
                )],
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
                    .with_pins_and_studio_fields(pins, serde_json::Map::new()),
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

/// The output a link out of `node` starts at: `passed_on` on a `PASS_ON` node, else `value`.
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
    let output = the_output_of(compiler, from);
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime")
        .block_on(connect_impl(
            LiveGraphChangeOnOneStream {
                compiler: Arc::clone(compiler),
                live: None,
                stream: LoadedStreamIdentity {
                    runtime_id: "Rfoundanotherway".to_string(),
                    stream_id: "main".to_string(),
                    stream_tag: crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1),
                },
            },
            OutputLinkPortRef::new(from.clone(), output),
            InputLinkPortRef::new(to.clone(), "value"),
            ALinkConnected::MadeNow,
        ))
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
        .expect("with nothing linked in, its families judge the link out");

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

#[test]
fn a_setting_choosing_a_type_a_wired_link_does_not_take_is_refused_naming_the_link() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let chosen = a_node_holding(&compiler, CHOSEN_SOURCE, choosing(PinType::String {}));
    let string_sink = a_node_of(&compiler, STRING_SINK);
    connect(&compiler, &chosen, &string_sink).expect("the setting chose a String");

    let refused = compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .v(&chosen)
            .first_mut()
            .expect("the node is held")
            .pins
            .insert("kind".to_owned(), json!(PinType::Audio {}));
        refuse_a_type_choice_a_wired_link_of_the_node_does_not_fit(graph, &chosen, "kind")
    });

    let words = refused
        .expect_err("Audio does not connect to the String input wired")
        .to_string();
    assert!(
        words.contains("setting `kind`") && words.contains("Audio"),
        "{words}"
    );
}

#[test]
fn an_output_whose_setting_chooses_no_type_yet_is_judged_by_its_family() {
    register_the_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let chosen = a_node_of(&compiler, CHOSEN_SOURCE);
    let float_sink = a_node_of(&compiler, FLOAT_SINK);

    let verdict = the_verdict_connect_takes_on_a_link(
        &compiler,
        OutputLinkPortRef::new(chosen, "value"),
        InputLinkPortRef::new(float_sink, "value"),
    )
    .expect("the two Records ports' families take the link");

    assert_eq!(
        verdict, None,
        "no verdict judges an end its graph types not"
    );
}
