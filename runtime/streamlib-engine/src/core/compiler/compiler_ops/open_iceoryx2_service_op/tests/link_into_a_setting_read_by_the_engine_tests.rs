// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link into a setting its node reads at setup is read by the engine, which holds each value
//! it carries for the node's rebuild, and closing the link stops the read.

use std::time::{Duration, Instant};

use serde_json::json;

use super::*;
use crate::core::descriptors::PinType;
use crate::core::graph::input_values::ValueAWireCarriedIntoASetting;
use crate::core::pin_value_packing::pack_pin_value;
use crate::core::test_support::{MockOutputOnlyNode, make_in1_a_setting_read_at_setup};

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

/// Every value waiting for its node's rebuild once one is, or none within `within`.
fn the_values_waiting_within(
    graph: &Graph,
    within: Duration,
) -> Vec<ValueAWireCarriedIntoASetting> {
    let deadline = Instant::now() + within;
    loop {
        let waiting = graph
            .values_wires_carry_into_settings()
            .take_each_value_waiting();
        if !waiting.is_empty() || Instant::now() >= deadline {
            return waiting;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The engine reads a link into a setting read at setup in its node's place: a node with no
/// instance at all, which reads nothing, has the value the wire carried held for its rebuild,
/// and once the link is closed a value its source writes is held for nothing.
///
/// Mental-revert: wiring the link's destination side as any input's waits on a node instance
/// the destination does not have, and no value is held.
#[test]
fn a_link_into_a_setting_read_at_setup_is_read_by_the_engine_until_it_is_closed() {
    let iceoryx2_node = Iceoryx2Node::for_this_test_process();
    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let (_, source_output, _) =
        attach_mock_instance::<MockOutputOnlyNode::Node>(&mut graph, &source_id);
    let source_output = source_output.expect("an output-only mock holds an output writer");
    let dest_id = add_mock_input_only(&mut graph);
    let dest_uuid = make_in1_a_setting_read_at_setup(&mut graph, &dest_id);
    let link_id = add_link_from_out1_to_in1(&mut graph, &source_id, &dest_id);

    open_iceoryx2_service(&mut graph, &link_id, &iceoryx2_node)
        .expect("a link into a setting read at setup wires with no node instance to read it");
    write_a_gain(&source_output, 0.25, 1).expect("the source's output is wired");

    assert_eq!(
        the_values_waiting_within(&graph, A_VALUE_IS_HANDED_OVER_WITHIN),
        vec![ValueAWireCarriedIntoASetting {
            node: dest_uuid,
            setting: "in1".to_owned(),
            value: json!(0.25),
            built_with_before: None,
        }]
    );

    close_iceoryx2_service(&mut graph, &link_id).expect("the link closes");
    let reader_left = graph
        .traversal()
        .e(&link_id)
        .first()
        .is_some_and(|link| link.has::<ReaderOfALinkIntoASettingComponent>());
    assert!(!reader_left, "closing the link takes its reader off");
    // The source's output may have no channel left to write to, which is a write gone nowhere.
    let _ = write_a_gain(&source_output, 0.75, 2);
    assert_eq!(
        the_values_waiting_within(&graph, Duration::from_millis(300)),
        Vec::new(),
        "a closed link's reader holds nothing more"
    );
}
