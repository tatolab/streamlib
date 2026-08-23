// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Output writer for sending frames to downstream processors.
//!
//! # Two-type split: handle vs. inner
//!
//! - [`OutputWriterInner`] holds the actual state — the
//!   `Mutex<HashMap<port, ChannelEgress>>` and the iceoryx2 publish +
//!   notify logic. All per-frame publish + notify work runs here.
//! - [`OutputWriter`] is the public handle that processor structs hold
//!   via the macro-emitted `outputs: OutputWriter` field. It wraps an
//!   `Arc<OutputWriterInner>` behind an opaque handle; its methods
//!   (`write`, `write_raw`, `has_port`, `clone`, `drop`) borrow the
//!   inner and invoke it directly.
//!
//! Host-side code that mutates the inner (e.g. compiler ops installing
//! a channel publisher + destination notifiers at wiring time) operates
//! on `Arc<OutputWriterInner>` directly via
//! [`OutputWriterInner::set_channel_publisher`] and
//! [`OutputWriterInner::add_channel_link`].

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;

use iceoryx2::port::notifier::Notifier;
use iceoryx2::port::publisher::Publisher;
use iceoryx2::prelude::*;
use parking_lot::Mutex;
use serde::Serialize;

use super::{ChannelTrustTier, FRAME_HEADER_SIZE, FrameHeader};
use crate::core::error::{ChannelTrustTierLabel, Error, Result};
use crate::core::media_clock::MediaClock;

/// Map the engine's [`ChannelTrustTier`] onto the engine-free
/// [`ChannelTrustTierLabel`] the [`Error::PayloadExceedsChannelCeiling`] variant
/// carries. Lives at the error boundary because the orphan rule forbids a `From`
/// between the two foreign enums, and it keeps the ceiling error engine-free.
fn trust_tier_label(trust_tier: ChannelTrustTier) -> ChannelTrustTierLabel {
    match trust_tier {
        ChannelTrustTier::Trusted => ChannelTrustTierLabel::Trusted,
        ChannelTrustTier::UntrustedSession => ChannelTrustTierLabel::UntrustedSession,
    }
}

/// View initialized bytes as `MaybeUninit` for writing into a loaned iceoryx2
/// sample (sound: `MaybeUninit<u8>` is `repr(transparent)` over `u8`, and the
/// uninit view is write-only). What [`SampleMutUninit::write_from_slice`] does
/// internally, exposed here so header and payload can fill one loan without a
/// staging copy.
///
/// [`SampleMutUninit::write_from_slice`]: iceoryx2::sample_mut_uninit::SampleMutUninit::write_from_slice
fn as_maybe_uninit_bytes(bytes: &[u8]) -> &[core::mem::MaybeUninit<u8>] {
    // SAFETY: `MaybeUninit<u8>` has the same layout as `u8`; the returned
    // slice is only ever the source of a copy.
    unsafe { core::mem::transmute::<&[u8], &[core::mem::MaybeUninit<u8>]>(bytes) }
}

/// One source output port's channel egress: the single channel publisher
/// (a channel carries exactly one publisher — see
/// [`streamlib_ipc_types::MAX_PUBLISHERS_PER_CHANNEL`]) and one notifier per
/// destination.
///
/// The transport inversion (#1419): one source output port maps to one channel,
/// so a single zero-copy loan reaches every subscriber. The per-destination
/// notifiers stay separate because each destination keeps its own listener-fd
/// (the notify service is destination-keyed for fd-multiplexed wakeups); the
/// data itself is published ONCE.
struct ChannelEgress {
    publisher: Publisher<ipc::Service, [u8], ()>,
    /// Every outbound `connect()` link from this source port. Its length — not
    /// the notifier count — is what decides when the last link went away and
    /// the publisher can be released, because a link whose destination never
    /// drains a listener carries no notifier at all.
    links: Vec<ChannelEgressLink>,
    /// iceoryx2 service name for this channel (`{source}/{output_port}`) —
    /// carried only for the growth / ceiling tracing fields.
    channel_service_name: String,
    /// Trust tier selecting the per-channel ceiling; set at wire time from the
    /// process boundary (host-to-host is trusted; a subprocess destination is
    /// untrusted-session).
    trust_tier: ChannelTrustTier,
    /// Per-channel payload ceiling in bytes. A frame above this is refused with
    /// [`Error::PayloadExceedsChannelCeiling`], counted, and the stream
    /// continues.
    ceiling_bytes: usize,
    /// Best-effort tracking of the publisher's current data-segment capacity so
    /// a PowerOfTwo growth event is observable. Primed to the hint's slot size;
    /// bumped (to `next_power_of_two`) the first time a loan exceeds it.
    current_slot_capacity_bytes: usize,
    /// Count of samples refused for crossing [`Self::ceiling_bytes`].
    refused_over_ceiling_count: u64,
}

/// One outbound `connect()` link from a source output port, and the notifier
/// that wakes its destination.
///
/// The notifier is `None` when that destination never drains a listener — a
/// self-driven sink that polls its mailboxes — because a notification nobody
/// collects fills the listener's queue and then fails delivery on every frame
/// for the rest of the run. The link id tags the entry so a per-link
/// `disconnect` reclaims exactly its own (see
/// [`OutputWriterInner::remove_channel_link`]) rather than the whole fan-out —
/// a source feeding N destinations must keep the other N-1 alive.
struct ChannelEgressLink {
    link_id: String,
    notifier: Option<Notifier<ipc::Service>>,
}

/// The channel-egress primitives that prime an output port's channel
/// publisher, passed by value into [`OutputWriterInner::set_channel_publisher`].
///
/// Reifying the trust-tier→ceiling coupling in one place: the `trust_tier`
/// selects the process boundary (trusted host-to-host vs. untrusted-session
/// subprocess) and `ceiling_bytes` is its per-channel payload ceiling.
pub struct ChannelEgressConfig {
    /// iceoryx2 service name for this channel (`{source}/{output_port}`);
    /// carried for the growth / ceiling tracing fields.
    pub service_name: String,
    /// Trust tier of the process boundary this channel crosses; selects the
    /// per-channel ceiling.
    pub trust_tier: ChannelTrustTier,
    /// Initial expected-payload hint sizing the publisher's data segment.
    pub expected_payload_bytes: usize,
    /// Per-channel payload ceiling in bytes; a frame above it is refused.
    pub ceiling_bytes: usize,
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
}

// OutputWriterInner is Send + Sync via Mutex.
unsafe impl Send for OutputWriterInner {}
unsafe impl Sync for OutputWriterInner {}

impl OutputWriterInner {
    /// Create a new inner with no channels (populated during wiring).
    pub fn new() -> Self {
        Self {
            channels: Mutex::new(HashMap::new()),
        }
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
    /// per-frame [`Self::write_raw`] enforces. Called once per output port (the
    /// first link out of it); a second call replaces the publisher, which the
    /// wiring op avoids via [`Self::has_channel_publisher`].
    pub fn set_channel_publisher(
        &self,
        output_port: &str,
        publisher: Publisher<ipc::Service, [u8], ()>,
        egress_config: ChannelEgressConfig,
    ) {
        let ChannelEgressConfig {
            service_name,
            trust_tier,
            expected_payload_bytes,
            ceiling_bytes,
        } = egress_config;
        self.channels.lock().insert(
            output_port.to_string(),
            ChannelEgress {
                publisher,
                links: Vec::new(),
                channel_service_name: service_name,
                trust_tier,
                ceiling_bytes,
                current_slot_capacity_bytes: expected_payload_bytes + FRAME_HEADER_SIZE,
                refused_over_ceiling_count: 0,
            },
        );
    }

    /// Number of samples this output port's channel refused for crossing its
    /// per-channel ceiling. Observation surface for tests and diagnostics.
    pub fn refused_over_ceiling_count(&self, output_port: &str) -> u64 {
        self.channels
            .lock()
            .get(output_port)
            .map(|e| e.refused_over_ceiling_count)
            .unwrap_or(0)
    }

    /// Record one outbound `connect()` link from this output port, with the
    /// notifier that wakes its destination.
    ///
    /// Pass `None` when the destination never drains a listener; the link is
    /// then carried for reclaim bookkeeping and notified on no frame. No-op
    /// (the notifier is dropped) if the channel publisher has not been
    /// installed yet, which the wiring op never does.
    pub fn add_channel_link(
        &self,
        output_port: &str,
        link_id: &str,
        notifier: Option<Notifier<ipc::Service>>,
    ) {
        if let Some(egress) = self.channels.lock().get_mut(output_port) {
            egress.links.push(ChannelEgressLink {
                link_id: link_id.to_string(),
                notifier,
            });
        }
    }

    /// Number of destination notifiers this output port's channel holds — one
    /// per `connect()` link whose destination waits on a listener. Observation
    /// surface for tests and diagnostics.
    pub fn channel_notifier_count(&self, output_port: &str) -> usize {
        self.channels
            .lock()
            .get(output_port)
            .map(|egress| egress.links.iter().filter(|l| l.notifier.is_some()).count())
            .unwrap_or(0)
    }

    /// Reclaim the source-side egress for one disconnected `connect()` link.
    ///
    /// When the dropped `link_id` notifier was the port's last outbound link, the
    /// whole [`ChannelEgress`] (publisher + data service) is released so a reconnect
    /// recreates a fresh-sized, refcounted service rather than colliding with the
    /// stale one (`DoesNotSupportRequestedMinBufferSize`) or exceeding the notify
    /// service's create-time `max_notifiers` cap (`ExceedsMaxSupportedNotifiers`).
    ///
    /// Returns `true` when that last link went away and the publisher was removed.
    pub fn remove_channel_link(&self, output_port: &str, link_id: &str) -> bool {
        let mut channels = self.channels.lock();
        let Some(egress) = channels.get_mut(output_port) else {
            return false;
        };
        // Keyed on the links, not the notifiers: a fan-out mixing destinations
        // that wait with destinations that poll holds fewer notifiers than
        // links, and releasing the publisher on the last *notifier* would cut
        // off the polling destinations still connected.
        egress.links.retain(|link| link.link_id != link_id);
        if egress.links.is_empty() {
            channels.remove(output_port);
            true
        } else {
            false
        }
    }

    /// Write raw bytes to the specified output port without serialization.
    ///
    /// The data is assumed to be pre-serialized (e.g., msgpack from a
    /// subprocess bridge). One zero-copy loan reaches every channel subscriber;
    /// the frame is built and sent ONCE, then every destination notifier is
    /// signalled.
    pub fn write_raw(&self, port: &str, data: &[u8], timestamp_ns: i64) -> Result<()> {
        let mut channels = self.channels.lock();
        let egress = channels
            .get_mut(port)
            .ok_or_else(|| Error::Link(format!("Unknown output port: {}", port)))?;

        let total_len = FRAME_HEADER_SIZE + data.len();

        // Per-channel ceiling refusal + PowerOfTwo growth bookkeeping share their
        // authority with the subprocess natives via
        // `decide_channel_egress_admission`; the host layers its typed error and
        // the quarter-of-ceiling warning on top of the shared decision.
        let admission = streamlib_ipc_types::decide_channel_egress_admission(
            total_len,
            egress.ceiling_bytes,
            &mut egress.refused_over_ceiling_count,
            &mut egress.current_slot_capacity_bytes,
        );
        streamlib_ipc_types::emit_channel_egress_admission_tracing(
            None,
            egress.trust_tier,
            &egress.channel_service_name,
            egress.ceiling_bytes,
            total_len,
            &admission,
        );
        if let streamlib_ipc_types::ChannelEgressAdmission::RefusedOverCeiling { .. } = admission {
            return Err(Error::PayloadExceedsChannelCeiling {
                channel: egress.channel_service_name.clone(),
                payload_bytes: total_len,
                ceiling_bytes: egress.ceiling_bytes,
                tier: trust_tier_label(egress.trust_tier),
            });
        }

        // Header on the stack before the loan: a port key that overflows the
        // wire capacity must never cost a loan slot.
        let mut header_bytes = [0u8; FRAME_HEADER_SIZE];
        FrameHeader::new(port, timestamp_ns, data.len() as u32)
            .map_err(|e| Error::Link(format!("output port '{}': {}", port, e)))?
            .write_to_slice(&mut header_bytes);

        // Header and payload are written straight into the loan — no staging
        // buffer, no second payload copy. `copy_from_slice` panics on a length
        // mismatch, so the two writes cover all `total_len` loaned bytes.
        let mut sample = egress
            .publisher
            .loan_slice_uninit(total_len)
            .map_err(|e| Error::Link(format!("Failed to loan slice: {:?}", e)))?;
        let (header_dst, payload_dst) = sample.payload_mut().split_at_mut(FRAME_HEADER_SIZE);
        header_dst.copy_from_slice(as_maybe_uninit_bytes(&header_bytes));
        payload_dst.copy_from_slice(as_maybe_uninit_bytes(data));
        // SAFETY: the two `copy_from_slice` calls above initialized
        // `FRAME_HEADER_SIZE + data.len()` bytes — exactly the `total_len` the
        // loan was taken for.
        let sample = unsafe { sample.assume_init() };
        sample
            .send()
            .map_err(|e| Error::Link(format!("Failed to send sample: {:?}", e)))?;

        // Wake every downstream listener fd. notify() may transiently fail
        // (e.g. a listener not yet created) — log and continue rather than
        // failing the publish; the data is already in shared memory and the
        // next send() will wake the listener anyway.
        for notifier in egress
            .links
            .iter()
            .filter_map(|link| link.notifier.as_ref())
        {
            if let Err(e) = notifier.notify() {
                tracing::trace!("OutputWriter: notify() failed for port '{}': {:?}", port, e);
            }
        }

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

// =============================================================================
// OutputWriter
// =============================================================================

/// Public output writer handle. The macro emits
/// `pub outputs: OutputWriter` on every processor struct that
/// declares output ports.
///
/// The sole field is an opaque pointer to the host's
/// [`OutputWriterInner`]. `Clone` bumps the `Arc<OutputWriterInner>`
/// strong count; `Drop` decrements it.
pub struct OutputWriter {
    /// Opaque handle: `Arc::into_raw(Arc<OutputWriterInner>)`. Null
    /// on a freshly-constructed processor before
    /// `set_iceoryx2_resources` fires.
    pub(crate) handle: *const c_void,
}

// SAFETY: `handle` points at an `Arc<OutputWriterInner>` whose
// interior is Send+Sync (OutputWriterInner declares both above).
unsafe impl Send for OutputWriter {}
unsafe impl Sync for OutputWriter {}

impl OutputWriter {
    /// Build a handle from an `Arc<OutputWriterInner>`. The strong
    /// reference is consumed; the handle owns it for its lifetime and
    /// releases on Drop.
    ///
    /// Engine-only — used by the processor wiring path
    /// (`ProcessorInstanceFactory::install_iceoryx2_resources`) and
    /// by the macro-emitted `from_config` initializer when no
    /// outputs are declared (an empty inner is used).
    pub fn from_inner_arc(inner: Arc<OutputWriterInner>) -> Self {
        let handle = Arc::into_raw(inner) as *const c_void;
        Self { handle }
    }

    /// Engine-internal borrow of the `OutputWriterInner`, or `None`
    /// when unwired.
    fn host_inner(&self) -> Option<&OutputWriterInner> {
        if self.handle.is_null() {
            return None;
        }
        // SAFETY: `handle` is `Arc::into_raw(Arc<OutputWriterInner>)`.
        Some(unsafe { &*(self.handle as *const OutputWriterInner) })
    }

    /// Build an empty pre-wiring handle with a null handle pointer. The
    /// engine patches in a real inner via
    /// `GeneratedProcessor::set_iceoryx2_resources` before any
    /// downstream connection wiring runs. Safe to hold before wiring —
    /// `has_port` / `is_configured` answer without a wired inner — but
    /// `write` / `write_raw` return [`Error::Link`] until it fires.
    pub fn empty() -> Self {
        Self {
            handle: std::ptr::null(),
        }
    }

    /// Returns true iff this has been wired to a real inner.
    pub fn is_configured(&self) -> bool {
        !self.handle.is_null()
    }

    /// Borrow the `Arc<OutputWriterInner>` this handle points at. Returns
    /// `None` for unwired handles. Bumps the strong count; the returned
    /// Arc balances with one Drop on the inner.
    ///
    /// Engine-only (used by the macro-emitted
    /// `iceoryx2_output_writer_inner` trait method to expose the
    /// wiring path to compiler ops).
    pub fn inner_arc(&self) -> Option<Arc<OutputWriterInner>> {
        if !self.is_configured() {
            return None;
        }
        // SAFETY: `handle` is `Arc::into_raw(Arc<OutputWriterInner>)`; bump
        // the strong count and reconstruct an owning `Arc` from the raw handle.
        unsafe {
            Arc::increment_strong_count(self.handle as *const OutputWriterInner);
            Some(Arc::from_raw(self.handle as *const OutputWriterInner))
        }
    }

    /// Write a frame to the specified output port. Serializes `T` to
    /// msgpack, then publishes the bytes. Thread-safe.
    pub fn write<T: Serialize>(&self, port: &str, value: &T) -> Result<()> {
        let timestamp_ns = MediaClock::now().as_nanos() as i64;
        self.write_with_timestamp(port, value, timestamp_ns)
    }

    /// Write a frame to the specified output port with an
    /// explicit timestamp.
    pub fn write_with_timestamp<T: Serialize>(
        &self,
        port: &str,
        value: &T,
        timestamp_ns: i64,
    ) -> Result<()> {
        let data = rmp_serde::to_vec_named(value)
            .map_err(|e| Error::Link(format!("Failed to serialize frame: {}", e)))?;
        self.write_raw(port, &data, timestamp_ns)
    }

    /// Write raw msgpack-encoded bytes to the specified output port.
    pub fn write_raw(&self, port: &str, data: &[u8], timestamp_ns: i64) -> Result<()> {
        let Some(inner) = self.host_inner() else {
            return Err(Error::Link(format!(
                "OutputWriter not wired (port='{}'): host has not yet \
                 installed iceoryx2 resources on this processor instance",
                port
            )));
        };
        inner.write_raw(port, data, timestamp_ns)
    }

    /// Check if a port is configured.
    pub fn has_port(&self, port: &str) -> bool {
        match self.host_inner() {
            Some(inner) => inner.has_port(port),
            None => false,
        }
    }
}

impl Default for OutputWriter {
    fn default() -> Self {
        Self::empty()
    }
}

impl Clone for OutputWriter {
    fn clone(&self) -> Self {
        if !self.is_configured() {
            return Self::empty();
        }
        // SAFETY: `handle` is `Arc::into_raw(Arc<OutputWriterInner>)`; bump
        // the strong count so both handles own one reference.
        unsafe {
            Arc::increment_strong_count(self.handle as *const OutputWriterInner);
        }
        Self {
            handle: self.handle,
        }
    }
}

impl Drop for OutputWriter {
    fn drop(&mut self) {
        if !self.is_configured() {
            return;
        }
        // SAFETY: `handle` is `Arc::into_raw(Arc<OutputWriterInner>)`.
        unsafe {
            drop(Arc::from_raw(self.handle as *const OutputWriterInner));
        }
        self.handle = std::ptr::null();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;

    /// Each test gets a unique service-name prefix so parallel invocations
    /// don't collide on iceoryx2's machine-global `/dev/shm` namespace.
    fn unique_suffix(tag: &str) -> String {
        format!(
            "test/output/{tag}/{}",
            mint_machine_global_unique_name_suffix()
        )
    }

    #[test]
    fn write_raw_calls_notifier() {
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let pubsub_name = unique_suffix("pubsub");
        let notify_name = unique_suffix("notify");

        let pubsub = node
            .service_builder(&ServiceName::new(&pubsub_name).unwrap())
            .publish_subscribe::<[u8]>()
            .max_publishers(2)
            .open_or_create()
            .unwrap();
        let publisher = pubsub
            .publisher_builder()
            .initial_max_slice_len(4096)
            .create()
            .unwrap();
        let _subscriber = pubsub.subscriber_builder().create().unwrap();

        let notify = node
            .service_builder(&ServiceName::new(&notify_name).unwrap())
            .event()
            .max_notifiers(2)
            .max_listeners(1)
            .open_or_create()
            .unwrap();
        let notifier = notify.notifier_builder().create().unwrap();
        let listener = notify.listener_builder().create().unwrap();

        let inner = Arc::new(OutputWriterInner::new());
        inner.set_channel_publisher(
            "out",
            publisher,
            ChannelEgressConfig {
                service_name: "test/out".to_string(),
                trust_tier: crate::iceoryx2::ChannelTrustTier::Trusted,
                expected_payload_bytes: 4096,
                ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
            },
        );
        inner.add_channel_link("out", "L-test-notify", Some(notifier));

        // Pre-flight: the listener has no events queued.
        let mut count: usize = 0;
        listener.try_wait_all(|_| count += 1).unwrap();
        assert_eq!(count, 0);

        let writer = OutputWriter::from_inner_arc(inner);
        writer.write_raw("out", b"payload", 1234).unwrap();
        writer.write_raw("out", b"more", 5678).unwrap();

        // Notifier::notify is non-blocking; give iceoryx2 a moment to deliver
        // before draining. timed_wait_all returns as soon as the first event
        // arrives, so the deadline is generous, not the typical wait time.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while count == 0 && std::time::Instant::now() < deadline {
            listener
                .timed_wait_all(|_| count += 1, std::time::Duration::from_millis(50))
                .unwrap();
        }
        // Drain anything still pending.
        listener.try_wait_all(|_| count += 1).unwrap();
        assert!(
            count >= 1,
            "expected at least one notify after write_raw, got {}",
            count
        );
    }

    /// A source port fanning out to a mix of destinations — one that waits on a
    /// listener, one that polls — holds fewer notifiers than links. Releasing
    /// the publisher when the last *notifier* goes would cut the polling
    /// destination off mid-stream, so the release keys on the links.
    #[test]
    fn a_fan_out_holds_its_publisher_until_the_last_link_goes_not_the_last_notifier() {
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let pubsub_name = unique_suffix("mixed-fanout/pubsub");
        let notify_name = unique_suffix("mixed-fanout/notify");

        let pubsub = node
            .service_builder(&ServiceName::new(&pubsub_name).unwrap())
            .publish_subscribe::<[u8]>()
            .max_publishers(2)
            .open_or_create()
            .unwrap();
        let publisher = pubsub
            .publisher_builder()
            .initial_max_slice_len(4096)
            .create()
            .unwrap();
        let notify = node
            .service_builder(&ServiceName::new(&notify_name).unwrap())
            .event()
            .max_notifiers(2)
            .max_listeners(1)
            .open_or_create()
            .unwrap();

        let inner = Arc::new(OutputWriterInner::new());
        inner.set_channel_publisher(
            "out",
            publisher,
            ChannelEgressConfig {
                service_name: "test/mixed-fanout".to_string(),
                trust_tier: crate::iceoryx2::ChannelTrustTier::Trusted,
                expected_payload_bytes: 4096,
                ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
            },
        );

        // The waiting destination brings a notifier; the polling one does not.
        inner.add_channel_link(
            "out",
            "L-waits",
            Some(notify.notifier_builder().create().unwrap()),
        );
        inner.add_channel_link("out", "L-polls", None);

        assert!(
            !inner.remove_channel_link("out", "L-waits"),
            "the polling destination is still connected, so the publisher must survive"
        );
        assert!(
            inner.has_channel_publisher("out"),
            "dropping the only notifier must not take the channel down with it"
        );
        assert_eq!(inner.channel_notifier_count("out"), 0);

        assert!(
            inner.remove_channel_link("out", "L-polls"),
            "the last link going away must release the publisher"
        );
        assert!(!inner.has_channel_publisher("out"));
    }

    /// Why a notifier aimed at a destination that never drains is a defect and
    /// not merely waste: iceoryx2 posts to the listener's signal mechanism on
    /// every `notify()`, so an undrained listener's queue fills after a bounded
    /// number of sends and every send after that fails to deliver —
    /// permanently, since nothing will ever drain it. Drain the same listener
    /// and the same send count delivers every time.
    ///
    /// The observable is `notify()`'s `Ok(usize)` — the number of listeners it
    /// actually triggered. A failed delivery does not surface as `Err`:
    /// iceoryx2 logs its own per-connection warning (a `{:?}` of the whole
    /// `Notifier`, kilobytes wide) and returns a count that excludes the
    /// listener it could not reach. That swallowed count is why the flood in
    /// #1764 ran for the life of the process with the engine none the wiser.
    #[test]
    fn an_undrained_listener_stops_being_delivered_to_a_drained_one_never_does() {
        // Well past any plausible queue depth — the ticket measured the onset
        // at ~280 notifications against the default socket buffer.
        const SENDS: usize = 8192;

        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();

        let open_notify_service = |name: &str| {
            node.service_builder(&ServiceName::new(name).unwrap())
                .event()
                .max_notifiers(1)
                .max_listeners(1)
                .open_or_create()
                .unwrap()
        };

        let undrained = open_notify_service(&unique_suffix("saturation/undrained"));
        let undrained_notifier = undrained.notifier_builder().create().unwrap();
        let _undrained_listener = undrained.listener_builder().create().unwrap();

        let mut sends_until_undeliverable = None;
        for send in 0..SENDS {
            if undrained_notifier.notify().unwrap() == 0 {
                sends_until_undeliverable = Some(send);
                break;
            }
        }
        let saturated_at = sends_until_undeliverable.unwrap_or_else(|| {
            panic!("an undrained listener absorbed {SENDS} notifications and still took more")
        });

        // Once full it stays full: this is what turns a one-off warning into a
        // per-frame flood for the rest of the run.
        assert_eq!(
            undrained_notifier.notify().unwrap(),
            0,
            "delivery recovered after saturating at send {saturated_at} with nothing draining"
        );

        let drained = open_notify_service(&unique_suffix("saturation/drained"));
        let drained_notifier = drained.notifier_builder().create().unwrap();
        let drained_listener = drained.listener_builder().create().unwrap();

        for send in 0..SENDS {
            assert_eq!(
                drained_notifier.notify().unwrap(),
                1,
                "send {send} reached no listener despite the listener being drained every time"
            );
            drained_listener.try_wait_all(|_| {}).unwrap();
        }
    }

    /// 1→N fan-out DELIVERY lock (#1419): a single `write_raw` publishes ONE
    /// frame that reaches EVERY subscriber on the channel through one zero-copy
    /// loan + one send. Three subscribers each receive exactly one copy of the
    /// payload.
    ///
    /// Revert lock: change `write_raw` to a per-connection copy loop (loan +
    /// send once per destination notifier) and each subscriber on the shared
    /// service receives N frames instead of one — the exactly-one assertion
    /// fails. The capacity-only tests (subscriber-count sizing) stay green under
    /// that revert; this is what pins the delivery guarantee.
    #[test]
    fn write_raw_fans_out_single_loan_to_all_subscribers() {
        const N: usize = 3;
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let pubsub_name = unique_suffix("fanout/pubsub");

        let pubsub = node
            .service_builder(&ServiceName::new(&pubsub_name).unwrap())
            .publish_subscribe::<[u8]>()
            .max_publishers(2)
            .max_subscribers(N + 1)
            .open_or_create()
            .unwrap();
        let publisher = pubsub
            .publisher_builder()
            .initial_max_slice_len(4096)
            .create()
            .unwrap();
        let subscribers: Vec<_> = (0..N)
            .map(|_| pubsub.subscriber_builder().create().unwrap())
            .collect();

        let inner = Arc::new(OutputWriterInner::new());
        inner.set_channel_publisher(
            "out",
            publisher,
            ChannelEgressConfig {
                service_name: "test/out".to_string(),
                trust_tier: crate::iceoryx2::ChannelTrustTier::Trusted,
                expected_payload_bytes: 4096,
                ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
            },
        );

        // N destination notifiers on the one channel — the compiler op's wiring
        // shape for N destinations. `write_raw` signals each, but the frame is
        // published exactly ONCE.
        let mut listeners = Vec::with_capacity(N);
        for i in 0..N {
            let notify = node
                .service_builder(
                    &ServiceName::new(&unique_suffix(&format!("fanout/notify/{i}"))).unwrap(),
                )
                .event()
                .max_notifiers(2)
                .max_listeners(1)
                .open_or_create()
                .unwrap();
            inner.add_channel_link(
                "out",
                &format!("L-test-fanout-{i}"),
                Some(notify.notifier_builder().create().unwrap()),
            );
            listeners.push(notify.listener_builder().create().unwrap());
        }

        let writer = OutputWriter::from_inner_arc(inner);
        writer.write_raw("out", b"fanout-payload", 4242).unwrap();

        for (i, subscriber) in subscribers.iter().enumerate() {
            let mut received: Vec<Vec<u8>> = Vec::new();
            while let Ok(Some(sample)) = subscriber.receive() {
                let slice: &[u8] = sample.payload();
                received.push(slice[FRAME_HEADER_SIZE..].to_vec());
            }
            assert_eq!(
                received.len(),
                1,
                "subscriber {i} must receive exactly one frame from a single-loan \
                 fan-out (a per-connection copy loop would deliver {N}), got {}",
                received.len()
            );
            assert_eq!(
                received[0], b"fanout-payload",
                "subscriber {i} received the wrong payload",
            );
        }
    }

    /// Per-link source reclaim (#1549): a source port feeding two destination
    /// links that both wait on a listener holds two tagged notifiers on ONE
    /// channel egress. Disconnecting one link drops only its notifier (the
    /// channel — and its publisher — survive so the other destination keeps
    /// receiving); disconnecting the last link removes the whole channel egress,
    /// releasing the publisher so a reconnect recreates a fresh-sized service.
    ///
    /// The mixed fan-out, where a destination carries no notifier at all, is
    /// locked separately by
    /// [`a_fan_out_holds_its_publisher_until_the_last_link_goes_not_the_last_notifier`].
    ///
    /// Fail-without-fix: revert `remove_channel_link` to a no-op (the pre-#1549
    /// `close_iceoryx2_service` behaviour) and the first removal leaves both
    /// notifiers, so `has_channel_publisher` stays true after the final
    /// disconnect and the "channel fully removed" assertion fails.
    #[test]
    fn remove_channel_link_reclaims_per_link_then_drops_channel() {
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let pubsub = node
            .service_builder(&ServiceName::new(&unique_suffix("reclaim/pubsub")).unwrap())
            .publish_subscribe::<[u8]>()
            .max_publishers(2)
            .max_subscribers(4)
            .open_or_create()
            .unwrap();
        let publisher = pubsub
            .publisher_builder()
            .initial_max_slice_len(4096)
            .create()
            .unwrap();

        let inner = Arc::new(OutputWriterInner::new());
        inner.set_channel_publisher(
            "out",
            publisher,
            ChannelEgressConfig {
                service_name: "test/reclaim/out".to_string(),
                trust_tier: ChannelTrustTier::Trusted,
                expected_payload_bytes: 4096,
                ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
            },
        );

        let notify = |tag: &str| {
            node.service_builder(&ServiceName::new(&unique_suffix(tag)).unwrap())
                .event()
                .max_notifiers(2)
                .max_listeners(1)
                .open_or_create()
                .unwrap()
                .notifier_builder()
                .create()
                .unwrap()
        };
        inner.add_channel_link("out", "L-link-a", Some(notify("reclaim/notify/a")));
        inner.add_channel_link("out", "L-link-b", Some(notify("reclaim/notify/b")));
        assert!(inner.has_channel_publisher("out"));

        // Disconnect one of two links: the channel (and publisher) survive.
        assert!(
            !inner.remove_channel_link("out", "L-link-a"),
            "removing one of two links must NOT drop the shared channel publisher",
        );
        assert!(
            inner.has_channel_publisher("out"),
            "the channel must stay wired while link-b is still connected",
        );

        // A stale/unknown link id is a no-op that keeps the channel.
        assert!(!inner.remove_channel_link("out", "L-does-not-exist"));
        assert!(inner.has_channel_publisher("out"));

        // Disconnect the last link: the whole channel egress is reclaimed.
        assert!(
            inner.remove_channel_link("out", "L-link-b"),
            "removing the last outbound link must drop the channel publisher",
        );
        assert!(
            !inner.has_channel_publisher("out"),
            "the publisher (and its data service) must be released after the final \
             disconnect so a reconnect recreates a fresh-sized service",
        );
    }

    /// Empty (unwired) writers should fail cleanly rather than crash.
    /// Mentally revert the `is_configured()` guard in `write_raw` and
    /// the test segfaults dereferencing the null handle.
    #[test]
    fn empty_writer_fails_cleanly() {
        let writer = OutputWriter::empty();
        assert!(!writer.is_configured());
        let err = writer.write_raw("any_port", b"data", 0).unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("not wired"),
            "unexpected error message: {}",
            msg
        );
        assert!(!writer.has_port("any_port"));
    }

    /// Clone bumps the strong count; both clones drop
    /// independently. Mentally revert the strong-count bump in
    /// `Clone::clone` and the second clone observes a freed handle.
    #[test]
    fn clone_balances_drop() {
        let inner = Arc::new(OutputWriterInner::new());
        // Bump strong count once so we can observe the post-Drop
        // strong-count drop without freeing the inner.
        let inner_for_test = inner.clone();
        let writer1 = OutputWriter::from_inner_arc(inner);
        // strong_count is 2 here: writer1's into_raw + inner_for_test.
        assert_eq!(Arc::strong_count(&inner_for_test), 2);
        let writer2 = writer1.clone();
        assert_eq!(Arc::strong_count(&inner_for_test), 3);
        drop(writer2);
        assert_eq!(Arc::strong_count(&inner_for_test), 2);
        drop(writer1);
        assert_eq!(Arc::strong_count(&inner_for_test), 1);
    }

    /// Per-channel ceiling + PowerOfTwo growth (#1421): a payload UNDER the
    /// channel ceiling but far over the primed slot grows the segment and
    /// delivers intact; a payload OVER the ceiling is refused with the named
    /// [`Error::PayloadExceedsChannelCeiling`], counted, and the stream
    /// continues (a subsequent in-bounds write still delivers).
    ///
    /// Fail-without-fix: remove the ceiling branch in `write_raw` and the
    /// over-ceiling write returns `Ok` (no error, count stays 0) — the graceful
    /// refusal this issue adds is gone. Remove the growth/PowerOfTwo path and the
    /// 100 KiB loan fails instead of delivering.
    #[test]
    fn write_raw_refuses_over_ceiling_and_grows_within_it() {
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let pubsub = node
            .service_builder(&ServiceName::new(&unique_suffix("ceiling/pubsub")).unwrap())
            .publish_subscribe::<[u8]>()
            .max_publishers(2)
            .max_subscribers(2)
            .open_or_create()
            .unwrap();
        // Prime tiny (4 KiB) under PowerOfTwo so a 100 KiB write must grow.
        let publisher = pubsub
            .publisher_builder()
            .initial_max_slice_len(4096)
            .allocation_strategy(AllocationStrategy::PowerOfTwo)
            .create()
            .unwrap();
        let subscriber = pubsub.subscriber_builder().create().unwrap();

        let inner = Arc::new(OutputWriterInner::new());
        let ceiling = 128 * 1024usize;
        inner.set_channel_publisher(
            "out",
            publisher,
            ChannelEgressConfig {
                service_name: "test/ceiling/out".to_string(),
                trust_tier: ChannelTrustTier::UntrustedSession,
                expected_payload_bytes: 64,
                ceiling_bytes: ceiling,
            },
        );

        // Within ceiling but far above the primed 4 KiB slot — grows + delivers.
        let within = vec![0x5Au8; 100 * 1024];
        inner
            .write_raw("out", &within, 111)
            .expect("in-bounds payload must grow the segment and send");
        assert_eq!(
            inner.refused_over_ceiling_count("out"),
            0,
            "an in-bounds payload must not be counted as refused"
        );
        let got = subscriber
            .receive()
            .expect("receive")
            .expect("grown in-bounds frame must be delivered");
        assert_eq!(got.payload().len(), FRAME_HEADER_SIZE + within.len());

        // Above ceiling — refused with the named error + counted, never a panic.
        let over = vec![0u8; ceiling + 1];
        let err = inner
            .write_raw("out", &over, 222)
            .expect_err("a payload above the channel ceiling must be refused");
        match err {
            Error::PayloadExceedsChannelCeiling {
                ref channel,
                payload_bytes,
                ceiling_bytes,
                tier,
            } => {
                assert_eq!(channel, "test/ceiling/out");
                assert_eq!(payload_bytes, FRAME_HEADER_SIZE + over.len());
                assert_eq!(ceiling_bytes, ceiling);
                assert_eq!(tier, ChannelTrustTierLabel::UntrustedSession);
            }
            other => panic!("expected PayloadExceedsChannelCeiling, got {other:?}"),
        }
        assert_eq!(
            inner.refused_over_ceiling_count("out"),
            1,
            "the refused sample must be counted"
        );

        // Stream continues: a subsequent in-bounds write still delivers.
        inner
            .write_raw("out", b"still-alive", 333)
            .expect("stream must continue after a refusal");
        let got = subscriber
            .receive()
            .expect("receive")
            .expect("post-refusal frame must be delivered");
        assert_eq!(
            got.payload().len(),
            FRAME_HEADER_SIZE + b"still-alive".len()
        );
    }

    /// Drift guard for the two trust-tier spellings: `ChannelTrustTier::as_str`
    /// (ipc-types) and `ChannelTrustTierLabel`'s `Display` (the engine-free error
    /// crate) name the tier independently — a forced layering cost (the orphan
    /// rule + engine purity keep the error crate off ipc-types), not duplication
    /// to merge. The engine is the one place that sees both, so it locks the two
    /// string forms — and `trust_tier_label`'s mapping between them — so they
    /// can't silently drift.
    #[test]
    fn trust_tier_label_spellings_do_not_drift() {
        for tier in [
            ChannelTrustTier::Trusted,
            ChannelTrustTier::UntrustedSession,
        ] {
            let label = trust_tier_label(tier);
            assert_eq!(
                tier.as_str(),
                label.to_string(),
                "ChannelTrustTier::as_str must match ChannelTrustTierLabel's Display",
            );
        }
        // Pin the exact variant pairing too, so a swapped mapping is caught.
        assert_eq!(
            ChannelTrustTier::Trusted.as_str(),
            ChannelTrustTierLabel::Trusted.to_string()
        );
        assert_eq!(
            ChannelTrustTier::UntrustedSession.as_str(),
            ChannelTrustTierLabel::UntrustedSession.to_string()
        );
    }

    /// Full-initialization lock for the loan-direct write path: a frame
    /// written after a larger one has cycled through the same publisher must
    /// deliver exactly header + payload — never bytes a previous tenant left
    /// in the recycled chunk.
    ///
    /// Fail-without-fix: skip (or short-write) either region of the loan and
    /// the received slice reads back the earlier frame's 0xAA filler where
    /// fresh bytes belong.
    #[test]
    fn a_frame_written_after_a_larger_one_carries_no_stale_bytes() {
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let pubsub = node
            .service_builder(&ServiceName::new(&unique_suffix("stale/pubsub")).unwrap())
            .publish_subscribe::<[u8]>()
            .max_publishers(2)
            .max_subscribers(2)
            .open_or_create()
            .unwrap();
        let publisher = pubsub
            .publisher_builder()
            .initial_max_slice_len(16 * 1024)
            .create()
            .unwrap();
        let subscriber = pubsub.subscriber_builder().create().unwrap();

        let inner = Arc::new(OutputWriterInner::new());
        inner.set_channel_publisher(
            "out",
            publisher,
            ChannelEgressConfig {
                service_name: "test/stale/out".to_string(),
                trust_tier: ChannelTrustTier::Trusted,
                expected_payload_bytes: 4096,
                ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
            },
        );

        // A large all-0xAA frame primes the publisher's chunk pool with
        // recognisable filler, then goes back to the pool for reuse.
        let filler = vec![0xAAu8; 8 * 1024];
        inner.write_raw("out", &filler, 1).unwrap();
        drop(subscriber.receive().expect("receive").expect("filler frame"));

        let payload = b"fresh-small-payload";
        inner.write_raw("out", payload, 4242).unwrap();
        let got = subscriber
            .receive()
            .expect("receive")
            .expect("fresh frame must deliver");
        let slice: &[u8] = got.payload();
        assert_eq!(
            slice.len(),
            FRAME_HEADER_SIZE + payload.len(),
            "the loan must be sized to exactly header + payload"
        );
        let header = FrameHeader::read_from_slice(slice);
        assert_eq!(header.port(), "out");
        assert_eq!(header.timestamp_ns, 4242);
        assert_eq!(header.len as usize, payload.len());
        assert_eq!(&slice[FRAME_HEADER_SIZE..], payload);
    }
}
