// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use iceoryx2::port::notifier::Notifier;
use iceoryx2::prelude::*;
use parking_lot::Mutex;

use super::helper_process_loss_count_board::OutputPortBagCountBoardMirror;
use super::loss_counters::BagCountsByOutputPort;
use super::{
    ChannelDataServiceLoanedSample, ChannelDataServicePublisher, DataChannelPayloadDescriptor,
    DataChannelSampleUserHeader, Iceoryx2Service, MessageGroupBoundary,
};
use crate::core::context::StreamClock;
use crate::core::descriptors::PortStreamFacts;
use crate::core::error::{Error, Result};
use crate::core::media_clock::MediaClock;
use crate::core::output_port_stream_facts::StreamFactsOfOutputPorts;

mod channel_egress;
mod output_writer;

pub use channel_egress::ChannelEgressConfig;
use channel_egress::{ChannelEgress, ChannelEgressLink};
pub use output_writer::OutputWriter;

/// Every egress ever installed is numbered from this, so a loan published after
/// its port was unwired and wired again is not sent on the new publisher's
/// numbering.
static NEXT_CHANNEL_EGRESS_INSTALLATION: AtomicU64 = AtomicU64::new(0);

/// The loaned sample's payload area as bytes.
fn payload_area_bytes_mut(loaned_sample: &mut ChannelDataServiceLoanedSample) -> &mut [u8] {
    let cache_lines = loaned_sample.payload_mut();
    // SAFETY: `DataChannelPayloadCacheLine` is `repr(C)` over a byte array and
    // has no padding, so its lines are that many contiguous bytes, borrowed
    // mutably for as long as the sample is.
    unsafe {
        std::slice::from_raw_parts_mut(
            cache_lines.as_mut_ptr().cast::<u8>(),
            std::mem::size_of_val(cache_lines),
        )
    }
}

/// Inner state for an output writer. Owns the per-output-port
/// channel publisher and its destination notifiers; all per-frame publish +
/// notify work runs here.
///
/// Held via `Arc<OutputWriterInner>`; the [`OutputWriter`] handle stores a
/// separate `Arc::into_raw`-encoded strong reference to the same inner.
pub struct OutputWriterInner {
    /// Map from source output port name to its channel egress.
    channels: Mutex<HashMap<String, ChannelEgress>>,
    declared_output_ports: Mutex<HashSet<String>>,
    bag_counts: Arc<BagCountsByOutputPort>,
    stream_facts_of_output_ports: std::sync::OnceLock<Arc<StreamFactsOfOutputPorts>>,
    stream_clock: std::sync::OnceLock<Arc<StreamClock>>,
}

/// The refusal for `what` on `port` while a loan of it is open.
fn the_open_loan_refuses(port: &str, what: &str) -> Error {
    Error::Link(format!(
        "output port '{port}' has a loan open, so {what} on it is refused: publish the loan or \
         give it back first"
    ))
}

/// Clears its port's open-loan flag when the loan it marks ends, published or
/// not.
struct OutputPortOpenLoanMarker(Arc<AtomicBool>);

impl Drop for OutputPortOpenLoanMarker {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Where an [`OutputPortPayloadLoan`]'s bytes live.
enum OutputPortPayloadLoanTarget {
    /// A sample loaned from the port's publisher.
    LoanedSample {
        loaned_sample: ChannelDataServiceLoanedSample,
        egress_installation: u64,
        _loan_marker: OutputPortOpenLoanMarker,
    },
    /// Scratch bytes for a declared port no link has reached; publishing them
    /// sends nothing.
    NoLinkYet { scratch_bytes: Vec<u8> },
}

/// A sample loaned on one output port, filled in place by its writer and then
/// published with [`OutputWriterInner::publish_output_port_payload_loan`] or
/// dropped, which gives it back unpublished.
pub struct OutputPortPayloadLoan {
    output_port: String,
    payload_byte_len: usize,
    loaned_to: OutputPortPayloadLoanTarget,
}

impl OutputPortPayloadLoan {
    /// The output port the loan was taken on.
    pub fn output_port(&self) -> &str {
        &self.output_port
    }

    /// The payload bytes the loan was taken for.
    pub fn payload_byte_len(&self) -> usize {
        self.payload_byte_len
    }

    /// The loan's payload bytes, for its writer to fill. They start as
    /// whatever the sample last carried.
    pub fn payload_bytes_mut(&mut self) -> &mut [u8] {
        let payload_byte_len = self.payload_byte_len;
        match &mut self.loaned_to {
            OutputPortPayloadLoanTarget::LoanedSample { loaned_sample, .. } => {
                &mut payload_area_bytes_mut(loaned_sample)[..payload_byte_len]
            }
            OutputPortPayloadLoanTarget::NoLinkYet { scratch_bytes } => scratch_bytes,
        }
    }
}

impl std::fmt::Debug for OutputPortPayloadLoan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputPortPayloadLoan")
            .field("output_port", &self.output_port)
            .field("payload_byte_len", &self.payload_byte_len)
            .finish_non_exhaustive()
    }
}

impl OutputWriterInner {
    /// Create a new inner with no channels (populated during wiring).
    pub fn new() -> Self {
        Self {
            channels: Mutex::new(HashMap::new()),
            declared_output_ports: Mutex::new(HashSet::new()),
            bag_counts: Arc::new(BagCountsByOutputPort::default()),
            stream_facts_of_output_ports: std::sync::OnceLock::new(),
            stream_clock: std::sync::OnceLock::new(),
        }
    }

    pub(crate) fn reach_the_stream_clock(&self, stream_clock: Arc<StreamClock>) {
        let _ = self.stream_clock.set(stream_clock);
    }

    fn implicit_stamp_now_ns(&self) -> i64 {
        match self.stream_clock.get() {
            Some(stream_clock) => stream_clock.now_ns(),
            None => MediaClock::now().as_nanos() as i64,
        }
    }

    pub(crate) fn reach_stream_facts_of_output_ports(
        &self,
        stream_facts_of_output_ports: Arc<StreamFactsOfOutputPorts>,
    ) {
        if self
            .stream_facts_of_output_ports
            .set(stream_facts_of_output_ports)
            .is_err()
        {
            tracing::warn!(
                "an output writer already reaches its node's stream facts; a second node's are \
                 ignored"
            );
        }
    }

    /// Replace `output_port`'s stream facts, refusing facts of another family
    /// than the port's.
    pub fn set_stream_facts(&self, output_port: &str, stream_facts: PortStreamFacts) -> Result<()> {
        self.stream_facts_of_output_ports
            .get()
            .ok_or_else(|| {
                Error::Link(format!(
                    "output port '{output_port}' has no node to carry its stream facts — the \
                     node was not created for a node"
                ))
            })?
            .replace_facts_of(output_port, stream_facts)
    }

    pub fn declare_output_ports(&self, port_names: impl IntoIterator<Item = String>) {
        self.declared_output_ports.lock().extend(port_names);
    }

    /// Whether a channel publisher has already been installed for this output
    /// port. The compiler op creates the single channel publisher on the FIRST
    /// link out of a source port and only appends notifiers thereafter.
    pub fn has_channel_publisher(&self, output_port: &str) -> bool {
        self.has_port(output_port)
    }

    /// Install the single channel publisher for an output port.
    ///
    /// The [`ChannelEgressConfig`] primes the growth / ceiling observability the
    /// per-frame [`Self::write_bytes_with_timestamp`] enforces. Called once per
    /// output port (the first link out of it); a second call replaces the
    /// publisher, which the wiring op avoids via [`Self::has_channel_publisher`].
    pub fn set_channel_publisher(
        &self,
        output_port: &str,
        channel: &Iceoryx2Service,
        publisher: ChannelDataServicePublisher,
        egress_config: ChannelEgressConfig,
    ) {
        let ChannelEgressConfig {
            service_name,
            trust_tier,
            expected_payload_bytes,
            chunk_ceiling_bytes,
        } = egress_config;
        self.channels.lock().insert(
            output_port.to_string(),
            ChannelEgress {
                publisher,
                channel: channel.clone(),
                subscribers_connected_at_the_last_send: Vec::new(),
                next_sequence_number: 0,
                messages_lost_upstream_before_the_next_send: 0,
                links: Vec::new(),
                channel_service_name: service_name,
                trust_tier,
                chunk_ceiling_bytes,
                largest_admitted_frame_bytes:
                    streamlib_ipc_types::largest_channel_frame_bytes_under_a_chunk_ceiling(
                        chunk_ceiling_bytes,
                    ),
                current_slot_capacity_bytes:
                    streamlib_ipc_types::iceoryx2_sample_bytes_for_a_channel_frame(
                        expected_payload_bytes,
                    ),
                bag_counters: self.bag_counts.counters_for_output_port(output_port),
                a_loan_is_open: Arc::new(AtomicBool::new(false)),
                installation: NEXT_CHANNEL_EGRESS_INSTALLATION.fetch_add(1, Ordering::Relaxed),
            },
        );
    }

    /// Write every new total of `output_port`'s refused-bag and written-bag
    /// counts onto its board entry, as the counts move.
    ///
    /// How a helper process's counts reach its parent. Refused for a port with
    /// no channel publisher, and for one whose channel already mirrors.
    pub fn mirror_an_output_ports_bag_counts_into(
        &self,
        output_port: &str,
        bag_count_board_entry_mirror: OutputPortBagCountBoardMirror,
    ) -> Result<()> {
        let bag_counters = self
            .channels
            .lock()
            .get(output_port)
            .map(|egress| egress.bag_counters.clone())
            .ok_or_else(|| {
                Error::Link(format!(
                    "output port '{output_port}' has no channel publisher, so there are no bag \
                     counts of it to mirror"
                ))
            })?;
        let refused_bag_board_entry_mirror = bag_count_board_entry_mirror.clone();
        bag_counters
            .mirror_every_new_total_into(
                Box::new(move |refused_bags| {
                    refused_bag_board_entry_mirror.mirror_refused_bags(refused_bags)
                }),
                Box::new(move |written_bags| {
                    bag_count_board_entry_mirror.mirror_written_bags(written_bags)
                }),
            )
            .map_err(|_| {
                Error::Link(format!(
                    "output port '{output_port}' already mirrors its bag counts onto a board entry"
                ))
            })
    }

    pub fn bag_counts_by_output_port(&self) -> Arc<BagCountsByOutputPort> {
        Arc::clone(&self.bag_counts)
    }

    /// Record one outbound `connect()` link from this output port, with the
    /// notifier that wakes its destination.
    ///
    /// No-op (the notifier is dropped) if the channel publisher has not been
    /// installed yet, which the wiring op never does.
    pub fn add_channel_link(
        &self,
        output_port: &str,
        link_id: &str,
        notifier: Notifier<ipc_threadsafe::Service>,
    ) {
        if let Some(egress) = self.channels.lock().get_mut(output_port) {
            egress.links.push(ChannelEgressLink {
                link_id: link_id.to_string(),
                notifier,
            });
        }
    }

    /// Number of destination notifiers this output port's channel holds — one
    /// per `connect()` link. Observation surface for tests and diagnostics.
    pub fn channel_notifier_count(&self, output_port: &str) -> usize {
        self.channels
            .lock()
            .get(output_port)
            .map(|egress| egress.links.len())
            .unwrap_or(0)
    }

    /// Whether a reader now connected to `output_port` connected after its last
    /// write, so that write never reached it.
    ///
    /// Every reader of a port subscribes to its one channel — a linked
    /// destination, a viewer on the reserved slot, an island's reader — so this
    /// sees each of them, and sees a link's destination only once its
    /// subscriber is open. A port with no channel has no reader.
    pub fn has_a_reader_the_last_write_missed(&self, output_port: &str) -> bool {
        self.channels.lock().get(output_port).is_some_and(|egress| {
            egress
                .channel
                .has_a_connected_subscriber_outside(&egress.subscribers_connected_at_the_last_send)
        })
    }

    /// Reclaim the source-side egress for one disconnected `connect()` link.
    ///
    /// When the dropped `link_id` notifier was the port's last outbound link, the
    /// whole [`ChannelEgress`] (publisher + data service) is released so this
    /// writer holds nothing against the channel once no link uses it, and a
    /// reconnect never exceeds the notify service's create-time `max_notifiers`
    /// cap (`ExceedsMaxSupportedNotifiers`). The port's refused-bag and
    /// written-bag counts go with it.
    ///
    /// Returns `true` when that last link went away and the publisher was removed.
    pub fn remove_channel_link(&self, output_port: &str, link_id: &str) -> bool {
        let mut channels = self.channels.lock();
        let Some(egress) = channels.get_mut(output_port) else {
            return false;
        };
        egress.links.retain(|link| link.link_id != link_id);
        if egress.links.is_empty() {
            channels.remove(output_port);
            self.bag_counts.forget_output_port(output_port);
            true
        } else {
            false
        }
    }

    /// Publish one message — `payload_bytes` as given, stamped `timestamp_ns` and
    /// `group_boundary` in its header — on `port`.
    ///
    /// The bytes are copied into one loaned sample that reaches every channel
    /// subscriber; it is sent once, then every destination notifier is
    /// signalled. Refused while a loan of the port is open.
    ///
    /// Every send that may deliver carries the port's next sequence number; a
    /// refusal, a failed loan and a send that failed before reaching anyone
    /// consume none.
    pub fn write_bytes_with_timestamp(
        &self,
        port: &str,
        payload_bytes: &[u8],
        timestamp_ns: i64,
        group_boundary: MessageGroupBoundary,
    ) -> Result<()> {
        self.write_bytes_described_by(
            port,
            payload_bytes,
            timestamp_ns,
            group_boundary,
            DataChannelPayloadDescriptor::bytes(),
        )
    }

    /// [`Self::write_bytes_with_timestamp`], the message's header carrying
    /// `payload_descriptor` — a pooled GPU frame's, for one.
    pub fn write_bytes_described_by(
        &self,
        port: &str,
        payload_bytes: &[u8],
        timestamp_ns: i64,
        group_boundary: MessageGroupBoundary,
        payload_descriptor: DataChannelPayloadDescriptor,
    ) -> Result<()> {
        let mut channels = self.channels.lock();
        let Some(egress) = self.linked_egress_of_a_declared_port(&mut channels, port)? else {
            return Ok(());
        };
        if egress.a_loan_is_open.load(Ordering::Acquire) {
            return Err(the_open_loan_refuses(port, "a write"));
        }
        let mut loaned_sample = egress.admit_and_loan(payload_bytes.len())?;
        payload_area_bytes_mut(&mut loaned_sample)[..payload_bytes.len()]
            .copy_from_slice(payload_bytes);
        egress.send_loaned_sample(
            port,
            loaned_sample,
            DataChannelSampleUserHeader::new(
                timestamp_ns,
                payload_bytes.len() as u32,
                group_boundary,
                payload_descriptor,
            ),
        )
    }

    /// Loan a sample of `payload_byte_len` bytes on `port` for its writer to
    /// fill in place, refused while another loan of the port is open.
    ///
    /// The loan is taken under the port lock and held outside it. A declared
    /// port no link has reached hands out scratch bytes its publish discards,
    /// as a write to it sends nothing.
    pub fn loan_output_port_payload(
        &self,
        port: &str,
        payload_byte_len: usize,
    ) -> Result<OutputPortPayloadLoan> {
        let mut channels = self.channels.lock();
        let Some(egress) = self.linked_egress_of_a_declared_port(&mut channels, port)? else {
            return Ok(OutputPortPayloadLoan {
                output_port: port.to_string(),
                payload_byte_len,
                loaned_to: OutputPortPayloadLoanTarget::NoLinkYet {
                    scratch_bytes: vec![0u8; payload_byte_len],
                },
            });
        };
        if egress
            .a_loan_is_open
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(the_open_loan_refuses(port, "a second loan"));
        }
        let loan_marker = OutputPortOpenLoanMarker(Arc::clone(&egress.a_loan_is_open));
        let loaned_sample = egress.admit_and_loan(payload_byte_len)?;
        Ok(OutputPortPayloadLoan {
            output_port: port.to_string(),
            payload_byte_len,
            loaned_to: OutputPortPayloadLoanTarget::LoanedSample {
                loaned_sample,
                egress_installation: egress.installation,
                _loan_marker: loan_marker,
            },
        })
    }

    /// Publish `loan`, stamped `timestamp_ns` and `group_boundary` and described
    /// by `payload_descriptor`, on the port it was loaned from.
    ///
    /// A loan whose port was unwired since, or never linked, is given back
    /// unpublished, as a write to such a port sends nothing.
    pub fn publish_output_port_payload_loan(
        &self,
        loan: OutputPortPayloadLoan,
        timestamp_ns: i64,
        group_boundary: MessageGroupBoundary,
        payload_descriptor: DataChannelPayloadDescriptor,
    ) -> Result<()> {
        let OutputPortPayloadLoan {
            output_port,
            payload_byte_len,
            loaned_to,
        } = loan;
        let OutputPortPayloadLoanTarget::LoanedSample {
            loaned_sample,
            egress_installation,
            _loan_marker,
        } = loaned_to
        else {
            return Ok(());
        };
        let mut channels = self.channels.lock();
        let Some(egress) = channels
            .get_mut(&output_port)
            .filter(|egress| egress.installation == egress_installation)
        else {
            return Ok(());
        };
        egress.send_loaned_sample(
            &output_port,
            loaned_sample,
            DataChannelSampleUserHeader::new(
                timestamp_ns,
                payload_byte_len as u32,
                group_boundary,
                payload_descriptor,
            ),
        )
    }

    fn linked_egress_of_a_declared_port<'channels>(
        &self,
        channels: &'channels mut HashMap<String, ChannelEgress>,
        port: &str,
    ) -> Result<Option<&'channels mut ChannelEgress>> {
        match channels.get_mut(port) {
            Some(egress) => Ok(Some(egress)),
            None if self.declared_output_ports.lock().contains(port) => Ok(None),
            None => Err(Error::Link(format!("Unknown output port: {}", port))),
        }
    }

    /// Declare `given_up_group_count` groups given up upstream of `port` since
    /// its last write: counted on this port at once, and carried to every
    /// `ordered` reader as one lost message each, ahead of the next message
    /// this port sends.
    ///
    /// For engine-internal writers only. A declaration after the port's last
    /// write reaches no reader; this port's count still has it. A declared
    /// port no link has reached counts nothing, as a write to it sends nothing.
    #[doc(hidden)]
    pub fn declare_groups_given_up_upstream_before_the_next_write(
        &self,
        port: &str,
        given_up_group_count: u64,
    ) -> Result<()> {
        let mut channels = self.channels.lock();
        let Some(egress) = self.linked_egress_of_a_declared_port(&mut channels, port)? else {
            return Ok(());
        };
        egress
            .bag_counters
            .record_given_up_groups(given_up_group_count);
        egress.messages_lost_upstream_before_the_next_send = egress
            .messages_lost_upstream_before_the_next_send
            .saturating_add(given_up_group_count);
        Ok(())
    }

    /// Check if a port is configured.
    pub fn has_port(&self, port: &str) -> bool {
        self.channels.lock().contains_key(port)
    }

    /// Get the list of configured output port names.
    pub fn port_names(&self) -> Vec<String> {
        self.channels.lock().keys().cloned().collect()
    }
}

impl Default for OutputWriterInner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
