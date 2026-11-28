// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link into a setting is read by the engine, which holds each value it carries as what the
//! setting reads and leaves the node to be handed it, and closing the link stops the read.

use std::time::{Duration, Instant};

use serde_json::json;

use super::*;
use crate::core::descriptors::PinType;
use crate::core::pin_value_packing::pack_pin_value;
use crate::core::test_support::{MockOutputOnlyNode, make_in1_an_input_handed_to_its_node};

/// The longest a test waits for the engine's reader to hand a value over.
const A_VALUE_IS_HANDED_OVER_WITHIN: Duration = Duration::from_secs(5);

fn gain() -> PinType {
    PinType::Float { exact_size: None }
}

fn write_a_gain(
    source_output: &crate::iceoryx2::OutputWriterInner,
    gain_value: f64,
    at: i64,
) -> Result<()> {
    let packed = pack_pin_value(&gain(), &json!(gain_value), 0).expect("a Float packs");
    source_output.write_bytes_described_by(
        "out1",
        &packed.payload,
        at,
        MessageGroupBoundary::StartsGroup,
        packed.payload_descriptor,
    )
}

/// Each node left to be handed what a wire into its settings carried once one is, or none
/// within `within`.
fn the_nodes_waiting_within(
    graph: &Graph,
    within: Duration,
) -> std::collections::BTreeSet<uuid::Uuid> {
    let deadline = Instant::now() + within;
    loop {
        let waiting = graph
            .values_wires_carry_into_settings()
            .take_each_node_waiting_for_its_values();
        if !waiting.is_empty() || Instant::now() >= deadline {
            return waiting;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The engine reads a link into a setting in its node's place: a node with no instance at all,
/// which reads nothing, has the value the wire carried held as what its setting reads, and once
/// the link is closed a value its source writes is held for nothing.
///
/// Mental-revert: wiring the link's destination side as any input's waits on a node instance
/// the destination does not have, and no value is held.
#[test]
fn a_link_into_an_input_handed_to_its_node_is_read_by_the_engine_until_it_is_closed() {
    let iceoryx2_node = Iceoryx2Node::for_this_test_process();
    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let (_, source_output, _) =
        attach_mock_instance::<MockOutputOnlyNode::Node>(&mut graph, &source_id);
    let source_output = source_output.expect("an output-only mock holds an output writer");
    let dest_id = add_mock_input_only(&mut graph);
    let dest_uuid = make_in1_an_input_handed_to_its_node(&mut graph, &dest_id);
    let link_id = add_link_from_out1_to_in1(&mut graph, &source_id, &dest_id);

    open_iceoryx2_service(&mut graph, &link_id, &iceoryx2_node)
        .expect("a link into an input handed to its node wires with no node instance to read it");
    write_a_gain(&source_output, 0.25, 1).expect("the source's output is wired");

    assert_eq!(
        the_nodes_waiting_within(&graph, A_VALUE_IS_HANDED_OVER_WITHIN),
        std::collections::BTreeSet::from([dest_uuid])
    );
    assert_eq!(
        graph
            .values_wires_carry_into_settings()
            .the_value_its_wire_carried(dest_uuid, "in1"),
        Some(json!(0.25))
    );

    close_iceoryx2_service(&mut graph, &link_id).expect("the link closes");
    the_engine_reads_no_more_of(&graph, &link_id, &source_output);
}

/// Once link `link_id` is closed, its reader is gone and a value its source writes is held for
/// nothing.
fn the_engine_reads_no_more_of(
    graph: &Graph,
    link_id: &LinkUniqueId,
    source_output: &crate::iceoryx2::OutputWriterInner,
) {
    let reader_left = graph
        .traversal()
        .e(link_id)
        .first()
        .is_some_and(|link| link.has::<ReaderOfALinkIntoASettingComponent>());
    assert!(!reader_left, "closing the link takes its reader off");
    // The source's output may have no channel left to write to, which is a write gone nowhere.
    let _ = write_a_gain(source_output, 0.75, 2);
    assert_eq!(
        the_nodes_waiting_within(graph, Duration::from_millis(300)),
        std::collections::BTreeSet::<uuid::Uuid>::new(),
        "a closed link's reader holds nothing more"
    );
}
