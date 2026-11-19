// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A reactive loop's reads of Trigger inputs: each fire, queued or linked, one run.

use super::*;

/// A fire queued on an input no link feeds, with no listener installed, wakes the waiting loop
/// at once through the mailboxes' own wake, and each of ten fires runs the node once.
///
/// Fail-without-fix: leave the mailboxes' wake out of the waiter and the loop sleeps its whole
/// bounded wait before it finds the first fire.
#[test]
fn each_fire_of_an_unlinked_input_wakes_the_loop_and_runs_the_node_once() {
    let (node, mailboxes) = input_only_node_with_plain_port(None);
    let running = ReactiveSchedulingLoopOnItsOwnThread::start(
        node,
        Arc::clone(&mailboxes),
        std::time::Duration::ZERO,
        PauseGateAtLoopStart::Open,
    );
    std::thread::sleep(std::time::Duration::from_millis(50));

    mailboxes.queue_a_trigger_fire("in1", 1).unwrap();
    assert!(
        running.made_within(
            NodeSchedulingCallback::Process,
            1,
            std::time::Duration::from_millis(250)
        ),
        "a fire must end the loop's wait, well inside its bounded wait"
    );

    for fired_at_ns in 2..=10 {
        mailboxes.queue_a_trigger_fire("in1", fired_at_ns).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let ran_ten_times = running.made_within(
        NodeSchedulingCallback::Process,
        10,
        std::time::Duration::from_secs(2),
    );
    std::thread::sleep(std::time::Duration::from_millis(50));
    let runs = running.count_of(NodeSchedulingCallback::Process);
    running.stop();
    assert!(ran_ten_times, "ten fires ran the node {runs} times");
    assert_eq!(runs, 10, "each fire runs the node once");
}

/// A node reading a Trigger input by the delivery its pin defaults to, its port added as its
/// declaration reads.
fn a_trigger_reader_by_its_declared_delivery(
    listener: iceoryx2::port::listener::Listener<ipc_threadsafe::Service>,
) -> (
    Arc<Mutex<NodeInstance>>,
    Arc<crate::iceoryx2::InputMailboxesInner>,
) {
    use crate::core::test_support::MockTriggerReaderNode;

    let declared = <MockTriggerReaderNode::Node as crate::core::GeneratedNode>::descriptor()
        .expect("the mock declares itself")
        .with_input_deliveries_defaulted_by_pin()
        .expect("a Trigger input reads by its pin's default");
    let delivery = crate::iceoryx2::DeliveryProfile::from_manifest_str(
        declared.inputs[0]
            .delivery_profile
            .as_deref()
            .expect("a Trigger input reads by a delivery"),
    )
    .unwrap()
    .resolve();
    let mut instance =
        NodeInstance::new(Box::new(
            <MockTriggerReaderNode::Node as crate::core::GeneratedNode>::from_config(
                Default::default(),
            )
            .expect("the mock constructs from its default config"),
        ));
    instance
        .install_iceoryx2_resources()
        .expect("the mock accepts its iceoryx2 resources");
    let mailboxes = instance
        .iceoryx2_input_mailboxes_inner()
        .expect("the mock holds input mailboxes");
    mailboxes.add_port("in1", delivery.depth, delivery.drain_order);
    mailboxes.set_listener(listener);
    (Arc::new(Mutex::new(instance)), mailboxes)
}

/// One Trigger message, the empty map in msgpack, stamped with its event's time.
fn a_trigger_message_at(
    fired_at_ns: i64,
) -> crate::iceoryx2::channel_sample_test_support::TestChannelSample {
    crate::iceoryx2::channel_sample_test_support::TestChannelSample::carrying(fired_at_ns, &[0x80])
}

/// A Trigger output linked to a Trigger input runs the reader once for each event: a burst of
/// ten, sent faster than the reader reads, is read whole, as a Trigger reads `ordered`.
///
/// Mental-revert: default a Trigger input to `newest` and the burst reads as its last event.
#[test]
fn a_linked_trigger_runs_its_reader_once_for_each_event() {
    let iceoryx2_node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
    let service = open_one_listener_event_service(&iceoryx2_node, "linked-trigger");
    let notifier = service.notifier_builder().create().unwrap();
    let (node, mailboxes) = a_trigger_reader_by_its_declared_delivery(
        crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&service)
            .unwrap(),
    );
    let link_into_the_trigger = a_test_link_into(&mailboxes, "in1");
    let running = ReactiveSchedulingLoopOnItsOwnThread::start(
        node,
        Arc::clone(&mailboxes),
        std::time::Duration::from_millis(2),
        PauseGateAtLoopStart::Open,
    );

    for fired_at_ns in 0..10 {
        link_into_the_trigger.send(&a_trigger_message_at(fired_at_ns));
        notifier.notify().unwrap();
    }
    let ran_ten_times = running.made_within(
        NodeSchedulingCallback::Process,
        10,
        std::time::Duration::from_secs(2),
    );
    std::thread::sleep(std::time::Duration::from_millis(50));
    let runs = running.count_of(NodeSchedulingCallback::Process);
    running.stop();
    assert!(ran_ten_times, "ten events ran the reader {runs} times");
    assert_eq!(runs, 10, "each event runs the reader once");
}

/// The events of a linked Trigger are read in the order they were sent, each stamped with its
/// own time.
#[test]
fn a_linked_triggers_events_are_read_in_order() {
    let iceoryx2_node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
    let service = open_one_listener_event_service(&iceoryx2_node, "linked-trigger-order");
    let (_node, mailboxes) = a_trigger_reader_by_its_declared_delivery(
        crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&service)
            .unwrap(),
    );
    let link_into_the_trigger = a_test_link_into(&mailboxes, "in1");
    for fired_at_ns in 0..10 {
        link_into_the_trigger.send(&a_trigger_message_at(fired_at_ns * 1_000));
    }

    let mut read = Vec::new();
    while let Some(event) = mailboxes.read_raw("in1").unwrap() {
        assert_eq!(&*event.payload, &[0x80]);
        read.push(event.timestamp_ns);
    }
    assert_eq!(read, (0..10).map(|n| n * 1_000).collect::<Vec<_>>());
}
