// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Windowed audio ports: the ring depth their channel holds and what a stalled windowed reader is told it lost.

use super::*;

/// The depth the channel a wired link holds open was created at.
fn creation_depth_of_the_channel_held_by(graph: &Graph, link_id: &LinkUniqueId) -> usize {
    graph
        .traversal()
        .e(link_id)
        .first()
        .expect("the link is in the graph")
        .get::<Iceoryx2ServicesHeldOpenForLinkComponent>()
        .expect("a wired link holds its services")
        .channel_data_service
        .channel_service_creation_depth()
}

/// Write `frames` of mono f32 audio at 16 kHz on `source_output`'s `out1`
/// as one Channels block, its first sample stamped at
/// `first_sample_timestamp_ns`.
fn write_a_mono_16k_audio_block_on_out1(
    source_output: &crate::iceoryx2::OutputWriterInner,
    frames: usize,
    first_sample_timestamp_ns: i64,
) -> Result<()> {
    let (payload_descriptor, planar_payload) = crate::iceoryx2::channels_block_of_one_window(
        &vec![0.25; frames],
        16_000,
        1,
        frames as u32,
        crate::iceoryx2::ChannelsBlockSampleType::F32,
        0,
    );
    source_output.write_bytes_described_by(
        "out1",
        &planar_payload,
        first_sample_timestamp_ns,
        MessageGroupBoundary::StartsGroup,
        payload_descriptor,
    )
}

/// An app-process source whose `out1` is linked, unwired, to an app-process
/// `newest` consumer. Hands back the source's id and the link.
fn add_a_source_linked_to_a_plain_consumer(graph: &mut Graph) -> (String, LinkUniqueId) {
    use crate::core::test_support::{MockInputOnlyNode, MockOutputOnlyNode};

    let source_id = add_mock_output_only(graph);
    attach_mock_instance::<MockOutputOnlyNode::Node>(graph, &source_id);
    let plain_consumer_id = add_mock_input_only(graph);
    attach_mock_instance::<MockInputOnlyNode::Node>(graph, &plain_consumer_id);
    let plain_link = add_link_from_out1_to_in1(graph, &source_id, &plain_consumer_id);
    (source_id, plain_link)
}

/// An exposed port no link reads, its channel opened first by the runtime's
/// publisher, still takes a windowed reader a live graph change connects
/// later: the publisher opens the channel as deep as a windowed ring.
///
/// Fail-without-fix: open it at the ordered depth and the windowed link is
/// refused as onto a channel created shallower than its ring.
#[test]
fn a_windowed_reader_connected_after_the_publisher_opened_the_channel_fits_it() {
    use crate::core::test_support::MockOutputOnlyNode;

    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    attach_mock_instance::<MockOutputOnlyNode::Node>(&mut graph, &source_id);
    let _held = open_the_source_side_for_a_reader_on_the_reserved_slot(
        &mut graph,
        &OutputLinkPortRef::new(&source_id, "out1"),
        &Iceoryx2Node::for_this_test_process(),
    )
    .expect("the publisher opens an unlinked port");
    assert_eq!(
        creation_depth_a_helper_opening_out1_finds(&source_id),
        WINDOWED_PORT_SUBSCRIBER_RING_DEPTH
    );

    let windowed = add_a_windowed_consumer_linked_from_out1(&mut graph, &source_id);
    open_iceoryx2_service(
        &mut graph,
        &windowed.link_id,
        &Iceoryx2Node::for_this_test_process(),
    )
    .expect("a windowed reader connected later fits the publisher's channel");
}

/// The publisher joins a channel a link already opened at the depth it was
/// opened at, and is never refused for it.
#[test]
fn the_publisher_joins_a_channel_a_link_opened_at_its_depth() {
    let mut graph = Graph::new();
    let (source_id, plain_link) = add_a_source_linked_to_a_plain_consumer(&mut graph);
    open_iceoryx2_service(
        &mut graph,
        &plain_link,
        &Iceoryx2Node::for_this_test_process(),
    )
    .expect("the plain link wires");
    let depth_the_link_opened_it_at = creation_depth_of_the_channel_held_by(&graph, &plain_link);

    let held = open_the_source_side_for_a_reader_on_the_reserved_slot(
        &mut graph,
        &OutputLinkPortRef::new(&source_id, "out1"),
        &Iceoryx2Node::for_this_test_process(),
    )
    .expect("the viewer joins a channel a link opened");
    assert_eq!(
        creation_depth_of_the_channel_held_by(&graph, &plain_link),
        depth_the_link_opened_it_at
    );
    drop(held);
}

/// A port the publisher holds keeps publishing once a live graph change
/// disconnects its last link, and lets its channel go once the publisher
/// does.
#[test]
fn a_port_the_publisher_holds_keeps_its_channel_after_its_last_link_is_disconnected() {
    use crate::core::test_support::{MockInputOnlyNode, MockOutputOnlyNode};

    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let (_, source_output, _) =
        attach_mock_instance::<MockOutputOnlyNode::Node>(&mut graph, &source_id);
    let source_output = source_output.expect("an output-only mock holds an output writer");
    let consumer_id = add_mock_input_only(&mut graph);
    attach_mock_instance::<MockInputOnlyNode::Node>(&mut graph, &consumer_id);
    let link_id = add_link_from_out1_to_in1(&mut graph, &source_id, &consumer_id);
    open_iceoryx2_service(&mut graph, &link_id, &Iceoryx2Node::for_this_test_process())
        .expect("the link wires");
    let held = open_the_source_side_for_a_reader_on_the_reserved_slot(
        &mut graph,
        &OutputLinkPortRef::new(&source_id, "out1"),
        &Iceoryx2Node::for_this_test_process(),
    )
    .expect("the publisher joins the port");

    close_iceoryx2_service(&mut graph, &link_id).expect("the link disconnects");
    assert!(
        source_output.has_channel_publisher("out1"),
        "the publisher's hold keeps the port publishing with no link left"
    );

    drop(held);
    assert!(!source_output.has_channel_publisher("out1"));
}

/// A channel created for a windowed destination is created at the windowed
/// ring depth, never at the profile's and never at its mailbox's.
///
/// Fail-without-fix: keep creating every channel at `ORDERED_DEPTH` and the
/// held service states 16.
#[test]
fn a_channel_created_for_a_windowed_destination_holds_the_windowed_ring_depth() {
    let link = NativeLinkIntoAWindowedPort::wire();

    assert_eq!(
        creation_depth_of_the_channel_held_by(&link.graph, &link.link_id),
        WINDOWED_PORT_SUBSCRIBER_RING_DEPTH,
    );
}

/// A windowed consumer added beside a plain one before either is wired sizes
/// the channel even when the compiler opens the plain link first — which is
/// the fix the live refusal names.
#[test]
fn a_windowed_consumer_added_beside_a_plain_one_sizes_the_channel_even_when_the_plain_link_opens_first()
 {
    let node = Iceoryx2Node::for_this_test_process();
    let mut graph = Graph::new();
    let (source_id, plain_link) = add_a_source_linked_to_a_plain_consumer(&mut graph);
    let windowed = add_a_windowed_consumer_linked_from_out1(&mut graph, &source_id);

    open_iceoryx2_service(&mut graph, &plain_link, &node).expect("the plain consumer wires");
    open_iceoryx2_service(&mut graph, &windowed.link_id, &node)
        .expect("the windowed consumer joins the channel created for it");

    assert_eq!(
        creation_depth_of_the_channel_held_by(&graph, &plain_link),
        WINDOWED_PORT_SUBSCRIBER_RING_DEPTH,
    );
}

/// A windowed consumer connected onto a channel already running at the
/// profile's depth is refused by name, before any port is opened for it.
///
/// Fail-without-fix: drop the refusal and iceoryx2 refuses the 64-slot
/// subscriber with an error naming neither the port nor the fix.
#[test]
fn a_windowed_consumer_connected_onto_a_running_shallower_channel_is_refused_naming_both_depths() {
    let node = Iceoryx2Node::for_this_test_process();
    let mut graph = Graph::new();
    let (source_id, plain_link) = add_a_source_linked_to_a_plain_consumer(&mut graph);
    open_iceoryx2_service(&mut graph, &plain_link, &node).expect("the plain consumer wires");
    let windowed = add_a_windowed_consumer_linked_from_out1(&mut graph, &source_id);

    let refusal = open_iceoryx2_service(&mut graph, &windowed.link_id, &node)
        .expect_err("a windowed port cannot read through a ring its channel cannot hold")
        .to_string();

    assert_the_refusal_names_the_port_the_link_both_depths_and_the_fix(&refusal, &windowed.link_id);
    assert!(
        !windowed.dest_input.has_subscribers(),
        "a refused link leaves no port half-wired"
    );
}

/// A windowed-onto-shallow refusal names the `audio` port, the link, the
/// channel's 16 and the ring's 64 in the phrases the message uses, and the
/// fix.
fn assert_the_refusal_names_the_port_the_link_both_depths_and_the_fix(
    refusal: &str,
    windowed_link: &LinkUniqueId,
) {
    let channel_depth = format!("created {} bags deep", DeliveryProfile::ORDERED_DEPTH);
    let ring_depth = format!("{}-bag ring", WINDOWED_PORT_SUBSCRIBER_RING_DEPTH);
    assert!(
        refusal.contains("'audio'")
            && refusal.contains(windowed_link.as_str())
            && refusal.contains(&channel_depth)
            && refusal.contains(&ring_depth)
            && refusal.contains("Connect the windowed consumer before the channel's other links"),
        "the refusal must name the port, the link, both depths and the fix; got {refusal}"
    );
}

/// A channel no link holds any more can still be alive in the runtime's
/// publisher, or in a helper yet to release its port, at the depth it was
/// created at. A windowed consumer connected onto it is refused by name
/// there too, rather than by iceoryx2's refusal of a 64-deep reopen.
#[test]
fn a_windowed_consumer_onto_a_shallower_channel_only_the_publisher_still_holds_is_refused_naming_both_depths()
 {
    let node = Iceoryx2Node::for_this_test_process();
    let mut graph = Graph::new();
    let (source_id, plain_link) = add_a_source_linked_to_a_plain_consumer(&mut graph);
    open_iceoryx2_service(&mut graph, &plain_link, &node).expect("the plain consumer wires");
    let _held_open_the_way_the_publisher_holds_it = node
        .open_or_create_service(
            &channel_service_name(&OutputLinkPortRef::new(source_id.as_str(), "out1"))
                .expect("the mock's output port derives a channel name"),
            MAX_DESTINATIONS_PER_CHANNEL + RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL,
            DeliveryProfile::ORDERED_DEPTH,
        )
        .expect("the publisher joins the running channel");
    close_iceoryx2_service(&mut graph, &plain_link).expect("the plain consumer disconnects");
    let windowed = add_a_windowed_consumer_linked_from_out1(&mut graph, &source_id);

    let refusal = open_iceoryx2_service(&mut graph, &windowed.link_id, &node)
        .expect_err("the channel the publisher holds is still 16 deep")
        .to_string();

    assert_the_refusal_names_the_port_the_link_both_depths_and_the_fix(&refusal, &windowed.link_id);
}

/// A stalled windowed consumer reads through a 64-bag ring: seventy bags
/// written past its last receive lose exactly six, counted on its link.
///
/// The mailbox the contract sizes holds far more than seventy, so every loss
/// counted here is the ring's. Fail-without-fix: give the subscriber the
/// profile's 16-bag ring and the link counts 54.
#[test]
fn a_stalled_windowed_consumer_counts_what_its_sixty_four_bag_ring_overwrote() {
    const BAGS_PAST_THE_RING: usize = 6;
    let link = NativeLinkIntoAWindowedPort::wire();

    let write_one_block = || {
        write_a_mono_16k_audio_block_on_out1(&link.source_output, 160, 0)
            .expect("the source publishes onto the windowed channel");
    };
    write_one_block();
    link.dest_input.receive_pending();
    for _ in 0..WINDOWED_PORT_SUBSCRIBER_RING_DEPTH + BAGS_PAST_THE_RING {
        write_one_block();
    }
    link.dest_input.receive_pending();

    assert_eq!(
        link.rendered_metrics_of_the_destination()["dropped_bags_by_link"],
        serde_json::json!({ link.link_id.as_str(): BAGS_PAST_THE_RING }),
    );
}

/// A stalled windowed consumer's next window is told of the bags its ring
/// overwrote, and no window after it is.
///
/// Each block is one 512-sample window. Fail-without-fix: leave a window's
/// losses at the stage's zero and every window reads as though nothing was
/// lost.
#[test]
fn a_stalled_windowed_consumers_next_window_is_told_what_its_ring_overwrote() {
    const BAGS_PAST_THE_RING: usize = 6;
    const WINDOW_FRAMES: usize = 512;
    const WINDOW_DURATION_NS: i64 = 32_000_000;
    let link = NativeLinkIntoAWindowedPort::wire();
    let write_block = |block_index: usize| {
        write_a_mono_16k_audio_block_on_out1(
            &link.source_output,
            WINDOW_FRAMES,
            block_index as i64 * WINDOW_DURATION_NS,
        )
        .expect("the source publishes onto the windowed channel");
    };

    write_block(0);
    let first_window = link
        .dest_input
        .read_raw("audio")
        .unwrap()
        .expect("one block is one window");
    for block_index in 1..=WINDOWED_PORT_SUBSCRIBER_RING_DEPTH + BAGS_PAST_THE_RING {
        write_block(block_index);
    }
    let (losses_told_after_the_overrun, _) = CapturedTracingWarnings::captured_while(|| {
        std::iter::from_fn(|| link.dest_input.read_raw("audio").unwrap())
            .map(|window| window.lost_since_last_read)
            .collect::<Vec<_>>()
    });

    assert_eq!(first_window.lost_since_last_read, 0);
    assert_eq!(
        losses_told_after_the_overrun.first(),
        Some(&(BAGS_PAST_THE_RING as u64))
    );
    assert_eq!(
        losses_told_after_the_overrun.iter().sum::<u64>(),
        BAGS_PAST_THE_RING as u64,
        "each loss is told once; got {losses_told_after_the_overrun:?}"
    );
}

/// A gap flush at a windowed destination shows the samples it discarded
/// under the link that fed them, beside that link's dropped bags.
///
/// Three hundred samples wait in the stage for a 512-sample window when a
/// block arrives a second late. Fail-without-fix: leave the flush uncounted
/// and the link renders no loss for a run that threw audio away.
#[test]
fn a_gap_flush_at_a_windowed_destination_renders_its_discarded_samples_under_its_link() {
    let link = NativeLinkIntoAWindowedPort::wire();
    let link_id = link.link_id.to_string();

    assert_eq!(
        link.rendered_metrics_of_the_destination()["discarded_samples_by_link"],
        serde_json::json!({ link_id.as_str(): 0 }),
        "a link into a windowed port renders a zero before it has lost anything"
    );

    write_a_mono_16k_audio_block_on_out1(&link.source_output, 300, 0)
        .expect("the source publishes");
    assert!(
        link.dest_input
            .read_raw("audio")
            .expect("the read succeeds")
            .is_none(),
        "300 of 512 samples is not a window"
    );
    write_a_mono_16k_audio_block_on_out1(&link.source_output, 300, 1_000_000_000)
        .expect("the source publishes");
    let (_, warnings) = CapturedTracingWarnings::captured_while(|| {
        link.dest_input
            .read_raw("audio")
            .expect("the read succeeds")
    });

    let [warning] = warnings.as_slice() else {
        panic!("the flush says so in exactly one warning; got {warnings:?}");
    };
    assert!(
        warning.contains("port=audio")
            && warning.contains(&format!("link={link_id}"))
            && warning.contains("discarded_samples=300"),
        "the warning names the port, the link and the count; got {warning}"
    );
    let metrics = link.rendered_metrics_of_the_destination();
    assert_eq!(
        metrics["discarded_samples_by_link"],
        serde_json::json!({ link_id.as_str(): 300 }),
    );
    assert_eq!(
        metrics["frames_dropped"],
        serde_json::json!(0),
        "discarded samples never enter the bag total"
    );
}

/// A helper-placed windowed port is handed the windowed ring depth as its
/// port depth; its mailbox is still sized from its contract in the child.
/// A port's declared borrow quota is what its resolution carries, a port
/// declaring none takes the default, and a helper is handed it with the
/// rest of the port's wiring.
#[test]
fn a_declared_borrow_quota_reaches_the_resolution_and_a_helpers_wiring() {
    crate::core::test_support::ensure_test_mocks_registered();
    let mut graph = Graph::new();
    let borrowing_id = graph
        .traversal_mut()
        .add_v(NodeSpec::new(
            crate::core::test_support::MockBorrowingConsumerNode::node_class_import_path(),
            serde_json::Value::Null,
        ))
        .expect("the node is named")
        .first()
        .expect("the borrowing mock is registered")
        .id
        .to_string();
    let windowed_id = add_mock_windowed_audio_consumer(&mut graph);

    let declared =
        delivery_resolution_of_input_port(&mut graph, &borrowing_id.as_str().into(), "frames")
            .expect("the declared quota resolves");
    assert_eq!(declared.borrow_quota, 5);
    assert_eq!(declared.depth, DeliveryProfile::ORDERED_DEPTH);
    let undeclared =
        delivery_resolution_of_input_port(&mut graph, &windowed_id.as_str().into(), "audio")
            .expect("an undeclared quota resolves");
    assert_eq!(
        undeclared.borrow_quota,
        crate::iceoryx2::DEFAULT_INPUT_PORT_BORROW_QUOTA
    );

    let dest_instance = attach_node_instance(
        &mut graph,
        &borrowing_id,
        NodeInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
    );
    let dest_link_wiring = out_of_process_link_wiring_of(&graph, &borrowing_id.as_str().into())
        .expect("a helper stub's node carries its link wiring");
    wire_subprocess_dest(
        &mut graph,
        &dest_link_wiring,
        &borrowing_id.as_str().into(),
        "frames",
        "pabc/out1",
        "pdef/notify",
        declared,
        ChannelSizing {
            max_subscribers: 2,
            channel_service_creation_depth: DeliveryProfile::ORDERED_DEPTH,
        },
        1,
        &"L-helper-borrow-quota".into(),
        None,
        None,
    )
    .expect("a helper port wires");
    let recorded = dest_instance
        .lock()
        .out_of_process_link_wiring()
        .expect("the stub records its own wiring")
        .as_setup_command_ports();
    assert_eq!(
        recorded["inputs"][0]["input_port_borrow_quota"],
        serde_json::json!(5)
    );
}

#[test]
fn a_helper_placed_windowed_port_is_handed_the_windowed_ring_depth() {
    let mut graph = Graph::new();
    let dest_id = add_mock_windowed_audio_consumer(&mut graph);
    let dest_instance = attach_node_instance(
        &mut graph,
        &dest_id,
        NodeInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
    );
    let Some(declared) =
        audio_windowing_declared_by_input_port_of(&graph, &dest_id.as_str().into(), "audio")
            .expect("the mock's contract resolves")
    else {
        panic!("the mock's audio port declares a window contract");
    };

    let dest_link_wiring = out_of_process_link_wiring_of(&graph, &dest_id.as_str().into())
        .expect("a helper stub's node carries its link wiring");
    wire_subprocess_dest(
        &mut graph,
        &dest_link_wiring,
        &dest_id.as_str().into(),
        "audio",
        "pabc/out1",
        "pdef/notify",
        DeliveryProfile::Ordered.resolve(),
        ChannelSizing {
            max_subscribers: 2,
            channel_service_creation_depth: WINDOWED_PORT_SUBSCRIBER_RING_DEPTH,
        },
        1,
        &"L-helper-windowed-ring".into(),
        Some(declared),
        None,
    )
    .expect("a windowed helper port wires");

    let recorded = dest_instance
        .lock()
        .out_of_process_link_wiring()
        .expect("the stub records its own wiring")
        .as_setup_command_ports();
    assert_eq!(
        recorded["inputs"][0]["input_port_ring_depth"],
        serde_json::json!(WINDOWED_PORT_SUBSCRIBER_RING_DEPTH),
    );
}
