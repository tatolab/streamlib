// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A set naming one member of a bundle input, saved or live, changes that member alone; `graph`
//! renders each member alone, and each member's notification equals its row.

use parking_lot::RwLock;
use serde_json::{Value, json};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    BundleMemberValueReadOnTheWire, InputValueComesFromOnTheWire, InputValueReadOnTheWire,
};

use super::super::live_graph_edits::set_the_saved_value_and_hand_it_over;
use super::*;
use crate::core::compiler::compiler_ops::function_node_island::tests::{
    a_function_node_type, a_graph_resolving_these_function_node_types,
    a_helper_reader_before_its_setup, add_a_node, link,
};
use crate::core::compiler::compiler_ops::function_node_island::wire_a_link_from_a_function_node_island_into_its_reader;
use crate::core::compiler::compiler_ops::input_port_pin_values::tests::the_link_wiring_of;
use crate::core::descriptors::{BundleMember, FloatSize, NodeClassImportPath, PinDeclaration, PinType, PortDescriptor};
use crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery;
use crate::iceoryx2::Iceoryx2Node;

pub(super) const BUNDLE_READER: &str = "test_ops:bundle_reader";

fn a_member(name: &str, member_type: PinType, default_value: Option<Value>) -> BundleMember {
    BundleMember {
        name: name.to_owned(),
        member_type,
        meaning: None,
        default_value,
    }
}

fn float() -> PinType {
    PinType::Float {
        exact_size: Some(FloatSize::Float64),
    }
}

fn a_typed_input(name: &str, pin_type: PinType) -> PortDescriptor {
    PortDescriptor::new(name, "", true, PinDeclaration::exactly(pin_type))
    .with_delivery_profile("newest")
}

/// A graph resolving a function node type reading `reading`, a bundle of a Float `level` and a
/// String `label`, neither with a default, and a Float `gain` defaulting to 1; and `x`, a Float.
pub(super) fn a_graph_resolving_a_bundle_reader() -> Graph {
    let bundle_reader = a_function_node_type(BUNDLE_READER, &[], &["result"])
        .with_input(a_typed_input(
            "reading",
            PinType::Bundle {
                name: None,
                members: vec![
                    a_member("level", float(), None),
                    a_member("label", PinType::String {}, None),
                    a_member("gain", float(), Some(json!(1.0))),
                ],
            },
        ))
        .with_input(a_typed_input("x", float()));
    a_graph_resolving_these_function_node_types([bundle_reader])
}

/// A bundle reader holding the saved reading `{level: 0.2, label: "kitchen"}`.
pub(super) fn a_bundle_reader_with_a_saved_reading() -> (RwLock<Graph>, NodeRunId, Uuid) {
    let mut graph = a_graph_resolving_a_bundle_reader();
    let reader = add_a_node(&mut graph, BUNDLE_READER, Value::Null);
    let reader_uuid = graph.traversal().v(&reader).first().unwrap().node_uuid;
    let graph = RwLock::new(graph);
    set_the_saved_value_and_hand_it_over(
        &graph,
        &reader,
        "reading",
        None,
        Some(json!({ "level": 0.2, "label": "kitchen" })),
    )
    .unwrap();
    (graph, reader, reader_uuid)
}

pub(super) fn a_member_set(
    node: Uuid,
    input: &str,
    member: Option<&str>,
    value: Option<Value>,
) -> InputValueToSetLive {
    InputValueToSetLive {
        node,
        input: input.to_owned(),
        member: member.map(str::to_owned),
        value,
    }
}

pub(super) fn the_values_set_live(
    graph: &RwLock<Graph>,
    sets: &[InputValueToSetLive],
) -> std::result::Result<Option<InputValuesHeldLive>, LiveInputValueSetRefusal> {
    set_the_input_values_live_and_hand_each_node_its_own(
        graph,
        sets,
        PlaybackOfTheStreamSetOn::NotStopped {
            why_a_node_not_built_does_not_run: WhyANodeOfAStreamIsNotRunning::Preparing,
        },
    )
}

/// The `member_values` `graph` renders for input `reading` of the bundle reader.
pub(super) fn the_member_rows(graph: &Graph) -> Vec<BundleMemberValueReadOnTheWire> {
    let rendered =
        serde_json::to_value(graph.to_graph_response("this-machine".to_owned())).unwrap();
    let reading = rendered["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["type"] == BUNDLE_READER)
        .and_then(|node| {
            node["ports"]["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|port| port["name"] == "reading")
                .cloned()
        })
        .unwrap();
    assert!(reading.get("value").is_none(), "{reading}");
    assert!(reading.get("value_comes_from").is_none(), "{reading}");
    serde_json::from_value(reading["member_values"].clone()).unwrap()
}

fn a_row(
    member: &str,
    value: Value,
    value_comes_from: Option<InputValueComesFromOnTheWire>,
) -> BundleMemberValueReadOnTheWire {
    BundleMemberValueReadOnTheWire {
        member: member.to_owned(),
        value: Some(value),
        value_comes_from,
    }
}

/// The change announced for `row`, the row of one member of input `reading` of `node`.
pub(super) fn the_change_of_the_row(
    node: Uuid,
    row: &BundleMemberValueReadOnTheWire,
) -> RuntimeEvent {
    RuntimeEvent::InputValueDidChange {
        node,
        input: "reading".to_owned(),
        member: Some(row.member.clone()),
        value_read: Some(InputValueReadOnTheWire {
            value: row.value.clone(),
            value_comes_from: row.value_comes_from,
            member_values: None,
        }),
    }
}

/// One member set live reads live, one saved reads saved and one never set reads its default,
/// each row resolved alone, and the live set announces its member alone, equal to its row.
///
/// Mental-revert: holding the member's value as the whole live value renders the reading's
/// other members as missing and announces a change naming no member.
#[test]
fn a_member_set_live_changes_that_member_alone_and_announces_it_as_its_row() {
    let (graph, _reader, reader_uuid) = a_bundle_reader_with_a_saved_reading();

    let held_live = the_values_set_live(
        &graph,
        &[a_member_set(
            reader_uuid,
            "reading",
            Some("level"),
            Some(json!(1.0)),
        )],
    )
    .unwrap()
    .expect("a bundle input of a function node is set apart from the graph");

    let rows = the_member_rows(&graph.read());
    assert_eq!(
        rows,
        vec![
            a_row(
                "level",
                json!(1.0),
                Some(InputValueComesFromOnTheWire::Live)
            ),
            a_row(
                "label",
                json!("kitchen"),
                Some(InputValueComesFromOnTheWire::Saved)
            ),
            a_row(
                "gain",
                json!(1.0),
                Some(InputValueComesFromOnTheWire::Default)
            ),
        ]
    );
    assert_eq!(
        held_live.changes_announced,
        vec![the_change_of_the_row(reader_uuid, &rows[0])]
    );
    let graph = graph.read();
    let node = graph.node_by_uuid(reader_uuid).unwrap();
    assert_eq!(
        the_input_value_change(&graph, node, "reading", None),
        rows.iter()
            .map(|row| the_change_of_the_row(reader_uuid, row))
            .collect::<Vec<_>>(),
        "each member's notification equals its row"
    );
    assert_eq!(
        node.input_values["reading"],
        json!({ "level": 0.2, "label": "kitchen" }),
        "the saved reading is kept"
    );
    assert_eq!(
        graph.live_input_values().value_of(reader_uuid, "reading"),
        Some(json!({ "level": 1.0 }))
    );
}

/// A member saved changes that member of the saved bundle alone and drops that member's live
/// value alone, so another member set live still reads live.
///
/// Mental-revert: dropping the input's whole live value takes `level` back to its saved 0.2.
#[test]
fn a_member_saved_changes_that_member_and_keeps_every_other_member_set_live() {
    let (graph, reader, reader_uuid) = a_bundle_reader_with_a_saved_reading();
    the_values_set_live(
        &graph,
        &[
            a_member_set(reader_uuid, "reading", Some("level"), Some(json!(1.0))),
            a_member_set(reader_uuid, "reading", Some("label"), Some(json!("porch"))),
        ],
    )
    .unwrap()
    .expect("held live");

    let announced = set_the_saved_value_and_hand_it_over(
        &graph,
        &reader,
        "reading",
        Some("label"),
        Some(json!("hall")),
    )
    .unwrap();

    let rows = the_member_rows(&graph.read());
    assert_eq!(
        rows[..2],
        [
            a_row(
                "level",
                json!(1.0),
                Some(InputValueComesFromOnTheWire::Live)
            ),
            a_row(
                "label",
                json!("hall"),
                Some(InputValueComesFromOnTheWire::Saved)
            ),
        ]
    );
    assert_eq!(
        announced,
        vec![the_change_of_the_row(reader_uuid, &rows[1])]
    );
    let graph = graph.read();
    assert_eq!(
        graph.node_by_uuid(reader_uuid).unwrap().input_values["reading"],
        json!({ "level": 0.2, "label": "hall" })
    );
}

/// A set naming a member the bundle does not declare, one of a value the member's type does not
/// take, and one naming a member of an input that is no bundle, is refused, and nothing is set.
#[test]
fn a_member_set_the_bundle_does_not_take_is_refused() {
    let (graph, _reader, reader_uuid) = a_bundle_reader_with_a_saved_reading();

    for (set, words_named) in [
        (
            a_member_set(reader_uuid, "reading", Some("depth"), Some(json!(1.0))),
            "`level`, `label`, `gain`",
        ),
        (
            a_member_set(reader_uuid, "reading", Some("level"), Some(json!("loud"))),
            "member `level`",
        ),
        (
            a_member_set(reader_uuid, "x", Some("level"), Some(json!(1.0))),
            "is no bundle",
        ),
    ] {
        let refused = the_values_set_live(&graph, &[set]);
        let Err(LiveInputValueSetRefusal::TheInputDoesNotTakeIt {
            set_index: 0,
            refusal,
        }) = refused
        else {
            panic!("refused, by its index: {:?}", refused.err());
        };
        assert!(refusal.words.contains(words_named), "{}", refusal.words);
    }
    assert_eq!(
        graph
            .read()
            .live_input_values()
            .value_of(reader_uuid, "reading"),
        None
    );
}

/// Two members of one input set in one live set are both taken; one member named twice is
/// refused at the second, as is the whole input set beside one of its members.
#[test]
fn one_live_set_takes_each_member_once_and_refuses_one_set_twice() {
    let (graph, _reader, reader_uuid) = a_bundle_reader_with_a_saved_reading();

    for sets in [
        [
            a_member_set(reader_uuid, "reading", Some("level"), Some(json!(1.0))),
            a_member_set(reader_uuid, "reading", Some("level"), Some(json!(2.0))),
        ],
        [
            a_member_set(reader_uuid, "reading", Some("level"), Some(json!(1.0))),
            a_member_set(
                reader_uuid,
                "reading",
                None,
                Some(json!({ "level": 0.5, "label": "hall" })),
            ),
        ],
    ] {
        assert!(matches!(
            the_values_set_live(&graph, &sets),
            Err(LiveInputValueSetRefusal::TheInputIsSetTwice { set_index: 1, .. })
        ));
    }
}

/// A saved bundle may leave out only a member with a default: taking `level`, which declares
/// none, off the saved reading is refused, and taking `gain` off is not.
#[test]
fn taking_a_member_with_no_default_off_the_saved_bundle_is_refused() {
    let graph = a_graph_resolving_a_bundle_reader();
    let node_type = NodeClassImportPath::new(BUNDLE_READER).unwrap();
    let check = |member: &str| {
        let after_the_set = streamlib_project_repository::input_values_after_the_set(
            &json!({ "reading": { "level": 0.2, "label": "kitchen", "gain": 2.0 } })
                .as_object()
                .unwrap()
                .clone(),
            "reading",
            Some(member),
            None,
        );
        refuse_an_input_value_its_input_does_not_take(
            graph.node_types_this_stream_resolves(),
            &NodeAnInputValueIsSetOn {
                name: "reader",
                node_type: &node_type,
                input_values_after_the_set: &after_the_set,
            },
            "reading",
            Some(member),
            None,
        )
    };

    let refusal = check("level").expect_err("`level` declares no default");
    assert!(
        refusal.words.contains("declares no default"),
        "{}",
        refusal.words
    );
    check("gain").expect("`gain` defaults to 1");
}

/// A member the input's default holds may be taken off the saved bundle, which then reads it
/// from that default, though the member declares no default of its own.
///
/// Mental-revert: judging the saved object alone refuses taking `label` off.
#[test]
fn a_member_the_inputs_default_holds_may_be_taken_off_the_saved_bundle() {
    const LABELLED_READER: &str = "test_ops:labelled_reader";
    let labelled_reader = a_function_node_type(LABELLED_READER, &[], &["result"]).with_input(
        a_typed_input(
            "reading",
            PinType::Bundle {
                name: None,
                members: vec![
                    a_member("level", float(), None),
                    a_member("label", PinType::String {}, None),
                ],
            },
        )
        .with_pin_default(json!({ "label": "unnamed" })),
    );
    let graph = a_graph_resolving_these_function_node_types([labelled_reader]);
    let node_type = NodeClassImportPath::new(LABELLED_READER).unwrap();
    let check = |member: &str| {
        let after_the_set = streamlib_project_repository::input_values_after_the_set(
            json!({ "reading": { "level": 0.2, "label": "kitchen" } })
                .as_object()
                .unwrap(),
            "reading",
            Some(member),
            None,
        );
        refuse_an_input_value_its_input_does_not_take(
            graph.node_types_this_stream_resolves(),
            &NodeAnInputValueIsSetOn {
                name: "reader",
                node_type: &node_type,
                input_values_after_the_set: &after_the_set,
            },
            "reading",
            Some(member),
            None,
        )
    };

    check("label").expect("the input's default holds `label`");
    check("level").expect_err("neither `level` nor the input's default gives it a default");
}

/// One live transaction may set every member a bundle needs: each set is checked against the
/// bundle the whole transaction leaves, not against the live value before it.
///
/// Mental-revert: checking the `level` set alone refuses a bundle with no `label`.
#[test]
fn one_live_transaction_setting_every_member_a_bundle_needs_is_taken() {
    let mut graph = a_graph_resolving_a_bundle_reader();
    let reader = add_a_node(&mut graph, BUNDLE_READER, Value::Null);
    let reader_uuid = graph.traversal().v(&reader).first().unwrap().node_uuid;
    let graph = RwLock::new(graph);

    the_values_set_live(
        &graph,
        &[
            a_member_set(reader_uuid, "reading", Some("level"), Some(json!(0.5))),
            a_member_set(reader_uuid, "reading", Some("label"), Some(json!("hall"))),
        ],
    )
    .unwrap()
    .expect("a bundle input of a function node is set apart from the graph");

    assert_eq!(
        graph
            .read()
            .live_input_values()
            .value_of(reader_uuid, "reading"),
        Some(json!({ "level": 0.5, "label": "hall" }))
    );
}

/// A node reading the bundle inside an island is handed both: the member set live over the
/// saved one, every other member as saved.
///
/// Mental-revert: the live value laid over the saved one whole hands the island a reading with
/// no `label`.
#[test]
fn the_island_reading_a_bundle_is_handed_the_member_set_live_beside_the_saved_ones() {
    let delivery = RecordingOutOfProcessFarSideLinkDelivery::default();
    let mut graph = a_graph_resolving_a_bundle_reader();
    let bundle_reader = add_a_node(&mut graph, BUNDLE_READER, Value::Null);
    let helper_reader = a_helper_reader_before_its_setup(&mut graph);
    the_link_wiring_of(&graph, &helper_reader)
        .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), delivery.clone())
        .unwrap();
    let into_the_reader = link(
        &mut graph,
        (&bundle_reader, "result"),
        (&helper_reader, "in1"),
    );
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();
    let reader_uuid = graph
        .traversal()
        .v(&bundle_reader)
        .first()
        .unwrap()
        .node_uuid;
    let graph = RwLock::new(graph);
    set_the_saved_value_and_hand_it_over(
        &graph,
        &bundle_reader,
        "reading",
        None,
        Some(json!({ "level": 0.2, "label": "kitchen" })),
    )
    .unwrap();

    the_values_set_live(
        &graph,
        &[a_member_set(
            reader_uuid,
            "reading",
            Some("level"),
            Some(json!(1.0)),
        )],
    )
    .unwrap()
    .expect("held live");

    let handed = delivery
        .late_fed_function_node_islands
        .lock()
        .last()
        .map(|island| island["nodes"][0]["pins"]["reading"].clone());
    assert_eq!(handed, Some(json!({ "level": 1.0, "label": "kitchen" })));
}
