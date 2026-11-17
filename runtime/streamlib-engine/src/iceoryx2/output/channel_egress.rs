// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! One output port's channel egress: its publisher, its links' notifiers and its send accounting.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use iceoryx2::identifiers::UniqueSubscriberId;
use iceoryx2::port::SendError;
use iceoryx2::port::notifier::Notifier;
use iceoryx2::prelude::*;

use crate::core::error::{ChannelTrustTierLabel, Error, Result};
use crate::iceoryx2::loss_counters::OutputPortBagCounters;
use crate::iceoryx2::{
    ChannelDataServiceLoanedSample, ChannelDataServicePublisher, ChannelTrustTier,
    DataChannelSampleUserHeader, Iceoryx2Service, data_channel_payload_cache_lines_for,
};

/// Map the engine's [`ChannelTrustTier`] onto the engine-free
/// [`ChannelTrustTierLabel`] the [`Error::PayloadExceedsChannelCeiling`] variant
/// carries. Lives at the error boundary because the orphan rule forbids a `From`
/// between the two foreign enums, and it keeps the ceiling error engine-free.
pub(super) fn trust_tier_label(trust_tier: ChannelTrustTier) -> ChannelTrustTierLabel {
    match trust_tier {
        ChannelTrustTier::Trusted => ChannelTrustTierLabel::Trusted,
        ChannelTrustTier::UntrustedSession => ChannelTrustTierLabel::UntrustedSession,
    }
}

/// Whether a send that ended in `send_outcome` may have reached a subscriber,
/// and so consumed its sequence number and the losses declared ahead of it.
///
/// A send consumes its number once it may have reached a subscriber. iceoryx2
/// does not say which subscribers a failed send reached, so only a failure
/// before any delivery gives the number back; any other leaves a gap for
/// whoever missed the bag, which is a real loss to that subscriber. Every
/// variant is named so an iceoryx2 upgrade adding one does not compile until
/// it is placed.
pub(super) fn a_send_may_have_delivered(
    send_outcome: &std::result::Result<usize, SendError>,
) -> bool {
    match send_outcome {
        Ok(_) => true,
        Err(
            SendError::ConnectionBrokenSinceSenderNoLongerExists
            | SendError::ConnectionError(_)
            | SendError::LoanError(_),
        ) => false,
        Err(
            SendError::ConnectionCorrupted | SendError::UnableToDeliver | SendError::InternalError,
        ) => true,
    }
}

/// The sequence number the send after one numbered `sent_sequence_number`
/// carries, given whether that send may have delivered. The number wraps to
/// zero past `u64::MAX` in every build profile, which a subscriber reads as no
/// gap.
pub(super) fn sequence_number_following_a_send(
    sent_sequence_number: u64,
    may_have_delivered: bool,
) -> u64 {
    if may_have_delivered {
        sent_sequence_number.wrapping_add(1)
    } else {
        sent_sequence_number
    }
}

/// One source output port's channel egress: the single channel publisher
/// (a channel carries exactly one publisher — see
/// [`streamlib_ipc_types::MAX_PUBLISHERS_PER_CHANNEL`]) and one notifier per
/// destination.
///
/// One source output port maps to one channel, so a single zero-copy loan
/// reaches every subscriber. The per-destination notifiers stay separate
/// because each destination keeps its own listener-fd (the notify service is
/// destination-keyed for fd-multiplexed wakeups); the data itself is published
/// ONCE.
pub(super) struct ChannelEgress {
    pub(super) publisher: ChannelDataServicePublisher,
    /// The channel the publisher sends on, read for which subscribers are
    /// connected to it.
    pub(super) channel: Iceoryx2Service,
    /// Every subscriber connected to the channel just before the last send
    /// that may have delivered; empty before the first.
    pub(super) subscribers_connected_at_the_last_send: Vec<UniqueSubscriberId>,
    /// The sequence number the next send that may deliver carries in its user
    /// header, counted from zero for this publisher's life.
    pub(super) next_sequence_number: u64,
    /// The messages declared lost upstream that the next send that may deliver
    /// carries in its user header.
    pub(super) messages_lost_upstream_before_the_next_send: u64,
    /// Every outbound `connect()` link from this source port. Its length is
    /// what decides when the last link went away and the publisher can be
    /// released.
    pub(super) links: Vec<ChannelEgressLink>,
    /// iceoryx2 service name for this channel (`{source}/{output_port}`) —
    /// carried only for the growth / ceiling tracing fields.
    pub(super) channel_service_name: String,
    /// Trust tier selecting the per-channel ceiling; set at wire time from the
    /// process boundary (host-to-host is trusted; a subprocess destination is
    /// untrusted-session).
    pub(super) trust_tier: ChannelTrustTier,
    /// Per-channel shared-memory chunk ceiling in bytes: the tier default or its
    /// operator override, and so the knob a refusal names.
    pub(super) chunk_ceiling_bytes: usize,
    /// The most bytes one frame on this channel may carry — the chunk ceiling
    /// less what iceoryx2 lays out ahead of a frame in its sample. Derived once
    /// here rather than at each refusal, so the number the decision refuses on
    /// and the number it reports cannot drift. A frame past it is refused with
    /// [`Error::PayloadExceedsChannelCeiling`], counted on
    /// [`Self::bag_counters`], and the stream continues.
    pub(super) largest_admitted_frame_bytes: usize,
    /// Best-effort tracking of the publisher's current data-segment capacity so
    /// a PowerOfTwo growth event is observable. Sample bytes, never frame bytes:
    /// primed to the hint's own sample size, and bumped (to `next_power_of_two`)
    /// the first time a loan's sample exceeds it.
    pub(super) current_slot_capacity_bytes: usize,
    /// This port's share of the writer's refused-bag and written-bag counts.
    pub(super) bag_counters: OutputPortBagCounters,
    /// Set while a loan of this port's publisher is out, so a second loan or a
    /// write on the port is refused by name rather than by iceoryx2's loan
    /// limit.
    pub(super) a_loan_is_open: Arc<AtomicBool>,
    /// Which installation of the port's publisher this is.
    pub(super) installation: u64,
}

/// One outbound `connect()` link from a source output port, and the notifier
/// that wakes its destination.
///
/// The link id tags the entry so a per-link `disconnect` reclaims exactly its
/// own (see
/// [`OutputWriterInner::remove_channel_link`](super::OutputWriterInner::remove_channel_link))
/// rather than the whole fan-out — a source feeding N destinations must keep
/// the other N-1 alive.
pub(super) struct ChannelEgressLink {
    pub(super) link_id: String,
    pub(super) notifier: Notifier<ipc_threadsafe::Service>,
}

/// The channel-egress primitives that prime an output port's channel
/// publisher, passed by value into
/// [`OutputWriterInner::set_channel_publisher`](super::OutputWriterInner::set_channel_publisher).
///
/// Reifying the trust-tier→ceiling coupling in one place: the `trust_tier`
/// selects the process boundary (trusted host-to-host vs. untrusted-session
/// subprocess) and `chunk_ceiling_bytes` is its per-channel chunk ceiling.
pub struct ChannelEgressConfig {
    /// iceoryx2 service name for this channel (`{source}/{output_port}`);
    /// carried for the growth / ceiling tracing fields.
    pub service_name: String,
    /// Trust tier of the process boundary this channel crosses; selects the
    /// per-channel ceiling.
    pub trust_tier: ChannelTrustTier,
    /// Initial expected-payload hint sizing the publisher's data segment.
    pub expected_payload_bytes: usize,
    /// Per-channel shared-memory chunk ceiling in bytes; a frame whose sample
    /// does not fit it is refused.
    pub chunk_ceiling_bytes: usize,
}

impl ChannelEgress {
    /// Admit a `payload_byte_len`-byte message against this channel's ceiling
    /// and loan the sample that carries it, refused and counted over the
    /// ceiling.
    pub(super) fn admit_and_loan(
        &mut self,
        payload_byte_len: usize,
    ) -> Result<ChannelDataServiceLoanedSample> {
        let admission = streamlib_ipc_types::decide_channel_egress_admission(
            payload_byte_len,
            self.chunk_ceiling_bytes,
            &mut self.current_slot_capacity_bytes,
            || self.bag_counters.record_one_refused_bag(),
        );
        streamlib_ipc_types::emit_channel_egress_admission_tracing(
            None,
            self.trust_tier,
            &self.channel_service_name,
            self.chunk_ceiling_bytes,
            payload_byte_len,
            &admission,
        );
        if let streamlib_ipc_types::ChannelEgressAdmission::RefusedOverCeiling { refused_count } =
            admission
        {
            return Err(Error::PayloadExceedsChannelCeiling {
                channel: self.channel_service_name.clone(),
                payload_bytes: payload_byte_len,
                largest_admitted_frame_bytes: self.largest_admitted_frame_bytes,
                chunk_ceiling_bytes: self.chunk_ceiling_bytes,
                tier: trust_tier_label(self.trust_tier),
                refused_bags_on_the_output_port: refused_count,
            });
        }
        let loaned_sample = self
            .publisher
            .loan_slice_uninit(data_channel_payload_cache_lines_for(payload_byte_len))
            .map_err(|e| Error::Link(format!("Failed to loan slice: {:?}", e)))?;
        // SAFETY: the payload area is a chunk of the publisher's shared-memory
        // data segment, mapped from the operating system, so its bytes are
        // initialized: they hold whatever this publisher last wrote to the
        // chunk, or the zeroes the mapping started as. Every writer overwrites
        // the stamped length before the send, and a reader reads no further.
        Ok(unsafe { loaned_sample.assume_init() })
    }

    /// Send `payload_bytes` under `user_header`, copied into one sample this
    /// admits and loans, as [`Self::send_loaned_sample`] does.
    pub(super) fn send_bytes(
        &mut self,
        port: &str,
        payload_bytes: &[u8],
        user_header: DataChannelSampleUserHeader,
    ) -> Result<()> {
        let mut loaned_sample = self.admit_and_loan(payload_bytes.len())?;
        super::payload_area_bytes_mut(&mut loaned_sample)[..payload_bytes.len()]
            .copy_from_slice(payload_bytes);
        self.send_loaned_sample(port, loaned_sample, user_header)
    }

    /// Send `loaned_sample` under `user_header`, numbered as this port's next
    /// send, then wake every destination.
    pub(super) fn send_loaned_sample(
        &mut self,
        port: &str,
        mut loaned_sample: ChannelDataServiceLoanedSample,
        user_header: DataChannelSampleUserHeader,
    ) -> Result<()> {
        *loaned_sample.user_header_mut() = DataChannelSampleUserHeader {
            sequence_number: self.next_sequence_number,
            messages_lost_upstream_before_this_one: self
                .messages_lost_upstream_before_the_next_send,
            ..user_header
        };
        self.channel.collect_connected_subscriber_ids_into(
            &mut self.subscribers_connected_at_the_last_send,
        );
        let send_outcome = loaned_sample.send();
        let may_have_delivered = a_send_may_have_delivered(&send_outcome);
        if !may_have_delivered {
            self.subscribers_connected_at_the_last_send.clear();
        }
        self.next_sequence_number =
            sequence_number_following_a_send(self.next_sequence_number, may_have_delivered);
        if may_have_delivered {
            self.messages_lost_upstream_before_the_next_send = 0;
        }
        send_outcome.map_err(|e| Error::Link(format!("Failed to send sample: {:?}", e)))?;
        self.bag_counters.record_one_written_bag();

        // Wake every downstream listener fd. notify() may transiently fail
        // (e.g. a listener not yet created) — log and continue rather than
        // failing the publish; the data is already in shared memory and the
        // next send() will wake the listener anyway.
        for link in &self.links {
            if let Err(e) = link.notifier.notify() {
                tracing::trace!("OutputWriter: notify() failed for port '{}': {:?}", port, e);
            }
        }
        Ok(())
    }
}
