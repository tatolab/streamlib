// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use serde_json::json;

use super::*;
use crate::core::compiler::compiler_ops::function_node_island::tests::link;
use crate::core::descriptors::{
    NodeClassImportPath, NodeClassShortName, NodeDescriptor, PinDeclaration, PinType,
    PortDescriptor,
};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{
    GraphNodeWithComponents, NodeInstanceComponent, OutOfProcessLinkWiringComponent,
};
use crate::core::nodes::{
    HowANodeOfADescribedTypeRuns, NodeInstance, NodeSpec, OutOfProcessLinkWiringEnvelope,
};
use crate::core::test_support::{MockInputOnlyNode, RecordingOutOfProcessFarSideLinkDelivery};
use crate::iceoryx2::InputMailboxesInner;

const BUTTON_NODE_TYPE: &str = "test_nodes:Button";

/// A graph resolving a process node type with a Trigger input `go`, a Channels input `level`
/// and a Trigger output `fired`.
fn a_graph_resolving_a_node_type_with_a_trigger_input() -> Graph {
    let trigger = || PinDeclaration::exactly(PinType::Trigger {});
    let descriptor = NodeDescriptor::new(
        NodeClassShortName::new("Button").unwrap(),
        NodeClassImportPath::new(BUTTON_NODE_TYPE).unwrap(),
        "a process node type with a Trigger input, for a test",
    )
    .with_input(PortDescriptor::new("go", "", true, trigger()))
    .with_input(PortDescriptor::new(
        "level",
        "",
        true,
        PinDeclaration::exactly(PinType::Float { exact_size: None }),
    ))
    .with_output(PortDescriptor::new("fired", "", true, trigger()));
    let node_types = NodeTypesOneStreamResolves::new();
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            descriptor,
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(Box::new(|node| {
                Err(Error::Configuration(format!(
                    "`{}` builds no node in this test",
                    node.id
                )))
            })),
        )
        .unwrap();
    crate::core::test_support::ensure_test_mocks_registered();
    Graph::new_resolving_node_types_through(Arc::new(node_types))
}

fn a_button(graph: &mut Graph) -> NodeRunId {
    graph
        .traversal_mut()
        .add_v(NodeSpec::new(
            NodeClassImportPath::new(BUTTON_NODE_TYPE).unwrap(),
            serde_json::Value::Null,
        ))
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone()
}

/// A button in a helper past its setup command, handing what it is handed to the delivery
/// answered.
fn a_button_in_a_helper_past_its_setup(
    graph: &mut Graph,
) -> (NodeRunId, RecordingOutOfProcessFarSideLinkDelivery) {
    let button = a_button(graph);
    let link_wiring = Arc::new(OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
        ProcessExecution::Reactive,
    ));
    let delivery = RecordingOutOfProcessFarSideLinkDelivery::default();
    link_wiring
        .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), delivery.clone())
        .unwrap();
    graph
        .traversal_mut()
        .v(&button)
        .first_mut()
        .unwrap()
        .insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(link_wiring));
    (button, delivery)
}

/// A button built in this process, and the input mailboxes it reads.
fn a_button_in_this_process(graph: &mut Graph) -> (NodeRunId, Arc<InputMailboxesInner>) {
    let button = a_button(graph);
    let mut instance = NodeInstance::new(Box::new(
        <MockInputOnlyNode::Node as crate::core::GeneratedNode>::from_config(Default::default())
            .expect("the mock constructs from its default config"),
    ));
    instance
        .install_iceoryx2_resources()
        .expect("the mock accepts its iceoryx2 resources");
    let input_mailboxes = instance
        .iceoryx2_input_mailboxes_inner()
        .expect("an input-only mock holds input mailboxes");
    graph
        .traversal_mut()
        .v(&button)
        .first_mut()
        .unwrap()
        .insert_component_without_rendering_it(NodeInstanceComponent(Arc::new(Mutex::new(
            instance,
        ))));
    (button, input_mailboxes)
}

fn the_uuid_of(graph: &Graph, node_run_id: &NodeRunId) -> Uuid {
    graph.traversal().v(node_run_id).first().unwrap().node_uuid
}

fn a_fire(node: Uuid) -> InputValueToSetLive {
    InputValueToSetLive {
        member: None,
        node,
        input: "go".to_owned(),
        value: None,
    }
}

fn while_playing() -> PlaybackOfTheStreamSetOn {
    PlaybackOfTheStreamSetOn::NotStopped {
        why_a_node_not_built_does_not_run: WhyANodeOfAStreamIsNotRunning::Preparing,
    }
}

fn the_fires(
    graph: &RwLock<Graph>,
    sets: &[InputValueToSetLive],
    playback: PlaybackOfTheStreamSetOn,
) -> std::result::Result<Option<InputValuesHeldLive>, LiveInputValueSetRefusal> {
    set_the_input_values_live_and_hand_each_node_its_own(graph, sets, playback)
}

/// Ten fires of a node in this process queue ten Trigger messages, in order, beside a value set
/// in the same transaction; the fires hold no live value and announce nothing.
///
/// Mental-revert: treating a fire as a value set takes `go`'s live value off once, and the
/// second fire is refused as an input set twice.
#[test]
fn each_fire_of_a_node_in_this_process_queues_one_message_and_holds_no_value() {
    let mut graph = a_graph_resolving_a_node_type_with_a_trigger_input();
    let (button, input_mailboxes) = a_button_in_this_process(&mut graph);
    let node = the_uuid_of(&graph, &button);
    let graph = RwLock::new(graph);
    let mut sets: Vec<InputValueToSetLive> = (0..10).map(|_| a_fire(node)).collect();
    sets.insert(
        3,
        InputValueToSetLive {
            member: None,
            node,
            input: "level".to_owned(),
            value: Some(json!(0.25)),
        },
    );

    let held = the_fires(&graph, &sets, while_playing())
        .unwrap()
        .expect("fires and a value are set apart from the graph");

    assert_eq!(held.made.len(), 11, "one made for each set, in order");
    assert_eq!(held.made[0].live_value_before, None);
    assert_eq!(
        held.changes_announced.len(),
        1,
        "only the value set is announced"
    );
    let graph = graph.read();
    assert_eq!(graph.live_input_values().value_of(node, "go"), None);
    assert_eq!(
        graph.live_input_values().value_of(node, "level"),
        Some(json!(0.25))
    );
    let mut stamps = Vec::new();
    while let Some(fire) = input_mailboxes.read_raw("go").unwrap() {
        assert_eq!(&*fire.payload, &[0x80]);
        stamps.push(fire.timestamp_ns);
    }
    assert_eq!(stamps.len(), 10, "ten fires queue ten messages");
    assert!(
        stamps.windows(2).all(|pair| pair[0] <= pair[1]),
        "in the order fired: {stamps:?}"
    );
}

/// A fire of a node in a helper reaches it through `set_input_values`, each input fired named
/// once for each fire, and no value is recorded for it.
#[test]
fn the_fires_of_a_node_in_a_helper_are_handed_to_it_in_order() {
    let mut graph = a_graph_resolving_a_node_type_with_a_trigger_input();
    let (button, delivery) = a_button_in_a_helper_past_its_setup(&mut graph);
    let node = the_uuid_of(&graph, &button);
    let graph = RwLock::new(graph);

    for _ in 0..10 {
        the_fires(&graph, &[a_fire(node)], while_playing())
            .unwrap()
            .expect("set apart from the graph");
    }
    the_fires(&graph, &[a_fire(node), a_fire(node)], while_playing())
        .unwrap()
        .expect("an input fired twice in one transaction fires twice");

    let fires_handed_over = delivery.fires_handed_over.lock().clone();
    assert_eq!(fires_handed_over.len(), 11);
    assert!(fires_handed_over[..10].iter().all(|fired| fired == &["go"]));
    assert_eq!(fires_handed_over[10], ["go", "go"]);
    assert!(
        delivery.input_values_handed_over.lock().is_empty(),
        "a fire changes no value its helper reads"
    );
}

#[test]
fn a_fire_with_a_value_on_a_linked_trigger_or_of_a_node_that_does_not_run_is_refused() {
    let mut graph = a_graph_resolving_a_node_type_with_a_trigger_input();
    let (button, _input_mailboxes) = a_button_in_this_process(&mut graph);
    let (linked_button, _) = a_button_in_this_process(&mut graph);
    let (source, _) = a_button_in_this_process(&mut graph);
    link(&mut graph, (&source, "fired"), (&linked_button, "go"));
    let not_built = a_button(&mut graph);
    let node = the_uuid_of(&graph, &button);
    let linked = the_uuid_of(&graph, &linked_button);
    let not_built = the_uuid_of(&graph, &not_built);
    let graph = RwLock::new(graph);

    let with_a_value = the_fires(
        &graph,
        &[InputValueToSetLive {
            value: Some(json!(true)),
            ..a_fire(node)
        }],
        while_playing(),
    );
    match with_a_value {
        Err(LiveInputValueSetRefusal::TheInputDoesNotTakeIt { set_index, refusal }) => {
            assert_eq!(set_index, 0);
            assert_eq!(refusal.code, InputValueRefusalCode::InputValueNotValid);
            assert!(refusal.words.contains("is a Trigger"), "{}", refusal.words);
        }
        other => panic!("a fire with a value is refused: {:?}", other.err()),
    }

    match the_fires(&graph, &[a_fire(node), a_fire(linked)], while_playing()) {
        Err(LiveInputValueSetRefusal::TheInputDoesNotTakeIt { set_index, refusal }) => {
            assert_eq!(set_index, 1);
            assert!(refusal.words.contains("a link feeds"), "{}", refusal.words);
        }
        other => panic!("a fire of a linked Trigger is refused: {:?}", other.err()),
    }

    match the_fires(&graph, &[a_fire(node)], PlaybackOfTheStreamSetOn::Stopped) {
        Err(LiveInputValueSetRefusal::TheNodeDoesNotRun {
            set_index: 0,
            refusal:
                Error::NodeOfAStreamIsNotRunning {
                    not_running_because,
                    ..
                },
        }) => assert_eq!(
            not_running_because,
            WhyANodeOfAStreamIsNotRunning::PlaybackStopped
        ),
        other => panic!("a fire on a stopped stream is refused: {:?}", other.err()),
    }

    match the_fires(&graph, &[a_fire(not_built)], while_playing()) {
        Err(LiveInputValueSetRefusal::TheNodeDoesNotRun {
            refusal:
                Error::NodeOfAStreamIsNotRunning {
                    not_running_because,
                    ..
                },
            ..
        }) => assert_eq!(
            not_running_because,
            WhyANodeOfAStreamIsNotRunning::Preparing
        ),
        other => panic!("a fire of a node not built is refused: {:?}", other.err()),
    }
}

/// A fire naming a member of a Trigger input is refused before anything is queued, naming the
/// input and the member: a Trigger is no bundle, so it holds no member to fire.
///
/// Mental-revert: firing without checking the member queues the whole input's fire.
#[test]
fn a_fire_naming_a_member_is_refused_and_queues_nothing() {
    let mut graph = a_graph_resolving_a_node_type_with_a_trigger_input();
    let (button, input_mailboxes) = a_button_in_this_process(&mut graph);
    let node = the_uuid_of(&graph, &button);
    let graph = RwLock::new(graph);

    let Err(refusal) = the_fires(
        &graph,
        &[InputValueToSetLive {
            member: Some("level".to_owned()),
            ..a_fire(node)
        }],
        while_playing(),
    ) else {
        panic!("a Trigger holds no member to fire");
    };

    let LiveInputValueSetRefusal::TheInputDoesNotTakeIt { set_index, refusal } = refusal else {
        panic!("refused as a fire its input does not take: {refusal:?}");
    };
    assert_eq!(set_index, 0);
    assert_eq!(refusal.code, InputValueRefusalCode::InputValueNotValid);
    assert!(
        refusal.words.contains("no member `level` to fire"),
        "{}",
        refusal.words
    );
    assert!(
        !matches!(input_mailboxes.read_raw("go"), Ok(Some(_))),
        "no fire is queued"
    );
}
