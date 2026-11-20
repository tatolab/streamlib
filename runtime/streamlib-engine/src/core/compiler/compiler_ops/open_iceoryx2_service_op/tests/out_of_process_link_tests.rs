// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Links into and out of helper processes: their wiring envelopes, rendered counts, reclaim and pools.

use super::*;

/// Record one link's wiring on both out-of-process endpoints, exactly as
/// the compiler op's subprocess branches do.
///
/// Shared so the wiring a disconnect has to undo is byte-for-byte the
/// wiring the connect laid down; both helpers take many positional
/// arguments, so a second copy is a slip waiting to happen.
fn record_wiring_for_both_out_of_process_endpoints(
    graph: &mut Graph,
    source_id: &str,
    dest_id: &str,
    link_id: &LinkUniqueId,
) {
    let source_link_wiring = out_of_process_link_wiring_of(graph, &source_id.into())
        .expect("a helper stub's node carries its link wiring");
    wire_subprocess_source(
        graph,
        &source_link_wiring,
        &source_id.into(),
        "out1",
        "pabc/out1",
        "pdef/notify",
        4096,
        1 << 20,
        sizing_of_a_two_subscriber_test_channel(),
        1,
        DeliveryProfile::Newest.resolve(),
        link_id,
    )
    .expect("recording source wiring must succeed");
    let dest_link_wiring = out_of_process_link_wiring_of(graph, &dest_id.into())
        .expect("a helper stub's node carries its link wiring");
    wire_subprocess_dest(
        graph,
        &dest_link_wiring,
        &dest_id.into(),
        "in1",
        "pabc/out1",
        "pdef/notify",
        DeliveryProfile::Newest.resolve(),
        sizing_of_a_two_subscriber_test_channel(),
        1,
        link_id,
        None,
        None,
    )
    .expect("recording dest wiring must succeed");
}

/// The wiring path reaches a host it cannot name — the whole point of the
/// seam. Mentally revert `wire_subprocess_source` / `wire_subprocess_dest`
/// to downcasting on the two engine-side host types and both vectors stay
/// empty, because this host is neither of them.
#[test]
fn link_wiring_reaches_a_host_the_engine_cannot_downcast_to() {
    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let dest_id = add_mock_input_only(&mut graph);
    let source_instance = attach_node_instance(
        &mut graph,
        &source_id,
        NodeInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
    );
    let dest_instance = attach_node_instance(
        &mut graph,
        &dest_id,
        NodeInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
    );
    let link_id: LinkUniqueId = "L-seam-test".into();

    record_wiring_for_both_out_of_process_endpoints(&mut graph, &source_id, &dest_id, &link_id);

    let recorded_source_ports = source_instance
        .lock()
        .out_of_process_link_wiring()
        .expect("the stub records its own wiring")
        .as_setup_command_ports();
    assert_eq!(
        recorded_source_ports["outputs"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        recorded_source_ports["outputs"][0]["channel_service_name"],
        serde_json::json!("pabc/out1"),
    );

    let recorded_dest_ports = dest_instance
        .lock()
        .out_of_process_link_wiring()
        .expect("the stub records its own wiring")
        .as_setup_command_ports();
    assert_eq!(recorded_dest_ports["inputs"].as_array().unwrap().len(), 1);
    assert_eq!(
        recorded_dest_ports["inputs"][0]["read_mode"],
        serde_json::json!("skip_to_latest"),
    );
}

/// What `graph` renders under `metrics` for `proc_id`, or `None` for no key.
fn rendered_metrics_in(graph: &mut Graph, proc_id: &str) -> Option<serde_json::Value> {
    graph
        .traversal_mut()
        .v(proc_id)
        .first()
        .expect("the node's node must be in the graph")
        .render_components()
        .metrics
        .map(|metrics| serde_json::to_value(metrics).unwrap())
}

/// Create a helper stub's board as its spawn would, and open its writer as
/// its helper would, from a node of the helper's own.
fn board_of_a_helper_stub_spawn(
    instance: &Arc<Mutex<NodeInstance>>,
    proc_id: &str,
    output_port_names: &[&str],
) -> (
    Arc<crate::iceoryx2::HelperProcessLossCountBoardWriter>,
    Iceoryx2Node,
) {
    let output_port_names: Vec<String> = output_port_names
        .iter()
        .map(|name| name.to_string())
        .collect();
    let setup_command_board = instance
        .lock()
        .out_of_process_link_wiring()
        .expect("the stub records its own wiring")
        .create_the_loss_count_board_for_this_helper_spawn_on(
            &Iceoryx2Node::for_this_test_process(),
            &Arc::default(),
            proc_id,
            output_port_names.clone(),
        )
        .expect("the spawn creates its board");
    assert_eq!(
        setup_command_board["output_ports"],
        serde_json::json!(output_port_names),
    );
    let helper_node = Iceoryx2Node::for_this_test_process();
    let writer = helper_node
        .open_helper_process_loss_count_board_writer(
            setup_command_board["service_name"]
                .as_str()
                .expect("the setup command names the board"),
            &output_port_names,
            &[],
        )
        .expect("the helper opens the board its setup command names");
    (Arc::new(writer), helper_node)
}

/// A helper-placed destination's node renders the counts its helper wrote
/// on the slot its link was given, and a helper-placed producer's node the
/// refusals its helper wrote per output port — zero before the helper has
/// written anything, never an absent key.
///
/// Fail-without-fix: leave `wire_subprocess_dest` publishing nothing and
/// the destination renders no `metrics` key at all, so a helper that lost
/// most of its bags reads like a healthy one.
#[test]
fn a_helper_placed_destinations_node_renders_the_counts_its_helper_wrote_on_its_slot() {
    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let dest_id = add_mock_input_only(&mut graph);
    let source_instance = attach_node_instance(
        &mut graph,
        &source_id,
        NodeInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
    );
    let dest_instance = attach_node_instance(
        &mut graph,
        &dest_id,
        NodeInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
    );
    let link_id: LinkUniqueId = "L-helper-placed".into();
    record_wiring_for_both_out_of_process_endpoints(&mut graph, &source_id, &dest_id, &link_id);

    assert_eq!(
        rendered_metrics_in(&mut graph, &dest_id),
        Some(serde_json::json!({
            "frames_dropped": 0,
            "dropped_bags_by_link": { "L-helper-placed": 0 },
            "refused_bags_by_output_port": {},
            "written_bags_by_output_port": {}
        })),
        "a wired link renders zero before its helper's board exists"
    );

    let entry = input_entry_recorded_on(&dest_instance, &link_id);
    assert_eq!(entry["loss_count_slot"], serde_json::json!(0));
    assert_eq!(entry["wiring_generation"], serde_json::json!(1));

    let (dest_board_writer, _dest_helper_node) =
        board_of_a_helper_stub_spawn(&dest_instance, &dest_id, &[]);
    dest_board_writer
        .claim_inbound_link_slot(
            crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                slot: 0,
                wiring_generation: 1,
            },
        )
        .unwrap()
        .mirror_dropped_bags(5);
    assert_eq!(
        rendered_metrics_in(&mut graph, &dest_id),
        Some(serde_json::json!({
            "frames_dropped": 5,
            "dropped_bags_by_link": { "L-helper-placed": 5 },
            "refused_bags_by_output_port": {},
            "written_bags_by_output_port": {}
        })),
    );

    let output_entry = source_instance
        .lock()
        .out_of_process_link_wiring()
        .expect("the stub records its own wiring")
        .as_setup_command_ports()["outputs"][0]
        .clone();
    let output_port_wiring_generation = output_entry["output_port_wiring_generation"]
        .as_u64()
        .expect("the output entry carries its channel's generation");
    let (source_board_writer, _source_helper_node) =
        board_of_a_helper_stub_spawn(&source_instance, &source_id, &["out1"]);
    let out1_mirror = source_board_writer
        .claim_output_port_entry("out1", output_port_wiring_generation)
        .expect("the source's board carries its declared port");
    out1_mirror.mirror_refused_bags(2);
    out1_mirror.mirror_written_bags(61);
    assert_eq!(
        rendered_metrics_in(&mut graph, &source_id),
        Some(serde_json::json!({
            "frames_dropped": 0,
            "dropped_bags_by_link": {},
            "refused_bags_by_output_port": { "out1": 2 },
            "written_bags_by_output_port": { "out1": 61 }
        })),
    );
}

/// A link disconnected and connected again takes the slot it had with a new
/// generation, renders from zero, and ignores a write its helper made under
/// the old wiring.
///
/// Fail-without-fix: render a slot whatever generation is written on it and
/// the reconnected link shows the old wiring's nine bags, a count outliving
/// the link it named.
#[test]
fn a_reconnected_link_reuses_its_slot_from_zero_and_a_write_from_the_old_wiring_is_ignored() {
    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let dest_id = add_mock_input_only(&mut graph);
    attach_node_instance(
        &mut graph,
        &source_id,
        NodeInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
    );
    let dest_instance = attach_node_instance(
        &mut graph,
        &dest_id,
        NodeInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
    );
    let link_id = add_link_from_out1_to_in1(&mut graph, &source_id, &dest_id);
    record_wiring_for_both_out_of_process_endpoints(&mut graph, &source_id, &dest_id, &link_id);
    let (board_writer, _helper_node) = board_of_a_helper_stub_spawn(&dest_instance, &dest_id, &[]);
    let first_wiring = board_writer
        .claim_inbound_link_slot(
            crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                slot: 0,
                wiring_generation: 1,
            },
        )
        .unwrap();
    first_wiring.mirror_dropped_bags(7);

    close_iceoryx2_service(&mut graph, &link_id).expect("the disconnect succeeds");
    assert_eq!(
        rendered_metrics_in(&mut graph, &dest_id)
            .map(|metrics| metrics["dropped_bags_by_link"].clone()),
        Some(serde_json::json!({})),
        "a disconnected link's count goes with it"
    );

    record_wiring_for_both_out_of_process_endpoints(&mut graph, &source_id, &dest_id, &link_id);
    let entry = input_entry_recorded_on(&dest_instance, &link_id);
    assert_eq!(entry["loss_count_slot"], serde_json::json!(0));
    assert_eq!(entry["wiring_generation"], serde_json::json!(2));

    board_writer
        .claim_inbound_link_slot(
            crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                slot: 0,
                wiring_generation: 1,
            },
        )
        .unwrap()
        .mirror_dropped_bags(9);
    assert_eq!(
        rendered_metrics_in(&mut graph, &dest_id)
            .map(|metrics| metrics["dropped_bags_by_link"].clone()),
        Some(serde_json::json!({ link_id.as_str(): 0 })),
        "a slot written under the old generation renders nothing for the new wiring"
    );

    board_writer
        .claim_inbound_link_slot(
            crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                slot: 0,
                wiring_generation: 2,
            },
        )
        .unwrap()
        .mirror_dropped_bags(1);
    assert_eq!(
        rendered_metrics_in(&mut graph, &dest_id)
            .map(|metrics| metrics["dropped_bags_by_link"].clone()),
        Some(serde_json::json!({ link_id.as_str(): 1 })),
    );
}

/// The keys of `rendered`, sorted.
fn rendered_metrics_keys(rendered: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> = rendered
        .as_object()
        .expect("metrics render as an object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

#[test]
fn a_helper_placed_nodes_metrics_carry_exactly_the_keys_an_app_process_ones_do() {
    let mut native_plain = NativeLinkWiredForLossCounting::wire(
        "metrics-keys-plain",
        DeliveryProfile::Ordered.resolve(),
        crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    );
    let dest_id = native_plain.dest_id.clone();
    let native_plain_keys = rendered_metrics_keys(
        &native_plain
            .rendered_metrics_of(&dest_id)
            .expect("an app-process destination renders metrics"),
    );
    let native_windowed_keys = rendered_metrics_keys(
        &NativeLinkIntoAWindowedPort::wire().rendered_metrics_of_the_destination(),
    );

    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let helper_plain_id = add_mock_input_only(&mut graph);
    let helper_windowed_id = add_mock_windowed_audio_consumer(&mut graph);
    for proc_id in [&source_id, &helper_plain_id, &helper_windowed_id] {
        attach_node_instance(
            &mut graph,
            proc_id,
            NodeInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
        );
    }
    record_wiring_for_both_out_of_process_endpoints(
        &mut graph,
        &source_id,
        &helper_plain_id,
        &"L-helper-plain".into(),
    );
    let declared = audio_windowing_declared_by_input_port_of(
        &graph,
        &helper_windowed_id.as_str().into(),
        "audio",
    )
    .expect("the mock's contract resolves");
    let dest_link_wiring =
        out_of_process_link_wiring_of(&graph, &helper_windowed_id.as_str().into())
            .expect("a helper stub's node carries its link wiring");
    wire_subprocess_dest(
        &mut graph,
        &dest_link_wiring,
        &helper_windowed_id.as_str().into(),
        "audio",
        "pabc/out1",
        "pdef/notify",
        DeliveryProfile::Ordered.resolve(),
        ChannelSizing {
            max_subscribers: 2,
            channel_service_creation_depth: WINDOWED_PORT_SUBSCRIBER_RING_DEPTH,
        },
        1,
        &"L-helper-windowed".into(),
        declared,
        None,
    )
    .expect("a windowed helper port wires");

    let helper_plain_keys = rendered_metrics_keys(
        &rendered_metrics_in(&mut graph, &helper_plain_id)
            .expect("a helper-placed destination renders metrics"),
    );
    let helper_windowed_keys = rendered_metrics_keys(
        &rendered_metrics_in(&mut graph, &helper_windowed_id)
            .expect("a helper-placed windowed destination renders metrics"),
    );
    assert_eq!(helper_plain_keys, native_plain_keys);
    assert_eq!(helper_windowed_keys, native_windowed_keys);
    assert!(
        helper_windowed_keys.contains(&"discarded_samples_by_link".to_string()),
        "the windowed rendering is the one carrying a sample count: {helper_windowed_keys:?}"
    );
}

/// Disconnecting a link whose endpoints both live out of process reclaims
/// BOTH halves through the same seam that wired them, each told its own
/// local port and direction — and each host's envelope forgets the link, so
/// the next setup does not re-send it beside the reconnect's own entry.
///
/// Revert lock: reclaim either endpoint through engine-side ports instead
/// of its envelope and that side records no reclaim at all, which is the
/// leak — a live helper child keeps the notifier and appends another on
/// reconnect, until the notify service's create-time `max_notifiers` cap
/// is exhausted (`ExceedsMaxSupportedNotifiers`).
/// A helper's output pools are sized from what its wired output links
/// keep: each link's mailbox depth and borrow quota from wiring until its
/// disconnect.
#[test]
fn a_wired_output_link_keeps_room_in_its_helpers_pools_until_it_is_disconnected() {
    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let dest_id = add_mock_input_only(&mut graph);
    for node_run_id in [&source_id, &dest_id] {
        attach_node_instance(
            &mut graph,
            node_run_id,
            NodeInstance::new(Box::new(
                OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                    RecordingOutOfProcessFarSideLinkDelivery::default(),
                    ProcessExecution::Reactive,
                ),
            )),
        );
    }
    let link_id = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(&source_id, "out1"),
            InputLinkPortRef::new(&dest_id, "in1"),
        )
        .first()
        .expect("the link must exist")
        .id
        .clone();
    let kept_by_the_sources_output_links =
        out_of_process_link_wiring_of(&graph, &source_id.as_str().into())
            .expect("a helper stub's node carries its link wiring")
            .output_frames_kept_by_its_output_links();

    record_wiring_for_both_out_of_process_endpoints(&mut graph, &source_id, &dest_id, &link_id);
    let newest_delivery = DeliveryProfile::Newest.resolve();
    assert_eq!(
        kept_by_the_sources_output_links.kept_by_every_output_link(),
        crate::core::context::texture_pools_and_rings::node_output_surface_pool::OutputFramesKeptByOutputLinks {
            queued_in_their_mailboxes: newest_delivery.depth,
            borrowed_past_their_mailboxes: newest_delivery.borrow_quota,
        }
    );

    close_iceoryx2_service(&mut graph, &link_id).expect("the disconnect must succeed");
    assert_eq!(
        kept_by_the_sources_output_links.kept_by_every_output_link(),
        Default::default()
    );
}

#[test]
fn disconnecting_an_out_of_process_link_reclaims_both_endpoints() {
    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let dest_id = add_mock_input_only(&mut graph);

    let source_reclaims: Arc<Mutex<Vec<ReclaimedLink>>> = Arc::default();
    let dest_reclaims: Arc<Mutex<Vec<ReclaimedLink>>> = Arc::default();
    let source_instance = attach_node_instance(
        &mut graph,
        &source_id,
        NodeInstance::new(Box::new(
            OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                RecordingOutOfProcessFarSideLinkDelivery {
                    reclaimed_links: source_reclaims.clone(),
                    ..Default::default()
                },
                ProcessExecution::Reactive,
            ),
        )),
    );
    let dest_instance = attach_node_instance(
        &mut graph,
        &dest_id,
        NodeInstance::new(Box::new(
            OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                RecordingOutOfProcessFarSideLinkDelivery {
                    reclaimed_links: dest_reclaims.clone(),
                    ..Default::default()
                },
                ProcessExecution::Reactive,
            ),
        )),
    );

    let link_id = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(&source_id, "out1"),
            InputLinkPortRef::new(&dest_id, "in1"),
        )
        .first()
        .expect("the link must exist")
        .id
        .clone();

    record_wiring_for_both_out_of_process_endpoints(&mut graph, &source_id, &dest_id, &link_id);

    close_iceoryx2_service(&mut graph, &link_id).expect("the disconnect must succeed");

    assert_eq!(
        *source_reclaims.lock(),
        [ReclaimedLink {
            port_direction: crate::core::PortDirection::Output,
            local_port_name: "out1".to_string(),
            link_id: link_id.to_string(),
        }],
        "the source host must be asked to drop its publisher-side link, by its own port",
    );
    assert_eq!(
        *dest_reclaims.lock(),
        [ReclaimedLink {
            port_direction: crate::core::PortDirection::Input,
            local_port_name: "in1".to_string(),
            link_id: link_id.to_string(),
        }],
        "the destination host must be asked to drop its subscriber, by its own port",
    );

    for (label, instance) in [("source", &source_instance), ("dest", &dest_instance)] {
        let ports = instance
            .lock()
            .out_of_process_link_wiring()
            .expect("the stub records its own wiring")
            .as_setup_command_ports();
        assert!(
            ports["inputs"].as_array().unwrap().is_empty()
                && ports["outputs"].as_array().unwrap().is_empty(),
            "the {label} envelope must carry nothing for a disconnected link; got {ports}",
        );
    }
}

/// Wiring a link records it on the envelope AND hands the same entry to a
/// far side past its setup command, so a far side that already read its
/// envelope opens the port now. Revert lock: record the entry without
/// handing it over and both vectors stay empty — the link never reaches a
/// running helper.
#[test]
fn wiring_an_out_of_process_link_hands_each_endpoint_its_entry() {
    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let dest_id = add_mock_input_only(&mut graph);

    let source_late_wired: Arc<Mutex<Vec<(crate::core::PortDirection, serde_json::Value)>>> =
        Arc::default();
    let dest_late_wired: Arc<Mutex<Vec<(crate::core::PortDirection, serde_json::Value)>>> =
        Arc::default();
    let source_instance = attach_node_instance(
        &mut graph,
        &source_id,
        NodeInstance::new(Box::new(
            OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                RecordingOutOfProcessFarSideLinkDelivery {
                    late_wired_links: source_late_wired.clone(),
                    ..Default::default()
                },
                ProcessExecution::Reactive,
            ),
        )),
    );
    let dest_instance = attach_node_instance(
        &mut graph,
        &dest_id,
        NodeInstance::new(Box::new(
            OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                RecordingOutOfProcessFarSideLinkDelivery {
                    late_wired_links: dest_late_wired.clone(),
                    ..Default::default()
                },
                ProcessExecution::Reactive,
            ),
        )),
    );

    let link_id: LinkUniqueId = "L-wired-late".into();
    record_wiring_for_both_out_of_process_endpoints(&mut graph, &source_id, &dest_id, &link_id);

    let source_handed = source_late_wired.lock();
    let [(source_direction, source_entry)] = &source_handed[..] else {
        panic!("the source host must be handed exactly one entry; got {source_handed:?}");
    };
    assert_eq!(*source_direction, crate::core::PortDirection::Output);
    assert_eq!(
        *source_entry,
        source_instance
            .lock()
            .out_of_process_link_wiring()
            .expect("the stub records its own wiring")
            .as_setup_command_ports()["outputs"][0],
        "the entry handed to the source is the one its envelope recorded",
    );

    let dest_handed = dest_late_wired.lock();
    let [(dest_direction, dest_entry)] = &dest_handed[..] else {
        panic!("the destination host must be handed exactly one entry; got {dest_handed:?}");
    };
    assert_eq!(*dest_direction, crate::core::PortDirection::Input);
    assert_eq!(
        *dest_entry,
        dest_instance
            .lock()
            .out_of_process_link_wiring()
            .expect("the stub records its own wiring")
            .as_setup_command_ports()["inputs"][0],
        "the entry handed to the destination is the one its envelope recorded",
    );
}

#[test]
fn a_host_with_an_out_of_process_transport_is_recognised_as_a_subprocess() {
    let mut graph = Graph::new();
    let helper_hosted_id = add_mock_output_only(&mut graph);
    attach_node_instance(
        &mut graph,
        &helper_hosted_id,
        NodeInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
    );
    assert!(out_of_process_link_wiring_of(&graph, &helper_hosted_id.as_str().into()).is_some());

    let engine_hosted_id = add_mock_input_only(&mut graph);
    attach_mock_instance::<crate::core::test_support::MockInputOnlyNode::Node>(
        &mut graph,
        &engine_hosted_id,
    );
    assert!(out_of_process_link_wiring_of(&graph, &engine_hosted_id.as_str().into()).is_none());
}

#[test]
fn a_link_between_an_engine_endpoint_and_a_helper_reclaims_each_its_own_way() {
    use crate::core::test_support::MockOutputOnlyNode;

    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let (source, source_output, _) =
        attach_mock_instance::<MockOutputOnlyNode::Node>(&mut graph, &source_id);
    let source_output = source_output.expect("an output-only mock holds an output writer");

    let dest_id = add_mock_input_only(&mut graph);
    let dest_reclaims: Arc<Mutex<Vec<ReclaimedLink>>> = Arc::default();
    attach_node_instance(
        &mut graph,
        &dest_id,
        NodeInstance::new(Box::new(
            OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                RecordingOutOfProcessFarSideLinkDelivery {
                    reclaimed_links: dest_reclaims.clone(),
                    ..Default::default()
                },
                ProcessExecution::Reactive,
            ),
        )),
    );

    let link_id = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(&source_id, "out1"),
            InputLinkPortRef::new(&dest_id, "in1"),
        )
        .first()
        .expect("the link must exist")
        .id
        .clone();

    let (channel, notify_service) =
        open_test_link_services("mixed-endpoints", DeliveryProfile::ORDERED_DEPTH);
    wire_rust_source(
        &mut graph,
        &source_id.as_str().into(),
        &source,
        "out1",
        &link_id,
        &channel,
        &notify_service,
        ChannelEgressConfig {
            service_name: unique_service_name("mixed-endpoints"),
            trust_tier: ChannelTrustTier::UntrustedSession,
            expected_payload_bytes: 4096,
            chunk_ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        },
    )
    .expect("the engine-side source wires");
    let dest_link_wiring = out_of_process_link_wiring_of(&graph, &dest_id.as_str().into())
        .expect("a helper stub's node carries its link wiring");
    wire_subprocess_dest(
        &mut graph,
        &dest_link_wiring,
        &dest_id.as_str().into(),
        "in1",
        "pabc/out1",
        "pdef/notify",
        DeliveryProfile::Newest.resolve(),
        sizing_of_a_two_subscriber_test_channel(),
        1,
        &link_id,
        None,
        None,
    )
    .expect("recording dest wiring must succeed");
    assert!(source_output.has_channel_publisher("out1"));

    close_iceoryx2_service(&mut graph, &link_id).expect("the disconnect must succeed");

    assert!(
        !source_output.has_channel_publisher("out1"),
        "the engine-side source must be reclaimed through its own writer, as it always was",
    );
    assert_eq!(
        *dest_reclaims.lock(),
        [ReclaimedLink {
            port_direction: crate::core::PortDirection::Input,
            local_port_name: "in1".to_string(),
            link_id: link_id.to_string(),
        }],
        "the helper destination must be asked to drop the subscriber it opened itself",
    );
}
