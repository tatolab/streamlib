// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Samples a test publishes raw, so it can stamp what a writer never would.

use super::{
    ChannelDataServicePublisher, DataChannelPayloadDescriptor, DataChannelSampleUserHeader,
    MessageGroupBoundary, data_channel_payload_bytes_of, data_channel_payload_cache_lines_for,
};

/// One sample as a test publishes it: a header stamping
/// `stamped_payload_bytes`, over a payload area holding `carried_body`. The two
/// lengths are separate because several tests exercise their divergence.
#[derive(Clone, Debug)]
pub(crate) struct TestChannelSample {
    pub(crate) timestamp_ns: i64,
    pub(crate) stamped_payload_bytes: u32,
    pub(crate) group_boundary: MessageGroupBoundary,
    pub(crate) payload_descriptor: DataChannelPayloadDescriptor,
    pub(crate) carried_body: Vec<u8>,
}

impl TestChannelSample {
    /// A sample stamping exactly the body it carries, opening a group.
    pub(crate) fn carrying(timestamp_ns: i64, body: &[u8]) -> Self {
        Self::stamping(timestamp_ns, body.len() as u32, body)
    }

    /// A sample stamping `stamped_payload_bytes` over `carried_body`.
    pub(crate) fn stamping(
        timestamp_ns: i64,
        stamped_payload_bytes: u32,
        carried_body: &[u8],
    ) -> Self {
        Self {
            timestamp_ns,
            stamped_payload_bytes,
            group_boundary: MessageGroupBoundary::StartsGroup,
            payload_descriptor: DataChannelPayloadDescriptor::bytes(),
            carried_body: carried_body.to_vec(),
        }
    }

    /// One mono f32 Channels block of `frames` samples at `sample_rate`, a
    /// ramp from 0 towards 1, its first sample stamped at
    /// `first_sample_timestamp_ns`: the shape a microphone publishes.
    pub(crate) fn of_a_mono_channels_block(
        frames: usize,
        sample_rate: u32,
        first_sample_timestamp_ns: i64,
    ) -> Self {
        let ramp: Vec<f32> = (0..frames)
            .map(|index| index as f32 / frames as f32)
            .collect();
        let (payload_descriptor, planar_payload) = super::channels_block_of_one_window(
            &ramp,
            sample_rate,
            1,
            frames as u32,
            super::ChannelsBlockSampleType::F32,
            0,
        );
        Self {
            payload_descriptor,
            ..Self::carrying(first_sample_timestamp_ns, &planar_payload)
        }
    }

    /// The user header this sample carries, numbered `sequence_number`.
    pub(crate) fn user_header_numbered(&self, sequence_number: u64) -> DataChannelSampleUserHeader {
        DataChannelSampleUserHeader {
            sequence_number,
            ..DataChannelSampleUserHeader::new(
                self.timestamp_ns,
                self.stamped_payload_bytes,
                self.group_boundary,
                self.payload_descriptor,
            )
        }
    }

    /// Publish this sample on `publisher` under `user_header`, with a payload
    /// area exactly as long as its body rounds up to, returning how many
    /// subscribers it reached.
    pub(crate) fn send_under(
        &self,
        publisher: &ChannelDataServicePublisher,
        user_header: DataChannelSampleUserHeader,
    ) -> usize {
        let loaned = publisher
            .loan_slice_uninit(data_channel_payload_cache_lines_for(
                self.carried_body.len(),
            ))
            .expect("a test loan");
        // SAFETY: shared memory mapped from the operating system holds
        // initialized bytes; the body is written over it before the send.
        let mut loaned = unsafe { loaned.assume_init() };
        *loaned.user_header_mut() = user_header;
        let cache_lines = loaned.payload_mut();
        // SAFETY: a cache line is a padding-free byte array, so the lines are
        // that many contiguous bytes.
        let area = unsafe {
            std::slice::from_raw_parts_mut(
                cache_lines.as_mut_ptr().cast::<u8>(),
                std::mem::size_of_val(cache_lines),
            )
        };
        area[..self.carried_body.len()].copy_from_slice(&self.carried_body);
        loaned.send().expect("a test send")
    }
}

/// The payload a received sample's header stamps, bounded by its area.
pub(crate) fn stamped_payload_of(sample: &super::ChannelDataServiceSample) -> &[u8] {
    let area = data_channel_payload_bytes_of(sample.payload());
    sample
        .user_header()
        .payload_within(area)
        .expect("the stamped payload fits the sample")
}

/// A link a test binds into one input port, publishing raw samples onto it
/// numbered from zero the way an output writer numbers its sends.
pub(crate) struct TestLinkIntoAnInputPort {
    publisher: ChannelDataServicePublisher,
    next_sequence_number: std::sync::atomic::AtomicU64,
    _channel_data_service: super::Iceoryx2Service,
}

impl TestLinkIntoAnInputPort {
    /// Bind a fresh channel `ring_depth` deep into `port` of `mailboxes`.
    pub(crate) fn bound_into(
        mailboxes: &super::InputMailboxesInner,
        port: &str,
        ring_depth: usize,
    ) -> Self {
        let service_name = format!(
            "test/link-into/{port}/{}",
            crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix()
        );
        let channel_data_service = super::Iceoryx2Node::for_this_test_process()
            .open_or_create_service(&service_name, 2, ring_depth)
            .expect("open a test channel");
        let publisher = channel_data_service
            .create_publisher(4096)
            .expect("a test publisher");
        let subscriber = channel_data_service
            .create_subscriber(ring_depth)
            .expect("a test subscriber");
        mailboxes.add_channel_subscriber(
            port,
            &format!("L-test-{port}"),
            &super::InboundLinkName::from("psource/out"),
            subscriber,
        );
        Self {
            publisher,
            next_sequence_number: std::sync::atomic::AtomicU64::new(0),
            _channel_data_service: channel_data_service,
        }
    }

    /// Publish `sample` as this link's next numbered send.
    pub(crate) fn send(&self, sample: &TestChannelSample) {
        let sequence_number = self
            .next_sequence_number
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _subscribers_reached = sample.send_under(
            &self.publisher,
            sample.user_header_numbered(sequence_number),
        );
    }
}

impl TestChannelSample {
    /// Publish this sample on `publisher`, numbered zero.
    pub(crate) fn send_unnumbered_on(&self, publisher: &ChannelDataServicePublisher) -> usize {
        self.send_under(publisher, self.user_header_numbered(0))
    }
}
