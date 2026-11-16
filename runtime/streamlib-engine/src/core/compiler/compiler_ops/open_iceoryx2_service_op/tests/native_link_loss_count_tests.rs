// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


use super::*;

/// What one helper destination was wired with, beside what each of its two
/// sources — one in the app process, one a helper — was told about it.
struct HelperDestinationFedByBothKindsOfSource {
    notifiers_the_engine_source_holds: usize,
    dest_notify_service_name_the_helper_source_was_handed: serde_json::Value,
    notify_service_names_the_destination_was_handed: Vec<serde_json::Value>,
    // Held so the services outlive the assertions.
    _graph: Graph,
}

/// Wire an app-process source and a helper source into one helper
/// destination whose child runs `far_side_process_execution`, through the op
/// the compiler runs.
fn wire_both_kinds_of_source_into_a_helper_destination_driven_in(
    far_side_process_execution: ProcessExecution,
) -> HelperDestinationFedByBothKindsOfSource {
    use crate::core::test_support::MockOutputOnlyProcessor;

    let node = Iceoryx2Node::for_this_test_process();
    let mut graph = Graph::new();
    let engine_source_id = add_mock_output_only(&mut graph);
    let (_, engine_source_output, _) =
        attach_mock_instance::<MockOutputOnlyProcessor::Processor>(&mut graph, &engine_source_id);
    let helper_source_id = add_mock_output_only(&mut graph);
    let helper_source = attach_processor_instance(
        &mut graph,
        &helper_source_id,
        ProcessorInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
    );
    let dest_id = add_mock_input_only(&mut graph);
    let dest = attach_processor_instance(
        &mut graph,
        &dest_id,
        ProcessorInstance::new(Box::new(
            OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                RecordingOutOfProcessFarSideLinkDelivery::default(),
                far_side_process_execution,
            ),
        )),
    );

    let from_the_engine_source = add_link_from_out1_to_in1(&mut graph, &engine_source_id, &dest_id);
    open_iceoryx2_service(&mut graph, &from_the_engine_source, &node)
        .expect("the engine source's link wires");
    let from_the_helper_source = add_link_from_out1_to_in1(&mut graph, &helper_source_id, &dest_id);
    open_iceoryx2_service(&mut graph, &from_the_helper_source, &node)
        .expect("the helper source's link wires");

    let helper_source_output_entry = helper_source
        .lock()
        .out_of_process_link_wiring()
        .expect("the stub records its own wiring")
        .as_setup_command_ports()["outputs"][0]
        .clone();
    HelperDestinationFedByBothKindsOfSource {
        notifiers_the_engine_source_holds: engine_source_output
            .expect("an output-only mock holds an output writer")
            .channel_notifier_count("out1"),
        dest_notify_service_name_the_helper_source_was_handed:
            helper_source_output_entry["dest_notify_service_name"].clone(),
        notify_service_names_the_destination_was_handed: [
            &from_the_engine_source,
            &from_the_helper_source,
        ]
        .into_iter()
        .map(|link_id| input_entry_recorded_on(&dest, link_id)["notify_service_name"].clone())
        .collect(),
        _graph: graph,
    }
}

/// A helper destination is notified by both kinds of source whatever mode
/// its child runs in, on the service its own wiring opens its listener on.
///
/// Fail-without-fix: notify only a reactive destination again and a
/// `continuous` or `manual` helper's app-process source holds no notifier
/// while its helper source is handed an empty name.
#[test]
fn a_helper_destination_is_notified_by_both_kinds_of_source_whatever_its_child_runs() {
    for far_side_process_execution in [
        ProcessExecution::Reactive,
        ProcessExecution::Continuous { interval_ms: 0 },
        ProcessExecution::Manual,
    ] {
        let wired = wire_both_kinds_of_source_into_a_helper_destination_driven_in(
            far_side_process_execution,
        );
        assert_eq!(
            wired.notifiers_the_engine_source_holds, 1,
            "an app-process source must notify a {far_side_process_execution:?} helper"
        );
        assert_ne!(
            wired.dest_notify_service_name_the_helper_source_was_handed,
            serde_json::json!(""),
            "a helper source must notify a {far_side_process_execution:?} helper"
        );
        assert_eq!(
            wired.notify_service_names_the_destination_was_handed,
            vec![
                wired
                    .dest_notify_service_name_the_helper_source_was_handed
                    .clone();
                2
            ],
            "a helper source notifies the service a {far_side_process_execution:?} helper \
             listens on"
        );
    }
}

/// What the control plane serves for a dropping run: the destination's node
/// carries its per-inbound-link dropped-bag counts, live off the mailboxes
/// that did the evicting.
///
/// Driven through the real seam end to end — the destination-side wiring
/// the compiler op runs, a real channel service, the source's own
/// `write_bytes_with_timestamp`, the destination's own `receive_pending` — so what is
/// asserted is the rendering a `GET /api/graph` reader gets, not a counter
/// poked by hand. Fail-without-fix: drop the publish step from
/// `wire_rust_dest` and the node renders no `metrics` at all, so a run that
/// lost three of its four bags reads exactly like a healthy one.
#[test]
fn a_dropping_destinations_node_renders_each_inbound_links_losses() {
    const DESTINATION_MAILBOX_DEPTH: usize = 1;
    const FRAMES_PUBLISHED: usize = 4;

    let mut link = NativeLinkWiredForLossCounting::wire(
        "dropped-bag-counts",
        DeliveryResolution {
            drain_order: crate::iceoryx2::ReadMode::ReadNextInOrder,
            depth: DESTINATION_MAILBOX_DEPTH,
            borrow_quota: crate::iceoryx2::DEFAULT_INPUT_PORT_BORROW_QUOTA,
        },
        crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    );
    let dest_id = link.dest_id.clone();
    let link_id = link.link_id.to_string();

    assert_eq!(
        link.rendered_metrics_of(&dest_id).unwrap()["dropped_bags_by_link"],
        serde_json::json!({ link_id.as_str(): 0 }),
        "a wired link that has lost nothing must render a zero, not go missing"
    );

    // Taken off the subscriber after every bag: the subscriber's ring is as
    // deep as the mailbox, so what is counted here is eviction alone.
    for _ in 0..FRAMES_PUBLISHED {
        link.write_bags(1);
        link.dest_input.receive_pending();
    }

    let metrics = link.rendered_metrics_of(&dest_id).unwrap();
    assert_eq!(
        metrics["dropped_bags_by_link"],
        serde_json::json!({ link_id.as_str(): FRAMES_PUBLISHED - DESTINATION_MAILBOX_DEPTH }),
        "the node must read the counts live, off the mailboxes that evicted"
    );
    assert_eq!(
        metrics["frames_dropped"],
        serde_json::json!(FRAMES_PUBLISHED - DESTINATION_MAILBOX_DEPTH),
        "the total must be the per-link counts summed, never a second tally"
    );
    assert!(
        metrics.get("discarded_samples_by_link").is_none(),
        "a link into a port that declares no window contract carries no sample count; \
         got {metrics}"
    );
}

/// An `ordered` consumer that stops reading shows exactly the bags its
/// subscriber ring overwrote, under the link they were lost from.
///
/// The consumer reads one bag, then nothing while the source writes six
/// past its ring, then receives once: the ring hands over the newest
/// sixteen and the jump in their sequence numbers is the six it lost. The
/// mailbox is as deep as the ring, so nothing is evicted there and the
/// count is the ring's alone. Fail-without-fix: drop the gap from
/// `receive_pending` and the node renders a zero for a run that lost six.
#[test]
fn an_ordered_consumer_that_stops_reading_renders_exactly_the_bags_its_ring_overwrote() {
    const BAGS_PAST_THE_RING: usize = 6;

    let mut link = NativeLinkWiredForLossCounting::wire(
        "ring-overrun/ordered",
        DeliveryProfile::Ordered.resolve(),
        crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    );
    let dest_id = link.dest_id.clone();
    let link_id = link.link_id.to_string();

    link.write_bags(1);
    link.dest_input
        .read_raw("in1")
        .unwrap()
        .expect("the consumer reads its first bag");
    link.write_bags(DeliveryProfile::ORDERED_DEPTH + BAGS_PAST_THE_RING);
    link.dest_input.receive_pending();

    let metrics = link.rendered_metrics_of(&dest_id).unwrap();
    assert_eq!(
        metrics["dropped_bags_by_link"],
        serde_json::json!({ link_id.as_str(): BAGS_PAST_THE_RING }),
    );
    assert_eq!(
        metrics["frames_dropped"],
        serde_json::json!(BAGS_PAST_THE_RING)
    );
    assert_eq!(
        link.dest_input.drain("in1").len(),
        DeliveryProfile::ORDERED_DEPTH,
        "counted plus delivered accounts for every bag written after the first"
    );
}

/// A message lost on a running `ordered` link is told to the reader's next
/// read, beside the group flag the producer wrote, and to no read after it.
///
/// The consumer reads one message, then nothing while the source writes six
/// past its ring. Fail-without-fix: leave the reads naming no loss and the
/// consumer reads sixteen messages that look contiguous over a gap of six.
#[test]
fn a_message_lost_on_a_running_ordered_link_is_told_to_the_next_read() {
    const BAGS_PAST_THE_RING: usize = 6;

    let mut link = NativeLinkWiredForLossCounting::wire(
        "lost-since-last-read/ordered",
        DeliveryProfile::Ordered.resolve(),
        crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    );
    let dest_id = link.dest_id.clone();
    let link_id = link.link_id.to_string();
    let write_one_message = |group_boundary| {
        link.source_output
            .write_bytes_with_timestamp("out1", b"a message", 0, group_boundary)
            .expect("the source publishes onto the wired channel");
    };

    write_one_message(MessageGroupBoundary::StartsGroup);
    let first = link
        .dest_input
        .read_raw("in1")
        .unwrap()
        .expect("the consumer reads its first message");
    for _ in 0..DeliveryProfile::ORDERED_DEPTH + BAGS_PAST_THE_RING {
        write_one_message(MessageGroupBoundary::ContinuesGroup);
    }
    let after_the_overrun: Vec<_> =
        std::iter::from_fn(|| link.dest_input.read_raw("in1").unwrap()).collect();

    assert_eq!(first.lost_since_last_read, 0);
    assert_eq!(first.group_boundary, MessageGroupBoundary::StartsGroup);
    assert_eq!(after_the_overrun.len(), DeliveryProfile::ORDERED_DEPTH);
    assert_eq!(
        after_the_overrun
            .iter()
            .map(|message| message.lost_since_last_read)
            .collect::<Vec<_>>(),
        std::iter::once(BAGS_PAST_THE_RING as u64)
            .chain(std::iter::repeat_n(0, DeliveryProfile::ORDERED_DEPTH - 1))
            .collect::<Vec<_>>(),
    );
    assert!(
        after_the_overrun
            .iter()
            .all(|message| message.group_boundary == MessageGroupBoundary::ContinuesGroup)
    );
    assert_eq!(
        link.rendered_metrics_of(&dest_id).unwrap()["dropped_bags_by_link"],
        serde_json::json!({ link_id.as_str(): BAGS_PAST_THE_RING }),
        "telling the reader leaves the cumulative count `graph` renders as it was"
    );
}

/// A `newest` consumer passing over bags is the profile working, in its
/// ring and in its mailbox alike, and its link shows no loss for either.
///
/// Fail-without-fix: count the ring gap whatever the read mode and the link
/// shows six; count every mailbox eviction whatever the read mode and it
/// shows the evictions of the second burst too.
#[test]
fn a_newest_consumer_renders_no_loss_for_bags_passed_over_in_its_ring_or_its_mailbox() {
    const BAGS_PAST_THE_RING: usize = 6;

    let mut link = NativeLinkWiredForLossCounting::wire(
        "ring-overrun/newest",
        DeliveryProfile::Newest.resolve(),
        crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    );
    let dest_id = link.dest_id.clone();
    let link_id = link.link_id.to_string();

    link.write_bags(1);
    link.dest_input.receive_pending();
    link.write_bags(DeliveryProfile::NEWEST_DEPTH + BAGS_PAST_THE_RING);
    link.dest_input.receive_pending();
    for _ in 0..DeliveryProfile::NEWEST_DEPTH + BAGS_PAST_THE_RING {
        link.write_bags(1);
        link.dest_input.receive_pending();
    }

    let metrics = link.rendered_metrics_of(&dest_id).unwrap();
    assert_eq!(
        metrics["dropped_bags_by_link"],
        serde_json::json!({ link_id.as_str(): 0 }),
    );
    assert_eq!(metrics["frames_dropped"], serde_json::json!(0));
}

/// A producer that only produces carries metrics too, and a write its
/// output port refused at the channel ceiling shows under that port — on the
/// producer, since the refused bag never reached any link.
///
/// Fail-without-fix: attach the metrics from destination wiring alone and
/// the source node renders no `metrics` key, so a camera refusing every
/// frame reads as healthy.
#[test]
fn a_producers_node_renders_the_writes_its_output_port_refused_at_the_ceiling() {
    const CEILING_BYTES: usize = 1024;

    let mut link = NativeLinkWiredForLossCounting::wire(
        "refused-at-the-ceiling",
        DeliveryProfile::Ordered.resolve(),
        CEILING_BYTES,
    );
    let source_id = link.source_id.clone();
    let dest_id = link.dest_id.clone();
    let link_id = link.link_id.to_string();

    assert_eq!(
        link.rendered_metrics_of(&source_id),
        Some(serde_json::json!({
            "frames_dropped": 0,
            "dropped_bags_by_link": {},
            "refused_bags_by_output_port": { "out1": 0 },
            "written_bags_by_output_port": { "out1": 0 }
        })),
        "a wired output port that has refused nothing renders a zero"
    );

    link.source_output
        .write_bytes_with_timestamp(
            "out1",
            &[0u8; CEILING_BYTES],
            0,
            MessageGroupBoundary::StartsGroup,
        )
        .expect_err("a bag past the ceiling is refused");
    link.write_bags(1);
    link.dest_input.receive_pending();

    assert_eq!(
        link.rendered_metrics_of(&source_id).unwrap()["refused_bags_by_output_port"],
        serde_json::json!({ "out1": 1 }),
    );
    assert_eq!(
        link.rendered_metrics_of(&source_id).unwrap()["written_bags_by_output_port"],
        serde_json::json!({ "out1": 1 }),
        "the refused bag is not counted as written; the one sent is"
    );
    assert_eq!(
        link.rendered_metrics_of(&dest_id).unwrap()["dropped_bags_by_link"],
        serde_json::json!({ link_id.as_str(): 0 }),
        "the refused bag is the producer's loss and no link's"
    );
}

/// Groups a producer's writer gave up upstream show under its output port
/// on the producer, and the downstream link that carried the declaration
/// renders no drop, while its reader is told of them.
///
/// Fail-without-fix: advance the sequence number instead of carrying the
/// declaration in its own field, and the loss renders as the downstream
/// link's drop.
#[test]
fn a_producers_node_renders_the_groups_its_writer_gave_up_and_the_downstream_link_drops_none() {
    let mut link = NativeLinkWiredForLossCounting::wire(
        "given-up-groups",
        DeliveryProfile::Ordered.resolve(),
        crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    );
    let source_id = link.source_id.clone();
    let dest_id = link.dest_id.clone();
    let link_id = link.link_id.to_string();

    assert!(
        link.rendered_metrics_of(&source_id)
            .unwrap()
            .get("given_up_groups_by_output_port")
            .is_none(),
        "a port that has given up nothing renders no key"
    );

    link.write_bags(1);
    link.source_output
        .declare_groups_given_up_upstream_before_the_next_write("out1", 2)
        .unwrap();
    link.write_bags(1);

    assert_eq!(
        link.rendered_metrics_of(&source_id).unwrap()["given_up_groups_by_output_port"],
        serde_json::json!({ "out1": 2 }),
    );
    let lost_told_per_read: Vec<u64> =
        std::iter::from_fn(|| link.dest_input.read_raw("in1").unwrap())
            .map(|message| message.lost_since_last_read)
            .collect();
    assert_eq!(lost_told_per_read, [0, 2]);
    let dest_metrics = link.rendered_metrics_of(&dest_id).unwrap();
    assert_eq!(
        dest_metrics["dropped_bags_by_link"],
        serde_json::json!({ link_id.as_str(): 0 }),
        "a group given up upstream is the producer's loss and no link's"
    );
    assert_eq!(dest_metrics["frames_dropped"], serde_json::json!(0));
}

/// Every destination is notified and holds a listener, a self-driven one
/// that never drains it included: iceoryx2 counts repeat notifications in
/// shared memory and sends no further wakeup until the listener drains, so
/// an undrained listener costs its source nothing but that count.
///
/// Fail-without-fix: notify only a reactive destination again and the
/// `manual` destination's link carries no notifier and opens no listener.
#[test]
fn a_destination_is_notified_and_listens_whatever_it_runs() {
    use crate::core::test_support::{MockInputOnlyProcessor, MockReactiveInputOnlyProcessor};

    const SELF_DRIVEN: &str = "self-driven";
    const REACTIVE: &str = "reactive";
    for (tag, (source_output, dest_input)) in [
        (
            SELF_DRIVEN,
            wire_one_test_link::<MockInputOnlyProcessor::Processor>(SELF_DRIVEN, false),
        ),
        (
            REACTIVE,
            wire_one_test_link::<MockReactiveInputOnlyProcessor::Processor>(REACTIVE, true),
        ),
    ] {
        assert!(source_output.has_channel_publisher("out1"));
        assert!(dest_input.has_port("in1"));
        assert_eq!(
            source_output.channel_notifier_count("out1"),
            1,
            "a {tag} destination's link must carry its notifier"
        );
        assert!(
            dest_input.has_listener(),
            "a {tag} destination must hold the listener its sources notify"
        );
    }
}
