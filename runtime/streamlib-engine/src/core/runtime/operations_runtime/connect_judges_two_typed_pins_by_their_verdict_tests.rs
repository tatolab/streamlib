// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `connect` between two typed pins makes the link the verdict connects, as it is or through a
//! free conversion, and refuses one that needs a convert node or does not connect, in the
//! verdict's words, naming the conversions the registered node types state.

use std::sync::Arc;

use serde_json::Value;

use super::{LiveGraphChangeOnOneStream, connect_impl};
use crate::core::compiler::Compiler;
use crate::core::descriptors::{
    NamedPinTypeImportPath, NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeInputMode,
    PinDeclaration, PinLinkRefusal, PinType, PortDescriptor, PortFamily, PortFamilyDeclaration,
    SettingReadTime,
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
const CELSIUS_TO_FAHRENHEIT: &str = "VerdictCheckCelsiusToFahrenheit";
const KELVIN_TO_RANKINE: &str = "VerdictCheckKelvinToRankine";

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
        register(
            a_node_type(
                NodeClassImportPath::of_built_in_node(CELSIUS_TO_FAHRENHEIT),
                vec![a_typed_port("celsius", temperature("Celsius"))],
                vec![a_typed_port("fahrenheit", temperature("Fahrenheit"))],
            )
            .stating_a_conversion(),
        );
        register(
            a_node_type(
                class_path(KELVIN_TO_RANKINE),
                vec![a_typed_port("kelvin", temperature("Kelvin"))],
                vec![a_typed_port("rankine", temperature("Rankine"))],
            )
            .stating_a_conversion(),
        );
    });
}

fn a_node_of(compiler: &Compiler, node_type: NodeClassImportPath) -> NodeRunId {
    compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .add_v(NodeSpec::new(node_type, Value::Null))
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
        !NODE_REGISTRY.is_registered(&class_path("VerdictCheckConversionWithTwoOutputs")),
        "the refused node type is not registered"
    );
}
