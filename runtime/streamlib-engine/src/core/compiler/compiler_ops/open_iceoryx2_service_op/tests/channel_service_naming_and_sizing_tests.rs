// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Channel and notify service naming, subscriber and notifier caps, and how a helper knows each link.

use super::*;

#[test]
fn channel_service_name_is_source_port_shaped() {
    let name = channel_service_name(&OutputLinkPortRef::new("Pabc123", "video_out"))
        .expect("legal source port derives a channel name");
    assert_eq!(name, "pabc123/video_out");
}

/// Every channel is created for the fixed destination cap plus the
/// publisher's slot, whatever it feeds today: iceoryx2 pins `max_subscribers` at
/// create time, so a link connected to a running source has to fit a slot
/// that already existed. Mentally revert to sizing from the current
/// fan-out and the first late connect onto a wired port fails to reopen
/// the service. Past the cap the port is refused by name, before any
/// service is touched.
#[test]
fn channel_max_subscribers_is_the_fixed_cap_plus_the_taps_reservation_and_refuses_past_it() {
    let mut graph = Graph::new();
    let src_id = add_mock_output_only(&mut graph);
    let src_uid: NodeRunId = src_id.as_str().into();
    let connect_one_more_destination = |graph: &mut Graph| {
        let dest_id = add_mock_input_only(graph);
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&src_id, "out1"),
            InputLinkPortRef::new(&dest_id, "in1"),
        );
    };

    for _ in 0..3 {
        connect_one_more_destination(&mut graph);
    }
    assert_eq!(
        channel_max_subscribers(&graph, &OutputLinkPortRef::new(src_uid.clone(), "out1"))
            .expect("three destinations fit the cap"),
        MAX_DESTINATIONS_PER_CHANNEL + RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL,
        "the channel is sized for the cap plus the publisher's slot, not for the three \
         destinations it feeds today",
    );

    for _ in 3..=MAX_DESTINATIONS_PER_CHANNEL {
        connect_one_more_destination(&mut graph);
    }
    let refused = channel_max_subscribers(&graph, &OutputLinkPortRef::new(src_uid.clone(), "out1"))
        .expect_err("one destination past the cap is refused");
    assert!(
        refused.to_string().contains("at most"),
        "the refusal names the cap; got {refused}"
    );
}

/// [`resolve_channel_sizing`] reconstructs the exact `max_subscribers` the
/// channel was created with: iceoryx2 verifies `max_subscribers` on every
/// reopen, so a drift here would make every reopen fail.
#[test]
fn resolve_channel_sizing_recovers_service_open_max_subscribers() {
    let mut graph = Graph::new();
    let src_id = add_mock_output_only(&mut graph);
    for _ in 0..2 {
        let dest_id = add_mock_input_only(&mut graph);
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&src_id, "out1"),
            InputLinkPortRef::new(&dest_id, "in1"),
        );
    }
    let src_uid: NodeRunId = src_id.as_str().into();

    let sizing = resolve_channel_sizing(
        &graph,
        &Iceoryx2Node::for_this_test_process(),
        &OutputLinkPortRef::new(src_uid.clone(), "out1"),
        RoomForAWindowedReaderTheSourcesGraphCannotSee::NotNeeded,
    )
    .expect("sizing resolves for a wired channel");
    assert_eq!(
        sizing.max_subscribers,
        channel_max_subscribers(&graph, &OutputLinkPortRef::new(src_uid.clone(), "out1"))
            .expect("two destinations fit the cap"),
        "resolve_channel_sizing must agree with channel_max_subscribers",
    );
}

/// The destination-keyed notify service is created for the fixed inbound
/// cap, not the fan-in of the day: it is created with the first inbound
/// link and iceoryx2 verifies `max_notifiers` on every reopen, so a source
/// connected to a running destination has to fit a notifier slot that
/// already existed. Past the cap the destination is refused by name.
#[test]
fn destination_max_notifiers_is_the_fixed_cap_and_refuses_past_it() {
    let mut graph = Graph::new();
    let dest_id = add_mock_input_only(&mut graph);
    let dest_uid: NodeRunId = dest_id.as_str().into();
    let connect_one_more_source = |graph: &mut Graph| {
        let src_id = add_mock_output_only(graph);
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&src_id, "out1"),
            InputLinkPortRef::new(&dest_id, "in1"),
        );
    };

    for _ in 0..3 {
        connect_one_more_source(&mut graph);
    }
    assert_eq!(
        destination_max_notifiers(&graph, &dest_uid).expect("three inbound links fit the cap"),
        MAX_INBOUND_LINKS_PER_DESTINATION,
    );

    for _ in 3..=MAX_INBOUND_LINKS_PER_DESTINATION {
        connect_one_more_source(&mut graph);
    }
    let refused = destination_max_notifiers(&graph, &dest_uid)
        .expect_err("one inbound link past the cap is refused");
    assert!(
        refused.to_string().contains("at most"),
        "the refusal names the cap; got {refused}"
    );
}

/// A graph resolving function node types, with a destination in a helper
/// process, which evaluates an island at each read, fed `channel_links` links
/// through channels.
fn a_destination_fed_through_channels(channel_links: usize) -> (Graph, NodeRunId) {
    let mut graph =
        crate::core::compiler::compiler_ops::function_node_island::tests::a_graph_resolving_three_function_node_types(
        );
    let dest_id =
        crate::core::compiler::compiler_ops::function_node_island::tests::a_helper_reader_before_its_setup(
            &mut graph,
        );
    for _ in 0..channel_links {
        let src_id = add_mock_output_only(&mut graph);
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&src_id, "out1"),
            InputLinkPortRef::new(&dest_id, "in1"),
        );
    }
    (graph, dest_id.as_str().into())
}

#[test]
fn a_link_from_an_island_the_destination_evaluates_holds_no_notifier_of_its_cap() {
    let (mut graph, dest_uid) =
        a_destination_fed_through_channels(MAX_INBOUND_LINKS_PER_DESTINATION);
    let lfo = crate::core::compiler::compiler_ops::function_node_island::tests::add_a_node(
        &mut graph,
        "test_ops:lfo",
        serde_json::Value::Null,
    );
    graph.traversal_mut().add_e(
        OutputLinkPortRef::new(&lfo, "result"),
        InputLinkPortRef::new(&dest_uid, "in1"),
    );
    assert_eq!(
        destination_max_notifiers(&graph, &dest_uid)
            .expect("the island link is evaluated in the read and needs no notifier"),
        MAX_INBOUND_LINKS_PER_DESTINATION,
    );
}

#[test]
fn each_read_of_a_link_into_an_island_at_the_destination_holds_a_notifier_of_its_cap() {
    use crate::core::compiler::compiler_ops::function_node_island::tests::{
        a_class_node_feeding_remap_into_a_helper_reader,
        a_graph_resolving_three_function_node_types,
    };
    let mut graph = a_graph_resolving_three_function_node_types();
    let (remap, dest_uid, into_the_island, into_in1) =
        a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
    let into_in2 = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(&remap, "result"),
            InputLinkPortRef::new(&dest_uid, "in2"),
        )
        .first()
        .unwrap()
        .id
        .clone();
    let iceoryx2_node = Iceoryx2Node::for_this_test_process();
    for island_link in [&into_in1, &into_in2] {
        crate::core::compiler::compiler_ops::function_node_island::wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            island_link,
            &iceoryx2_node,
        )
        .unwrap();
    }
    crate::core::compiler::compiler_ops::function_node_island::wire_a_link_into_a_function_node_island_through_its_reader(
        &mut graph,
        &into_the_island,
        &iceoryx2_node,
    )
    .unwrap();
    for _ in 0..MAX_INBOUND_LINKS_PER_DESTINATION - 1 {
        let src_id = add_mock_output_only(&mut graph);
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&src_id, "out1"),
            InputLinkPortRef::new(&dest_uid, "in1"),
        );
    }
    let refused = destination_max_notifiers(&graph, &dest_uid)
        .expect_err("the link read through the destination is one inbound link past the cap");
    assert!(
        refused.to_string().contains(&format!(
            "{} inbound links",
            MAX_INBOUND_LINKS_PER_DESTINATION + 1
        )),
        "got {refused}"
    );
}

/// Island reads that push a destination past its inbound cap are refused at the edit and by
/// the wiring in the same words, counted from the graph alike.
///
/// Mental-revert: counting the edit's links apart from the wiring's lets the edit take a link
/// the start then refuses.
#[test]
fn island_reads_past_a_destinations_inbound_cap_are_refused_at_the_edit_and_the_wiring_alike() {
    use crate::core::compiler::compiler_ops::function_node_island::tests::{
        a_class_node_feeding_remap_into_a_helper_reader,
        a_graph_resolving_three_function_node_types,
    };
    use crate::core::compiler::compiler_ops::refuse_a_link_a_channel_cannot_carry;
    let mut graph = a_graph_resolving_three_function_node_types();
    let (remap, dest_uid, _, into_in1) =
        a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
    for _ in 0..MAX_INBOUND_LINKS_PER_DESTINATION - 1 {
        let src_id = add_mock_output_only(&mut graph);
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&src_id, "out1"),
            InputLinkPortRef::new(&dest_uid, "in1"),
        );
    }
    fn link_of<'graph>(
        graph: &'graph Graph,
        link_id: &LinkUniqueId,
    ) -> &'graph crate::core::graph::Link {
        graph.traversal().e(link_id).first().unwrap()
    }
    refuse_a_link_a_channel_cannot_carry(&graph, link_of(&graph, &into_in1))
        .expect("one island read and the channel links fill the cap");
    destination_max_notifiers(&graph, &dest_uid).expect("the wiring takes the cap filled");

    let into_in2 = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(&remap, "result"),
            InputLinkPortRef::new(&dest_uid, "in2"),
        )
        .first()
        .unwrap()
        .id
        .clone();
    let refused_at_the_edit =
        refuse_a_link_a_channel_cannot_carry(&graph, link_of(&graph, &into_in2))
            .expect_err("a second island read is one past the cap");
    let refused_by_the_wiring = destination_max_notifiers(&graph, &dest_uid)
        .expect_err("the wiring refuses the destination past its cap");
    assert_eq!(
        refused_at_the_edit.to_string(),
        refused_by_the_wiring.to_string()
    );
    assert!(
        refused_at_the_edit.to_string().contains(&format!(
            "{} inbound links",
            MAX_INBOUND_LINKS_PER_DESTINATION + 1
        )),
        "{refused_at_the_edit}"
    );
}

/// One helper-placed destination's recorded input entry, wired through the
/// op's own subprocess branch.
fn one_helper_destinations_recorded_input_entry(channel_service_name: &str) -> serde_json::Value {
    let mut graph = Graph::new();
    let dest_id = add_mock_input_only(&mut graph);
    let dest_instance = attach_node_instance(
        &mut graph,
        &dest_id,
        NodeInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
    );
    let link_wiring = out_of_process_link_wiring_of(&graph, &dest_id.as_str().into())
        .expect("a helper stub's node carries its link wiring");
    wire_subprocess_dest(
        &mut graph,
        &link_wiring,
        &dest_id.as_str().into(),
        "in1",
        channel_service_name,
        "pdef/notify",
        DeliveryProfile::Newest.resolve(),
        sizing_of_a_two_subscriber_test_channel(),
        1,
        &LinkUniqueId::from("L-envelope"),
        None,
        &crate::iceoryx2::BundleMembersAtTheRead::default(),
    )
    .expect("recording dest wiring must succeed");

    dest_instance
        .lock()
        .out_of_process_link_wiring()
        .expect("the stub records its own wiring")
        .as_setup_command_ports()["inputs"][0]
        .clone()
}

/// A helper knows a link by the channel it subscribes to.
#[test]
fn the_envelope_hands_a_helper_its_channel_as_the_name_it_knows_the_link_by() {
    let entry = one_helper_destinations_recorded_input_entry("pcam/video");

    assert_eq!(
        entry["channel_service_name"],
        serde_json::json!("pcam/video")
    );
    assert_eq!(entry["inbound_link_name"], serde_json::json!("pcam/video"));
}

/// A native destination knows a link by the channel it subscribes to.
#[test]
fn a_native_destination_knows_each_link_by_its_channel() {
    use crate::core::test_support::MockInputOnlyNode;

    let (_source_output, dest_input) =
        wire_one_test_link::<MockInputOnlyNode::Node>("inbound-link-name", false);

    assert_eq!(
        dest_input.inbound_link_names("in1"),
        vec![InboundLinkName::from("psource/out1")]
    );
}

#[test]
fn a_dead_source_node_makes_releasing_the_publishers_source_side_a_no_op() {
    for source_node_still_there in [true, false] {
        let mut source_graph = Graph::new();
        let source_id = add_mock_output_only(&mut source_graph);
        let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
        let reclaimed_links = far_side.reclaimed_links.clone();
        let source_instance = attach_node_instance(
            &mut source_graph,
            &source_id,
            NodeInstance::new(Box::new(
                OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                    far_side,
                    ProcessExecution::Reactive,
                ),
            )),
        );
        let held = open_the_source_side_for_a_reader_on_the_reserved_slot(
            &mut source_graph,
            &OutputLinkPortRef::new(&source_id, "out1"),
            &Iceoryx2Node::for_this_test_process(),
        )
        .expect("a helper source's port opens for the publisher");
        if !source_node_still_there {
            drop(source_instance);
            drop(source_graph);
            drop(held);
            assert!(
                reclaimed_links.lock().is_empty(),
                "a source node that is gone is told nothing"
            );
            continue;
        }

        drop(held);
        assert_eq!(
            reclaimed_links.lock().len(),
            1,
            "a source node still there is told to drop its side"
        );
        drop(source_instance);
    }
}
