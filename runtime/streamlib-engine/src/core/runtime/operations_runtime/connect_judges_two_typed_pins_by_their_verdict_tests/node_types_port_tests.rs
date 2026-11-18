// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link between a placed end and a port of a node type no node has yet is judged as `connect`
//! judges it once a node of the type is placed: the port by what it declares, and no node left
//! in the graph.

use super::super::{
    ALinkEndTheGraphHolds, the_verdict_connect_takes_on_a_link_with_a_node_types_port,
};
use super::*;
use crate::core::descriptors::{DeclaredPinType, PinAnnotations, PinFlow, PinLinkWarning};

const ONE_OF_SINK: &str = "VerdictCheckOneOfSink";
const ANY_SINK: &str = "VerdictCheckAnySink";
const CHOSEN_TYPE_SINK: &str = "VerdictCheckChosenTypeSink";

/// A port declaring `pin_type`, read as a value, of the Records family.
fn a_port_declaring(name: &str, pin_type: DeclaredPinType) -> PortDescriptor {
    PortDescriptor::new(
        name,
        name,
        true,
        PortFamilyDeclaration::of_family(PortFamily::Records),
    )
    .with_pin_declaration(PinDeclaration {
        pin_type,
        flow: PinFlow::Value,
        annotations: PinAnnotations::default(),
        former_names: Vec::new(),
        may_be_unset: false,
    })
}

/// Register the verdict's fixture node types and the ones whose ports declare more than one
/// exact type, once per process.
fn register_the_node_types_port_fixtures() {
    register_the_verdict_fixture_node_types();
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        register(a_node_type(
            class_path(ONE_OF_SINK),
            vec![a_port_declaring(
                "either",
                DeclaredPinType::OneOf {
                    pin_types: vec![PinType::String {}, float()],
                },
            )],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(ANY_SINK),
            vec![a_port_declaring("anything", DeclaredPinType::Any)],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(CHOSEN_TYPE_SINK),
            vec![a_port_declaring(
                "chosen",
                DeclaredPinType::SetByInput {
                    input_name: "kind".to_owned(),
                },
            )],
            Vec::new(),
        ));
    });
}

fn the_nodes_in(compiler: &Compiler) -> usize {
    compiler.scope(|graph, _tx| graph.traversal().v(()).iter().count())
}

/// The verdict on a link from the output `port` of `source` into `input` of `node_type`.
fn the_verdict_into_a_node_types_input(
    compiler: &Arc<Compiler>,
    source: &NodeRunId,
    port: &str,
    node_type: &str,
    input: &str,
) -> Result<Option<crate::core::descriptors::PinLinkVerdict>> {
    the_verdict_connect_takes_on_a_link_with_a_node_types_port(
        compiler,
        &ALinkEndTheGraphHolds::Output(OutputLinkPortRef::new(source.clone(), port)),
        &class_path(node_type),
        input,
    )
}

/// A node type's exactly typed port is judged as a placed node of the type is: a Float into
/// its Integer input connects for free, into its String input or its setting it is refused in
/// connect's very words, and the graph holds no more nodes and links after each check than
/// before.
///
/// Mental-revert: judging the node type's port by its family alone would answer `None` for the
/// Integer input and refuse no String; leaving the held node in the graph would count it.
#[test]
fn a_node_types_typed_port_is_judged_as_a_placed_node_of_the_type_is() {
    register_the_node_types_port_fixtures();
    let compiler = Arc::new(Compiler::new());
    let float_source = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let nodes_before = the_nodes_in(&compiler);

    let verdict = the_verdict_into_a_node_types_input(
        &compiler,
        &float_source,
        "value",
        NUMBER_SINK,
        "count",
    )
    .expect("a Float is read as an Integer for free")
    .expect("two typed ends have a verdict");
    assert_eq!(
        verdict.outcome,
        PinLinkOutcome::ConnectsWithFreeConversion(FreePinConversion::BetweenNumbers)
    );
    assert_eq!(the_nodes_in(&compiler), nodes_before);

    let placed_sink = a_node_of(&compiler, class_path(NUMBER_SINK));
    for input in ["label", "gain"] {
        let judged = the_verdict_into_a_node_types_input(
            &compiler,
            &float_source,
            "value",
            NUMBER_SINK,
            input,
        )
        .expect_err("connect refuses the link");
        let placed = the_verdict_connect_takes_on_a_link(
            &compiler,
            OutputLinkPortRef::new(float_source.clone(), "value"),
            InputLinkPortRef::new(placed_sink.clone(), input),
        )
        .expect_err("connect refuses the link");
        assert_eq!(
            std::mem::discriminant(&judged),
            std::mem::discriminant(&placed),
            "{input}: {judged} / {placed}"
        );
    }
    assert_eq!(the_nodes_in(&compiler), nodes_before + 1);
    assert_eq!(the_links_in(&compiler), 0);
}

/// A port declaring one of several types answers the one that connects best: a Float into one
/// taking a String or a Float connects as it is.
///
/// Mental-revert: judging only the first type it lists would refuse a Float into a String.
#[test]
fn a_node_types_port_of_one_of_several_types_answers_the_one_that_connects_best() {
    register_the_node_types_port_fixtures();
    let compiler = Arc::new(Compiler::new());
    let float_source = a_node_of(&compiler, class_path(FLOAT_SOURCE));

    let verdict = the_verdict_into_a_node_types_input(
        &compiler,
        &float_source,
        "value",
        ONE_OF_SINK,
        "either",
    )
    .expect("a Float connects into a port taking a Float")
    .expect("two typed ends have a verdict");

    assert_eq!(verdict.outcome, PinLinkOutcome::Connects);
}

/// A port taking any type connects as the other end's type, and one whose type a setting
/// chooses connects the same way, warning that its type is found only once a node is placed.
///
/// Mental-revert: judging either by its family alone would answer no verdict, and so no
/// warning.
#[test]
fn a_node_types_port_of_any_type_or_one_found_once_placed_connects_as_the_other_ends_type() {
    register_the_node_types_port_fixtures();
    let compiler = Arc::new(Compiler::new());
    let float_source = a_node_of(&compiler, class_path(FLOAT_SOURCE));

    let any = the_verdict_into_a_node_types_input(
        &compiler,
        &float_source,
        "value",
        ANY_SINK,
        "anything",
    )
    .expect("any type takes a Float")
    .expect("two typed ends have a verdict");
    let chosen = the_verdict_into_a_node_types_input(
        &compiler,
        &float_source,
        "value",
        CHOSEN_TYPE_SINK,
        "chosen",
    )
    .expect("a chosen type is judged as a Float")
    .expect("two typed ends have a verdict");

    assert_eq!(any.outcome, PinLinkOutcome::Connects);
    assert!(any.warnings.is_empty(), "{:?}", any.warnings);
    assert_eq!(chosen.outcome, PinLinkOutcome::Connects);
    assert_eq!(
        chosen.warnings,
        vec![PinLinkWarning::TypeFoundOnlyOncePlaced]
    );
}

/// Judged from a placed input, a node type's output is the link's source: a Float output of a
/// node type into a placed Integer input connects for free.
#[test]
fn a_node_types_output_is_judged_into_a_placed_input() {
    register_the_node_types_port_fixtures();
    let compiler = Arc::new(Compiler::new());
    let number_sink = a_node_of(&compiler, class_path(NUMBER_SINK));

    let verdict = the_verdict_connect_takes_on_a_link_with_a_node_types_port(
        &compiler,
        &ALinkEndTheGraphHolds::Input(InputLinkPortRef::new(number_sink, "count")),
        &class_path(FLOAT_SOURCE),
        "value",
    )
    .expect("a Float is read as an Integer for free")
    .expect("two typed ends have a verdict");

    assert_eq!(
        verdict.outcome,
        PinLinkOutcome::ConnectsWithFreeConversion(FreePinConversion::BetweenNumbers)
    );
}

/// A node type the stream does not resolve is refused as unknown, and no node is left placed.
#[test]
fn a_node_type_the_stream_does_not_resolve_is_refused_as_unknown() {
    register_the_node_types_port_fixtures();
    let compiler = Arc::new(Compiler::new());
    let float_source = a_node_of(&compiler, class_path(FLOAT_SOURCE));

    let refused = the_verdict_into_a_node_types_input(
        &compiler,
        &float_source,
        "value",
        "VerdictCheckNoSuchType",
        "count",
    )
    .expect_err("no such node type");

    assert!(
        matches!(refused, Error::UnknownNodeType { .. }),
        "{refused:?}"
    );
    assert_eq!(the_nodes_in(&compiler), 1);
}
