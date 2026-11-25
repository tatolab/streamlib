// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link between a placed end and a port of a node type no node has yet is judged as `connect`
//! judges it once a node of the type is placed: by what the port declares, logging nothing a
//! commit reads and leaving no node in the graph.

use super::super::{
    ALinkEndTheGraphHolds, ANodeTypesPortToJudge,
    the_verdicts_connect_takes_on_links_with_node_types_ports,
};
use super::*;
use crate::core::PortDirection;
use crate::core::descriptors::{
    DeclaredPinType, PinAnnotations, PinFlow, PinLinkVerdict, PinLinkWarning,
};
use crate::core::graph::Graph;

const ONE_OF_SINK: &str = "VerdictCheckOneOfSink";
const ANY_SINK: &str = "VerdictCheckAnySink";
const CHOSEN_TYPE_SINK: &str = "VerdictCheckChosenTypeSink";

/// A port declaring `pin_type`, read as a value.
fn a_port_declaring(name: &str, pin_type: DeclaredPinType) -> PortDescriptor {
    PortDescriptor::new(
        name,
        name,
        true,
        PinDeclaration {
            pin_type,
            flow: PinFlow::Value,
            annotations: PinAnnotations::default(),
            former_names: Vec::new(),
            may_be_unset: false,
        },
    )
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

/// Port `port` of `direction` of `node_type`, as `graph`'s stream describes the type.
fn the_port_to_judge(
    graph: &Graph,
    node_type: &str,
    port: &str,
    direction: PortDirection,
) -> ANodeTypesPortToJudge {
    let node_types = graph.node_types_this_stream_resolves();
    let descriptor = node_types
        .descriptor(&class_path(node_type))
        .expect("the fixture's node type is registered");
    let ports_of_its_direction = match direction {
        PortDirection::Input => descriptor.inputs,
        PortDirection::Output => descriptor.outputs,
    };
    ANodeTypesPortToJudge::of(
        descriptor.node_class_import_path,
        ports_of_its_direction.into(),
        port,
        None,
    )
}

/// The verdict on the link between `held` and port `port` of `node_type`, judged alone; the
/// judgement logs nothing a commit reads.
fn the_verdict_on_a_node_types_port(
    compiler: &Arc<Compiler>,
    held: ALinkEndTheGraphHolds,
    node_type: &str,
    port: &str,
) -> Result<Option<PinLinkVerdict>> {
    let direction = match held {
        ALinkEndTheGraphHolds::Output(_) => PortDirection::Input,
        ALinkEndTheGraphHolds::Input(_) => PortDirection::Output,
    };
    let to_judge =
        compiler.scope(|graph, _tx| the_port_to_judge(graph, node_type, port, direction));
    let mut verdicts =
        the_verdicts_connect_takes_on_links_with_node_types_ports(compiler, held, vec![to_judge])
            .expect("the held end's port name casts");
    assert!(
        compiler.logged_pending_operations().is_empty(),
        "the judgement logged {:?}",
        compiler.logged_pending_operations()
    );
    assert_eq!(verdicts.len(), 1);
    verdicts.remove(0)
}

/// The verdict on a link from the output `port` of `source` into `input` of `node_type`.
fn the_verdict_into_a_node_types_input(
    compiler: &Arc<Compiler>,
    source: &NodeRunId,
    port: &str,
    node_type: &str,
    input: &str,
) -> Result<Option<PinLinkVerdict>> {
    the_verdict_on_a_node_types_port(
        compiler,
        ALinkEndTheGraphHolds::Output(OutputLinkPortRef::new(source.clone(), port)),
        node_type,
        input,
    )
}

/// A node type's exactly typed port is judged as a placed node of the type is: a Float into
/// its Integer input connects for free, into its String input, and a stream of Floats into its
/// setting, it is refused in connect's very words, and the graph holds no more nodes and links after each check than
/// before.
///
/// Mental-revert: leaving the held node in the graph would count it.
#[test]
fn a_node_types_typed_port_is_judged_as_a_placed_node_of_the_type_is() {
    register_the_node_types_port_fixtures();
    let compiler = Arc::new(Compiler::new());
    let float_source = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let float_stream_source = a_node_of(&compiler, class_path(FLOAT_STREAM_SOURCE));
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
    for (source, input) in [(&float_source, "label"), (&float_stream_source, "gain")] {
        let judged =
            the_verdict_into_a_node_types_input(&compiler, source, "value", NUMBER_SINK, input)
                .expect_err("connect refuses the link");
        let placed = the_verdict_connect_takes_on_a_link(
            &compiler,
            OutputLinkPortRef::new(source.clone(), "value"),
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

    let verdict = the_verdict_on_a_node_types_port(
        &compiler,
        ALinkEndTheGraphHolds::Input(InputLinkPortRef::new(number_sink, "count")),
        FLOAT_SOURCE,
        "value",
    )
    .expect("a Float is read as an Integer for free")
    .expect("two typed ends have a verdict");

    assert_eq!(
        verdict.outcome,
        PinLinkOutcome::ConnectsWithFreeConversion(FreePinConversion::BetweenNumbers)
    );
}

/// A port the node type does not declare is refused as not found on the type, naming the type,
/// and no node is left placed.
///
/// Mental-revert: naming the node held for the judgement would name a node no caller placed.
#[test]
fn a_port_the_node_type_does_not_declare_is_refused_as_not_found_on_the_type() {
    register_the_node_types_port_fixtures();
    let compiler = Arc::new(Compiler::new());
    let float_source = a_node_of(&compiler, class_path(FLOAT_SOURCE));

    let refused = the_verdict_into_a_node_types_input(
        &compiler,
        &float_source,
        "value",
        NUMBER_SINK,
        "nothing",
    )
    .expect_err("no such port");

    assert!(
        matches!(&refused, Error::NodeTypePortNotFound { node_type, port_name, .. }
            if *node_type == class_path(NUMBER_SINK) && port_name == "nothing"),
        "{refused:?}"
    );
    assert_eq!(the_nodes_in(&compiler), 1);
}

/// Several node types' ports are judged under one lock, each answered in its place, and none of
/// their nodes is left placed.
#[test]
fn several_node_types_ports_are_each_answered_in_their_order() {
    register_the_node_types_port_fixtures();
    let compiler = Arc::new(Compiler::new());
    let float_source = a_node_of(&compiler, class_path(FLOAT_SOURCE));
    let to_judge = compiler.scope(|graph, _tx| {
        Vec::from(
            [
                (NUMBER_SINK, "label"),
                (ANY_SINK, "anything"),
                (NUMBER_SINK, "count"),
            ]
            .map(|(node_type, port)| {
                the_port_to_judge(graph, node_type, port, PortDirection::Input)
            }),
        )
    });

    let verdicts = the_verdicts_connect_takes_on_links_with_node_types_ports(
        &compiler,
        ALinkEndTheGraphHolds::Output(OutputLinkPortRef::new(float_source, "value")),
        to_judge,
    )
    .expect("the held end's port name casts");

    assert!(verdicts[0].is_err(), "{:?}", verdicts[0]);
    assert_eq!(
        verdicts[1]
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map(|verdict| &verdict.outcome),
        Some(&PinLinkOutcome::Connects)
    );
    assert_eq!(
        verdicts[2]
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map(|verdict| &verdict.outcome),
        Some(&PinLinkOutcome::ConnectsWithFreeConversion(
            FreePinConversion::BetweenNumbers
        ))
    );
    assert!(compiler.logged_pending_operations().is_empty());
    assert_eq!(the_nodes_in(&compiler), 1);
}
