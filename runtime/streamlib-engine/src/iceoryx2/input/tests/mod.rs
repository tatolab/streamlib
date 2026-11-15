// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
use crate::iceoryx2::audio_window::ResolvedAudioWindowContract;
use crate::iceoryx2::channel_sample_test_support::TestChannelSample;
use crate::iceoryx2::read_mode::ReadMode;

mod audio_window_contract_settling;
mod audio_windowed_reads;
mod frames_lost_before_the_read;
mod function_node_islands;
mod inbound_link_named_on_each_read;
mod link_removal_and_payload_bounds;
mod listener_fan_in_and_link_losses;
mod reads_borrows_and_quotas;

fn unique_suffix(tag: &str) -> String {
    format!(
        "test/input/{tag}/{}",
        mint_machine_global_unique_name_suffix()
    )
}

/// A test channel's publisher, numbering its sends from zero the way
/// [`crate::iceoryx2::OutputWriterInner::write_bytes_with_timestamp`] does, so a frame
/// published raw — one whose stamped length a test wants wrong — still
/// reaches the receive seam in sequence.
struct TestChannelPublisherNumberingItsSends {
    publisher: crate::iceoryx2::ChannelDataServicePublisher,
    next_sequence_number: std::cell::Cell<u64>,
}

impl TestChannelPublisherNumberingItsSends {
    fn numbering(publisher: crate::iceoryx2::ChannelDataServicePublisher) -> Self {
        Self {
            publisher,
            next_sequence_number: std::cell::Cell::new(0),
        }
    }

    /// Send `sample` carrying the next sequence number.
    fn send_numbered(&self, sample: &TestChannelSample) {
        let sequence_number = self
            .next_sequence_number
            .replace(self.next_sequence_number.get() + 1);
        let _subscribers_reached = sample.send_under(
            &self.publisher,
            sample.user_header_numbered(sequence_number),
        );
    }
}

/// Open a channel sized for `buffered_frames` in flight and hand back the
/// publisher (kept alive so sent samples stay resident) plus a bound
/// subscriber, so a test can publish one frame at a time.
fn open_channel_for_one_link(
    tag: &str,
    buffered_frames: usize,
) -> (
    TestChannelPublisherNumberingItsSends,
    crate::iceoryx2::ChannelDataServiceSubscriber,
) {
    open_channel_for_one_link_loaning(tag, buffered_frames, 4096)
}

/// The same, with the publisher's loan sized explicitly — an audio block of
/// a thousand `f32` samples outgrows the 4 KiB the frame tests want.
fn open_channel_for_one_link_loaning(
    tag: &str,
    buffered_frames: usize,
    loan_bytes: usize,
) -> (
    TestChannelPublisherNumberingItsSends,
    crate::iceoryx2::ChannelDataServiceSubscriber,
) {
    let channel = crate::iceoryx2::Iceoryx2Node::for_this_test_process()
        .open_or_create_service(&unique_suffix(tag), 2, buffered_frames)
        .unwrap();
    let publisher = channel.create_publisher(loan_bytes).unwrap();
    let subscriber = channel.create_subscriber(buffered_frames).unwrap();
    (
        TestChannelPublisherNumberingItsSends::numbering(publisher),
        subscriber,
    )
}

/// Publish one sample carrying `body` onto an open channel.
fn publish_one_frame(
    publisher: &TestChannelPublisherNumberingItsSends,
    _source_port: &str,
    body: &[u8],
) {
    publisher.send_numbered(&TestChannelSample::carrying(0, body));
}

/// A link bound into `port` of `mailboxes`, for a test that publishes
/// samples onto it one at a time.
fn a_test_link_into(
    mailboxes: &InputMailboxesInner,
    port: &str,
) -> TestChannelPublisherNumberingItsSends {
    let (publisher, subscriber) = open_channel_for_one_link(&format!("test-link/{port}"), 16);
    mailboxes.add_channel_subscriber(
        port,
        &format!("L-test-{port}"),
        &InboundLinkName::from("psource/out"),
        subscriber,
    );
    publisher
}

fn a_512_512_contract_at(sample_rate: u32, channels: u32) -> ResolvedAudioWindowContract {
    ResolvedAudioWindowContract::from_declared_values(
        &crate::core::descriptors::AudioWindowContractDeclaredValues {
            sample_rate,
            channels: Some(channels),
            dtype: "f32".to_string(),
            window_size: 512,
            hop: 512,
        },
    )
    .expect("a contract the stage can honour")
}

/// Publish one mono f32 Channels block of `frames` samples at
/// `sample_rate`, the shape a microphone publishes.
fn publish_one_mono_channels_block(
    publisher: &TestChannelPublisherNumberingItsSends,
    frames: usize,
    sample_rate: u32,
    first_sample_timestamp_ns: i64,
) {
    publisher.send_numbered(&TestChannelSample::of_a_mono_channels_block(
        frames,
        sample_rate,
        first_sample_timestamp_ns,
    ));
}

/// An `OutputWriterInner` publishing `output_port` onto `channel`, the way
/// the wiring op installs one, with one outbound link.
fn output_writer_publishing_onto(
    channel: &crate::iceoryx2::Iceoryx2Service,
    output_port: &str,
) -> crate::iceoryx2::OutputWriterInner {
    let output_writer = crate::iceoryx2::OutputWriterInner::new();
    install_a_fresh_publisher(&output_writer, channel, output_port);
    output_writer
}

fn install_a_fresh_publisher(
    output_writer: &crate::iceoryx2::OutputWriterInner,
    channel: &crate::iceoryx2::Iceoryx2Service,
    output_port: &str,
) {
    output_writer.set_channel_publisher(
        output_port,
        channel,
        channel.create_publisher(64).unwrap(),
        crate::iceoryx2::ChannelEgressConfig {
            service_name: format!("test/input/{output_port}"),
            trust_tier: crate::iceoryx2::ChannelTrustTier::Trusted,
            expected_payload_bytes: 64,
            chunk_ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        },
    );
    output_writer.add_channel_link(
        output_port,
        "L-to-the-destination",
        crate::iceoryx2::a_notifier_nothing_listens_on(&unique_suffix(&format!(
            "{output_port}/notify"
        ))),
    );
}

fn dropped_bag_counts_of(
    mailboxes: &InputMailboxesInner,
) -> std::collections::BTreeMap<String, u64> {
    mailboxes
        .dropped_bag_counts_by_inbound_link()
        .dropped_bag_count_snapshot_by_inbound_link()
}

/// A port fed by a native output writer over one link `L-declared`, read
/// as `read_mode`, and the writer publishing its `out` port.
fn a_port_read_as_fed_by_a_native_writer(
    tag: &str,
    read_mode: ReadMode,
) -> (InputMailboxesInner, crate::iceoryx2::OutputWriterInner) {
    let channel = crate::iceoryx2::Iceoryx2Node::for_this_test_process()
        .open_or_create_service(&unique_suffix(tag), 2, 16)
        .unwrap();
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("in", 16, read_mode);
    mailboxes.add_channel_subscriber(
        "in",
        "L-declared",
        &InboundLinkName::from("psource/out"),
        channel.create_subscriber(16).unwrap(),
    );
    (mailboxes, output_writer_publishing_onto(&channel, "out"))
}

fn write_starting_a_group(output_writer: &crate::iceoryx2::OutputWriterInner, payload: &str) {
    output_writer
        .write_bytes_with_timestamp(
            "out",
            payload.as_bytes(),
            0,
            MessageGroupBoundary::StartsGroup,
        )
        .unwrap();
}

/// Every message a port hands a reader, as its payload and its
/// `lost_since_last_read`, until the port runs dry.
fn payloads_and_losses_read_off(mailboxes: &InputMailboxesInner, port: &str) -> Vec<(String, u64)> {
    std::iter::from_fn(|| mailboxes.read_raw(port).unwrap())
        .map(|message| {
            (
                String::from_utf8(message.payload.to_vec()).unwrap(),
                message.lost_since_last_read,
            )
        })
        .collect()
}
