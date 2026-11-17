// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// A writer whose one output port `out` publishes onto `pubsub` and keeps its
/// last message.
fn output_writer_keeping_the_last_message_of_one_channel(
    pubsub: &Iceoryx2Service,
) -> OutputWriterInner {
    let inner = output_writer_with_one_channel(
        pubsub,
        crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    );
    inner.keep_the_last_message_of_output_ports(["out".to_string()]);
    inner
}

/// A writer whose one declared output port `out` keeps its last message and no
/// link has reached yet.
fn output_writer_keeping_the_last_message_of_one_unlinked_port() -> OutputWriterInner {
    let inner = OutputWriterInner::new();
    inner.declare_output_ports(["out".to_string()]);
    inner.keep_the_last_message_of_output_ports(["out".to_string()]);
    inner
}

/// Install a channel publisher for `out` on `pubsub`, as a first link out of
/// the port does.
fn link_out_onto(inner: &OutputWriterInner, pubsub: &Iceoryx2Service) {
    inner.set_channel_publisher(
        "out",
        pubsub,
        pubsub.create_publisher(64).unwrap(),
        ChannelEgressConfig {
            service_name: "test/out".to_string(),
            trust_tier: ChannelTrustTier::Trusted,
            expected_payload_bytes: 64,
            chunk_ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        },
    );
}

/// A reader connected after a value was written reads that value once the port
/// sends its kept message, stamped as written; a second call sends nothing,
/// since that send reached every reader.
#[test]
fn a_reader_connected_after_a_value_was_written_reads_it_from_the_kept_message() {
    let pubsub = open_channel_data_service("kept/late-reader", 3);
    let inner = output_writer_keeping_the_last_message_of_one_channel(&pubsub);
    let first_reader = pubsub.create_subscriber(4).unwrap();
    inner
        .write_bytes_with_timestamp(
            "out",
            b"the old value",
            1,
            MessageGroupBoundary::StartsGroup,
        )
        .unwrap();
    inner
        .write_bytes_with_timestamp("out", b"the value", 7, MessageGroupBoundary::StartsGroup)
        .unwrap();
    while first_reader.receive().unwrap().is_some() {}

    let late_reader = pubsub.create_subscriber(4).unwrap();
    inner.send_each_kept_message_to_a_reader_it_missed();

    let sample = late_reader
        .receive()
        .unwrap()
        .expect("the late reader reads the kept message");
    assert_eq!(stamped_payload_of(&sample), b"the value");
    assert_eq!(sample.user_header().timestamp_ns, 7);
    assert!(late_reader.receive().unwrap().is_none());

    inner.send_each_kept_message_to_a_reader_it_missed();
    assert!(
        late_reader.receive().unwrap().is_none(),
        "the kept message reached every reader, so nothing is sent again"
    );
}

/// A port that keeps no last message sends nothing again to a late reader.
#[test]
fn a_port_that_keeps_no_last_message_sends_nothing_to_a_late_reader() {
    let pubsub = open_channel_data_service("kept/not-kept", 3);
    let inner = output_writer_with_one_channel(
        &pubsub,
        crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    );
    inner
        .write_bytes_with_timestamp("out", b"a frame", 0, MessageGroupBoundary::StartsGroup)
        .unwrap();

    let late_reader = pubsub.create_subscriber(4).unwrap();
    inner.send_each_kept_message_to_a_reader_it_missed();
    assert!(late_reader.receive().unwrap().is_none());
}

/// A value written while no link reaches the port is kept, and reaches the
/// reader of the port's first link.
#[test]
fn the_last_message_is_kept_while_no_link_reaches_the_port() {
    let inner = output_writer_keeping_the_last_message_of_one_unlinked_port();
    inner
        .write_bytes_with_timestamp("out", b"no link yet", 3, MessageGroupBoundary::StartsGroup)
        .expect("a write to a declared port no link reached sends nothing");

    let pubsub = open_channel_data_service("kept/no-link-write", 2);
    link_out_onto(&inner, &pubsub);
    let reader = pubsub.create_subscriber(4).unwrap();
    inner.send_each_kept_message_to_a_reader_it_missed();

    let sample = reader.receive().unwrap().expect("the kept message");
    assert_eq!(stamped_payload_of(&sample), b"no link yet");
    assert_eq!(sample.user_header().timestamp_ns, 3);
}

/// A loan published while no link reaches the port is kept like a write.
#[test]
fn a_loan_published_while_no_link_reaches_the_port_is_kept() {
    let inner = output_writer_keeping_the_last_message_of_one_unlinked_port();
    let mut loan = inner.loan_output_port_payload("out", 4).unwrap();
    loan.payload_bytes_mut().copy_from_slice(b"loan");
    inner
        .publish_output_port_payload_loan(
            loan,
            5,
            MessageGroupBoundary::ContinuesGroup,
            DataChannelPayloadDescriptor::bytes(),
        )
        .unwrap();

    let pubsub = open_channel_data_service("kept/no-link-loan", 2);
    link_out_onto(&inner, &pubsub);
    let reader = pubsub.create_subscriber(4).unwrap();
    inner.send_each_kept_message_to_a_reader_it_missed();

    let sample = reader.receive().unwrap().expect("the kept loan");
    assert_eq!(stamped_payload_of(&sample), b"loan");
    assert_eq!(sample.user_header().timestamp_ns, 5);
    assert_eq!(
        sample.user_header().group_boundary(),
        MessageGroupBoundary::ContinuesGroup
    );
}

/// A loan published on a linked port is kept, and reaches a later reader.
#[test]
fn a_loan_published_on_a_linked_port_is_kept_for_a_later_reader() {
    let pubsub = open_channel_data_service("kept/linked-loan", 3);
    let inner = output_writer_keeping_the_last_message_of_one_channel(&pubsub);
    let mut loan = inner.loan_output_port_payload("out", 6).unwrap();
    loan.payload_bytes_mut().copy_from_slice(b"loaned");
    inner
        .publish_output_port_payload_loan(
            loan,
            2,
            MessageGroupBoundary::StartsGroup,
            DataChannelPayloadDescriptor::bytes(),
        )
        .unwrap();

    let late_reader = pubsub.create_subscriber(4).unwrap();
    inner.send_each_kept_message_to_a_reader_it_missed();
    assert_eq!(
        stamped_payload_of(&late_reader.receive().unwrap().expect("the kept loan")),
        b"loaned"
    );
}

/// The kept message outlives the port's last link, and reaches the reader of
/// the link that wires it again.
#[test]
fn the_last_message_outlives_the_ports_last_link() {
    let first_channel = open_channel_data_service("kept/relink/first", 2);
    let inner = output_writer_keeping_the_last_message_of_one_channel(&first_channel);
    inner
        .write_bytes_with_timestamp("out", b"kept across", 4, MessageGroupBoundary::StartsGroup)
        .unwrap();
    assert!(inner.remove_channel_link("out", "no link was added"));
    assert!(!inner.has_channel_publisher("out"));

    let second_channel = open_channel_data_service("kept/relink/second", 2);
    link_out_onto(&inner, &second_channel);
    let reader = second_channel.create_subscriber(4).unwrap();
    inner.send_each_kept_message_to_a_reader_it_missed();
    assert_eq!(
        stamped_payload_of(&reader.receive().unwrap().expect("the kept message")),
        b"kept across"
    );
}

/// The events waiting on `listener`, taken.
fn events_waiting_on(
    listener: &iceoryx2::port::listener::Listener<ipc_threadsafe::Service>,
) -> usize {
    let mut events = 0;
    listener.try_wait(|_| events += 1).unwrap();
    events
}

/// Sending a kept message again wakes each destination, so a reader that runs
/// only when a message arrives runs for it.
#[test]
fn sending_a_kept_message_again_wakes_each_destination() {
    let node = Iceoryx2Node::for_this_test_process();
    let pubsub = open_channel_data_service("kept/wake/pubsub", 2);
    let notify_service = node
        .open_or_create_notify_service(&unique_suffix("kept/wake/notify"), 1)
        .unwrap();
    let listener = notify_service.create_listener().unwrap();
    let inner = output_writer_keeping_the_last_message_of_one_channel(&pubsub);
    inner.add_channel_link(
        "out",
        "L-kept-wake",
        notify_service.create_notifier().unwrap(),
    );
    inner
        .write_bytes_with_timestamp("out", b"the value", 0, MessageGroupBoundary::StartsGroup)
        .unwrap();
    while events_waiting_on(&listener) > 0 {}

    let late_reader = pubsub.create_subscriber(4).unwrap();
    inner.send_each_kept_message_to_a_reader_it_missed();
    assert!(
        events_waiting_on(&listener) > 0,
        "the destination is woken by the kept message"
    );
    assert!(late_reader.receive().unwrap().is_some());
}

/// A port with a loan open sends no kept message, since its writer is about
/// to send.
#[test]
fn a_port_with_a_loan_open_sends_no_kept_message() {
    let pubsub = open_channel_data_service("kept/open-loan", 2);
    let inner = output_writer_keeping_the_last_message_of_one_channel(&pubsub);
    inner
        .write_bytes_with_timestamp("out", b"the value", 0, MessageGroupBoundary::StartsGroup)
        .unwrap();
    let late_reader = pubsub.create_subscriber(4).unwrap();
    let loan = inner.loan_output_port_payload("out", 1).unwrap();

    inner.send_each_kept_message_to_a_reader_it_missed();
    assert!(late_reader.receive().unwrap().is_none());

    drop(loan);
    inner.send_each_kept_message_to_a_reader_it_missed();
    assert!(late_reader.receive().unwrap().is_some());
}
