// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::Arc;

use iceoryx2::identifiers::UniquePublisherId;
use iceoryx2::port::listener::Listener;
use iceoryx2::prelude::*;
use serde::de::DeserializeOwned;

use super::audio_window::{
    AudioWindowAccumulator, AudioWindowContractMatchingADeviceStream,
    DeviceMatchedAudioWindowContractsByInputPort,
};
use super::channel_name::InboundLinkName;
use super::function_node_island_input_port::InputPortFedByAFunctionNodeIsland;
use super::input_port_message_payload::{InputPortMessagePayload, SamplesOneInboundLinkHolds};
use super::loss_counters::{
    DiscardedSampleCountsByInboundLink, DroppedBagCountsByInboundLink,
    InboundLinkDiscardedSampleCounter, InboundLinkDroppedBagCounter,
};
use super::mailbox::PortMailbox;
use super::{
    ChannelDataServiceSubscriber, DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind,
    MessageGroupBoundary,
};
use crate::core::descriptors::PortStreamFacts;
use crate::core::error::{Error, Result};
use crate::core::graph::cast_exposed_name_to_url_safe;
use crate::core::output_port_stream_facts::UpstreamStreamFactsByInboundLink;
use streamlib_ipc_types::BagsAGapInTheNumberingSaysWereLost;

mod function_node_island_feeding;
mod inbound_link_accounting;
mod input_port_mailbox_reads;
mod input_port_setup;

/// One windowed port's stage, shared out of the `ports` map so the resample,
/// mixdown and framing work runs with that mutex released.
type SharedAudioWindowStage = Arc<parking_lot::Mutex<AudioWindowAccumulator>>;

/// One message ready to hand a reader, and the inbound link it came in on.
pub(super) struct BagBodyForTheReader {
    pub(super) message: MessageReadFromInputPort,
    /// The link this body came in on. `None` where nothing delivered it — a
    /// manually injected frame — and filled in for a window by the port that
    /// staged it, since a window is cut from bags rather than being one.
    pub(super) inbound_link_name: Option<InboundLinkName>,
}

/// One message read off an input port: its payload as the producer wrote it, its
/// stamp, whether it starts a group, and what its link lost ahead of it.
#[derive(Debug, PartialEq, Eq)]
pub struct MessageReadFromInputPort {
    /// The payload exactly as the producer wrote it, borrowed in its sample
    /// where it arrived in one.
    pub payload: InputPortMessagePayload,
    /// The publish stamp, or a window's first-sample instant.
    pub timestamp_ns: i64,
    /// Whether the message opens a group; a window always opens one.
    pub group_boundary: MessageGroupBoundary,
    /// The bags this message's inbound link lost between the previous message
    /// read from that link and this one; for a window, the bags lost ahead of
    /// those it was cut from. Each loss is told once, and the link's cumulative
    /// count `graph` renders is never zeroed.
    pub lost_since_last_read: u64,
    /// What the payload is: its bytes, or a pooled GPU frame the reader claims.
    pub payload_descriptor: DataChannelPayloadDescriptor,
}

impl From<streamlib_ipc_types::MessageReadFromAReservedSlot> for MessageReadFromInputPort {
    fn from(message: streamlib_ipc_types::MessageReadFromAReservedSlot) -> Self {
        Self {
            payload: message.payload.into(),
            timestamp_ns: message.timestamp_ns,
            group_boundary: message.group_boundary,
            lost_since_last_read: message.lost_since_last_read,
            payload_descriptor: message.payload_descriptor,
        }
    }
}

/// Decode one msgpack payload into the type a reader named.
fn deserialize_msgpack_payload<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
    rmp_serde::from_slice(body)
        .map_err(|failure| Error::Link(format!("Failed to deserialize frame: {failure}")))
}

/// The one spelling of "this port was never configured".
fn unknown_input_port(port: &str) -> Error {
    Error::Link(format!("Unknown input port: {port}"))
}

/// The one link feeding a windowed port, taken together with the port's stage.
struct WindowedPortInboundLink {
    link_id: String,
    inbound_link_name: InboundLinkName,
    dropped_bag_counter: InboundLinkDroppedBagCounter,
    discarded_sample_counter: Option<InboundLinkDiscardedSampleCounter>,
}

/// Whether the stage a read took is still the one its port reads through.
enum WindowedPortStageInstallation {
    /// The port was unwired, and perhaps wired again, since the read took it.
    Replaced,
    /// Still installed, fed by this link — or by none, for a manually injected
    /// frame.
    StillInstalled {
        feeding_link: Option<WindowedPortInboundLink>,
    },
}

/// One channel subscriber bound to the local input port it feeds.
///
/// A channel is keyed on its source output port, so a destination consuming N
/// inbound channels holds N subscribers.
/// Routing is by this binding — the receive path pushes every frame a subscriber
/// delivers into `local_port`'s mailbox — NOT by the frame's stamped port key
/// (a channel's single publisher stamps its own source port, which two
/// destinations subscribing the same channel would each map to a different local
/// port).
struct PortBoundSubscriber {
    /// The inbound `connect()` link this subscriber serves. Tags the subscriber
    /// so a per-link `disconnect` reclaims exactly it (see
    /// [`InputMailboxesInner::remove_channel_link`]) — a destination fanning in
    /// N links holds N subscribers on one local port, and only the disconnected
    /// one must go.
    link_id: String,
    local_port: String,
    /// The name a read hands back for every frame this subscriber delivers:
    /// the channel it subscribed to, which is what `graph` shows for the same
    /// link.
    inbound_link_name: InboundLinkName,
    subscriber: ChannelDataServiceSubscriber,
    /// This link's share of the destination's dropped-bag counts. Every frame
    /// this subscriber delivers is queued holding it, so an eviction names the
    /// link the evicted bag came in on rather than the one that made room.
    dropped_bag_counter: InboundLinkDroppedBagCounter,
    /// This link's share of the destination's discarded-sample counts, held only
    /// by a link into a windowed port.
    discarded_sample_counter: Option<InboundLinkDiscardedSampleCounter>,
    /// What this subscriber's ring has overwritten, read off jumps in the
    /// numbering of the publisher that is sending it — one unbroken run per
    /// publisher, so a replaced producer is a baseline rather than a gap.
    bags_the_ring_overwrote: BagsAGapInTheNumberingSaysWereLost<UniquePublisherId>,
    /// The samples this link holds, queued or with its reader, held to its
    /// port's mailbox depth plus borrow quota.
    samples_this_link_holds: SamplesOneInboundLinkHolds,
}

/// A destination's channel subscribers and its notify-service [`Listener`],
/// wired and unwired by one thread and read by any other.
#[derive(Default)]
struct InboundLinkSubscribersAndListener {
    subscribers: Vec<PortBoundSubscriber>,
    listener: Option<Listener<ipc_threadsafe::Service>>,
}

impl InboundLinkSubscribersAndListener {
    /// Remove the subscriber serving `link_id`, returning the local input port it
    /// was bound to (so the caller can decide whether that port's mailbox is now
    /// orphaned). `None` if no subscriber matches — a no-op.
    fn remove_by_link(&mut self, link_id: &str) -> Option<String> {
        let position = self
            .subscribers
            .iter()
            .position(|bound| bound.link_id == link_id)?;
        Some(self.subscribers.remove(position).local_port)
    }

    /// Whether any remaining subscriber is still bound to `local_port`.
    fn local_port_still_bound(&self, local_port: &str) -> bool {
        self.bound_to_local_port(local_port).next().is_some()
    }

    /// The one binding feeding `local_port`, or `None` where it has none or
    /// several.
    fn the_only_subscriber_bound_to_local_port<'a>(
        &'a self,
        local_port: &'a str,
    ) -> Option<&'a PortBoundSubscriber> {
        let mut feeding = self.bound_to_local_port(local_port);
        let only = feeding.next()?;
        feeding.next().is_none().then_some(only)
    }

    /// The bindings feeding one local input port, in wiring order — a
    /// destination fanning in N links holds N of them on that one port.
    fn bound_to_local_port<'a>(
        &'a self,
        local_port: &'a str,
    ) -> impl Iterator<Item = &'a PortBoundSubscriber> {
        self.subscribers
            .iter()
            .filter(move |bound| bound.local_port == local_port)
    }
}

/// What one port would hand a reader, read off its state under the `ports`
/// lock and judged with that lock released.
enum PortReadiness {
    /// A frame is queued.
    AFrameIsWaiting,
    /// Nothing is waiting.
    NothingIsWaiting,
    /// A windowed port: the stage answers, jointly over the remainder it holds
    /// and what the bags still in the mailbox are worth.
    AWindowedPort {
        stage: SharedAudioWindowStage,
        queued_output_frame_equivalents: u64,
        /// A windowed port that cannot form a window out of a mailbox with no
        /// room left is stalled; the stage says so once.
        the_mailbox_is_full: bool,
    },
}

impl PortReadiness {
    fn of(port_config: &PortConfig) -> Self {
        match &port_config.audio_windowing {
            InstalledInputPortAudioWindowing::NotWindowed => {
                if port_config.mailbox.is_empty() {
                    PortReadiness::NothingIsWaiting
                } else {
                    PortReadiness::AFrameIsWaiting
                }
            }
            // A port whose contract is not settled hands a reader nothing at
            // all — never the raw bags a settled one would have windowed. That
            // is the contract's whole promise applied to its own start-up:
            // `process()` receives exact-size blocks, and a device-shaped
            // window has no exact size until the device says what it is.
            InstalledInputPortAudioWindowing::AwaitingItsDeviceStreamFormat => {
                PortReadiness::NothingIsWaiting
            }
            InstalledInputPortAudioWindowing::Windowed(stage) => PortReadiness::AWindowedPort {
                stage: Arc::clone(stage),
                queued_output_frame_equivalents: port_config.mailbox.queued_frame_measure_total(),
                the_mailbox_is_full: port_config.mailbox.len() >= port_config.mailbox.capacity(),
            },
        }
    }

    fn a_read_would_return_something(&self) -> bool {
        match self {
            PortReadiness::AFrameIsWaiting => true,
            PortReadiness::NothingIsWaiting => false,
            PortReadiness::AWindowedPort {
                stage,
                queued_output_frame_equivalents,
                the_mailbox_is_full,
            } => stage.lock().a_full_window_would_be_ready_after(
                *queued_output_frame_equivalents,
                *the_mailbox_is_full,
            ),
        }
    }
}

/// Per-port configuration: the mailbox, which carries the port's read mode.
///
/// Interior mutability: the host-side wiring path discovers
/// per-port configuration (read_mode, buffer_size) at the moment
/// the first downstream `connect` op runs and may need to
/// add ports after the inner is already shared as `Arc`. We use
/// `parking_lot::Mutex<HashMap>` for `ports` rather than threading
/// `&mut self` through `Arc<...>`.
struct PortConfig {
    mailbox: PortMailbox,
    /// The messages a reader may hold past the mailbox, per inbound link.
    borrow_quota: usize,
    /// How this port windows what arrives on it.
    audio_windowing: InstalledInputPortAudioWindowing,
    /// The dropped-bag counter of the link that delivered the bag last handed
    /// to the reader, until the reader counts that bag lost. `None` before any
    /// read, after the count, and for a bag no link delivered or a window.
    dropped_bag_counter_of_the_bag_last_handed_to_the_reader: Option<InboundLinkDroppedBagCounter>,
}

/// How one port windows what arrives on it.
///
/// `Clone` bumps the stage's `Arc`, which is what lets a read carry the
/// windowing out of the `ports` lock and run a resampler pass with that mutex
/// released.
#[derive(Clone)]
enum InstalledInputPortAudioWindowing {
    /// The port declared no window contract. Unchanged in every respect: bags
    /// reach the reader exactly as they were published.
    NotWindowed,
    AwaitingItsDeviceStreamFormat,
    /// The port windows through this stage.
    Windowed(SharedAudioWindowStage),
}

impl InstalledInputPortAudioWindowing {
    /// Whether this port reads through `stage` itself, not one installed since.
    fn installs_stage(&self, stage: &SharedAudioWindowStage) -> bool {
        matches!(self, InstalledInputPortAudioWindowing::Windowed(installed) if Arc::ptr_eq(installed, stage))
    }

    /// Whether the port declared a window contract, settled or not.
    fn is_windowed(&self) -> bool {
        !matches!(self, InstalledInputPortAudioWindowing::NotWindowed)
    }

    /// Why this port cannot take a contract settled from a device stream, or
    /// `None` when it is waiting for exactly that.
    ///
    /// The state, not a bare bool: the difference between "you declared no
    /// contract" and "this is already settled" is the whole of what a caller
    /// needs to fix it.
    fn why_it_cannot_be_settled(&self) -> Option<&'static str> {
        match self {
            InstalledInputPortAudioWindowing::AwaitingItsDeviceStreamFormat => None,
            InstalledInputPortAudioWindowing::NotWindowed => Some("it declares no window contract"),
            InstalledInputPortAudioWindowing::Windowed(_) => {
                Some("its contract is already settled")
            }
        }
    }
}

pub struct InputMailboxesInner {
    inbound_link_subscribers_and_listener: parking_lot::Mutex<InboundLinkSubscribersAndListener>,
    ports: parking_lot::Mutex<HashMap<String, PortConfig>>,
    /// Counts every listener installed, so a runner can tell a listener
    /// created after its last link went away from the one it registered.
    listener_generation: std::sync::atomic::AtomicU64,
    dropped_bag_counts: Arc<DroppedBagCountsByInboundLink>,
    discarded_sample_counts: Arc<DiscardedSampleCountsByInboundLink>,
    device_matched_audio_window_contracts: Arc<DeviceMatchedAudioWindowContractsByInputPort>,
    upstream_stream_facts_by_inbound_link:
        std::sync::OnceLock<Arc<UpstreamStreamFactsByInboundLink>>,
    input_ports_fed_by_function_node_islands:
        parking_lot::RwLock<HashMap<String, Arc<InputPortFedByAFunctionNodeIsland>>>,
    input_ports_read_as_their_pin_value_while_unlinked:
        parking_lot::RwLock<HashMap<String, Arc<InputPortFedByAFunctionNodeIsland>>>,
}

impl InputMailboxesInner {
    /// Create a new empty inner.
    pub fn new() -> Self {
        Self {
            inbound_link_subscribers_and_listener: parking_lot::Mutex::default(),
            ports: parking_lot::Mutex::new(HashMap::new()),
            listener_generation: std::sync::atomic::AtomicU64::new(0),
            dropped_bag_counts: Arc::new(DroppedBagCountsByInboundLink::default()),
            discarded_sample_counts: Arc::new(DiscardedSampleCountsByInboundLink::default()),
            device_matched_audio_window_contracts: Arc::new(
                DeviceMatchedAudioWindowContractsByInputPort::default(),
            ),
            upstream_stream_facts_by_inbound_link: std::sync::OnceLock::new(),
            input_ports_fed_by_function_node_islands: parking_lot::RwLock::default(),
            input_ports_read_as_their_pin_value_while_unlinked: parking_lot::RwLock::default(),
        }
    }

    pub(crate) fn reach_upstream_stream_facts_by_inbound_link(
        &self,
        upstream_stream_facts_by_inbound_link: Arc<UpstreamStreamFactsByInboundLink>,
    ) {
        if self
            .upstream_stream_facts_by_inbound_link
            .set(upstream_stream_facts_by_inbound_link)
            .is_err()
        {
            tracing::warn!(
                "input mailboxes already reach their node's upstream stream facts; a second \
                 node's are ignored"
            );
        }
    }

    /// The current stream facts of the output port feeding `port` over
    /// `inbound_link`, or over its one link when `None`.
    pub fn upstream_stream_facts(
        &self,
        port: &str,
        inbound_link: Option<&InboundLinkName>,
    ) -> Result<Option<PortStreamFacts>> {
        self.upstream_stream_facts_by_inbound_link
            .get()
            .ok_or_else(|| {
                Error::Link(format!(
                    "input port '{port}' has no node whose inbound links it could read — the \
                     processor was not created for a node"
                ))
            })?
            .stream_facts_of_inbound_link(port, inbound_link.map(InboundLinkName::as_str))
    }

    /// Check if a port has already been configured.
    pub fn has_port(&self, port: &str) -> bool {
        self.ports.lock().contains_key(port)
            || self
                .input_ports_fed_by_function_node_islands
                .read()
                .contains_key(port)
            || self
                .input_ports_read_as_their_pin_value_while_unlinked
                .read()
                .contains_key(port)
    }

    /// Check if a listener has already been configured.
    pub fn has_listener(&self) -> bool {
        self.inbound_link_subscribers_and_listener
            .lock()
            .listener
            .is_some()
    }

    /// Set the iceoryx2 Listener for fd-multiplexed wakeups.
    pub fn set_listener(&self, listener: Listener<ipc_threadsafe::Service>) {
        let mut subscribers_and_listener = self.inbound_link_subscribers_and_listener.lock();
        subscribers_and_listener.listener = Some(listener);
        self.listener_generation
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    /// How many listeners have been installed so far. A runner that built its
    /// waiter for an earlier generation is watching an fd that no longer
    /// belongs to this destination.
    pub fn listener_generation(&self) -> u64 {
        self.listener_generation
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn listener_fd(&self) -> Option<i32> {
        // SAFETY: native_handle() is unsafe per iceoryx2-bb-posix because storing
        // the value across the Listener's lifetime would dangle. We return the
        // raw int and document that callers must drop usage before the Listener
        // is dropped, mirroring the FileDescriptor lifetime contract.
        self.inbound_link_subscribers_and_listener
            .lock()
            .listener
            .as_ref()
            .map(|l| unsafe { l.file_descriptor().native_handle() })
    }

    /// Drain any pending event activations from the listener so the fd transitions
    /// back to the not-readable state. No-op when no listener is configured.
    ///
    /// Call this after `epoll_wait` reports the fd readable, before the next
    /// `epoll_wait`, otherwise the wait returns immediately on the same event.
    pub fn drain_listener(&self) {
        if let Some(listener) = &self.inbound_link_subscribers_and_listener.lock().listener
            && let Err(e) = listener.try_wait(|_event_activation| {})
        {
            tracing::trace!("InputMailboxes: drain_listener try_wait failed: {:?}", e);
        }
    }

    /// Set how many messages `port`'s reader may hold past its mailbox, per
    /// inbound link. Refused for a port never added.
    pub fn set_input_port_borrow_quota(&self, port: &str, borrow_quota: usize) -> Result<()> {
        self.ports
            .lock()
            .get_mut(port)
            .ok_or_else(|| unknown_input_port(port))?
            .borrow_quota = borrow_quota;
        Ok(())
    }

    pub fn has_data(&self, port: &str) -> bool {
        if self.pin_value_read_by_an_unlinked(port).is_some() {
            return true;
        }
        self.receive_pending();
        if let Some(island_port) = self.function_node_island_feeding(port) {
            if island_port
                .the_reader_input_ports_its_links_from_outside_it_arrive_at()
                .is_empty()
            {
                return true;
            }
            return island_port
                .a_read_would_return_something(
                    self.a_message_waits_at_a_reader_input_port_of(&island_port),
                )
                .unwrap_or_else(|refusal| {
                    tracing::warn!(
                        input_port = port,
                        %refusal,
                        "the function node island feeding the input port refused to evaluate \
                         while its readiness was judged; a read will meet the refusal"
                    );
                    true
                });
        }
        let Some(readiness) = self.readiness_of_one_port(port) else {
            return false;
        };
        readiness.a_read_would_return_something()
    }

    /// True iff any configured input port has at least one queued
    /// payload. Drains pending iceoryx2 samples into the per-port
    /// mailboxes first, so this reflects total queue depth rather than
    /// just iceoryx2-buffered state.
    ///
    /// Used by the reactive scheduler to keep dispatching `process()`
    /// while events remain after a single epoll wake — iceoryx2's
    /// Event service coalesces multiple notifies on the same EventId
    /// into one fd-readable transition, so the runner must check
    /// queue depth itself rather than trusting one wake = one event.
    ///
    /// A reader input port an island reads a link from outside it at does not
    /// count on its own: the island fed port it feeds answers for it, so a
    /// message the island would hand nothing out for wakes no reader.
    pub fn any_port_has_data(&self) -> bool {
        self.receive_pending();
        let island_ports_reading_links_from_outside_them: Vec<(
            String,
            Arc<InputPortFedByAFunctionNodeIsland>,
        )> = self
            .input_ports_fed_by_function_node_islands
            .read()
            .iter()
            .filter(|(_, island_port)| {
                !island_port
                    .the_reader_input_ports_its_links_from_outside_it_arrive_at()
                    .is_empty()
            })
            .map(|(port, island_port)| (port.clone(), Arc::clone(island_port)))
            .collect();
        let reader_input_ports_islands_read: HashSet<&str> =
            island_ports_reading_links_from_outside_them
                .iter()
                .flat_map(|(_, island_port)| {
                    island_port
                        .the_reader_input_ports_its_links_from_outside_it_arrive_at()
                        .iter()
                        .map(String::as_str)
                })
                .collect();
        if self.any_ports_read_would_return_something(&reader_input_ports_islands_read) {
            return true;
        }
        island_ports_reading_links_from_outside_them
            .iter()
            .any(|(port, island_port)| {
                island_port
                    .wakes_its_reader(self.a_message_waits_at_a_reader_input_port_of(island_port))
                    .unwrap_or_else(|refusal| {
                        tracing::warn!(
                            input_port = port.as_str(),
                            %refusal,
                            "the function node island feeding the input port refused to \
                             evaluate while its readiness was judged; a read will meet the \
                             refusal"
                        );
                        true
                    })
            })
    }

    /// Whether a message the island feeding `island_port` has not read yet
    /// waits at one of the reader input ports its links from outside it
    /// arrive at.
    fn a_message_waits_at_a_reader_input_port_of(
        &self,
        island_port: &InputPortFedByAFunctionNodeIsland,
    ) -> bool {
        let reader_input_ports =
            island_port.the_reader_input_ports_its_links_from_outside_it_arrive_at();
        if reader_input_ports.is_empty() {
            return false;
        }
        let ports = self.ports.lock();
        reader_input_ports.iter().any(|reader_input_port| {
            ports
                .get(reader_input_port)
                .is_some_and(|port_config| !port_config.mailbox.is_empty())
        })
    }

    /// What one port would hand a reader, gathered under the `ports` lock so
    /// the judgement itself is made with that lock released.
    fn readiness_of_one_port(&self, port: &str) -> Option<PortReadiness> {
        self.ports.lock().get(port).map(PortReadiness::of)
    }

    fn any_ports_read_would_return_something(
        &self,
        reader_input_ports_islands_read: &HashSet<&str>,
    ) -> bool {
        let windowed: Vec<PortReadiness> = {
            let ports = self.ports.lock();
            let mut windowed = Vec::new();
            for readiness in ports
                .iter()
                .filter(|(port, _)| !reader_input_ports_islands_read.contains(port.as_str()))
                .map(|(_, port_config)| PortReadiness::of(port_config))
            {
                match readiness {
                    PortReadiness::AFrameIsWaiting => return true,
                    PortReadiness::NothingIsWaiting => {}
                    windowed_port => windowed.push(windowed_port),
                }
            }
            windowed
        };
        windowed
            .iter()
            .any(|readiness| readiness.a_read_would_return_something())
    }

    /// Drain every payload queued on the given port's mailbox, as copies.
    pub fn drain(&self, port: &str) -> Vec<Vec<u8>> {
        let ports = self.ports.lock();
        ports
            .get(port)
            .into_iter()
            .flat_map(|p| p.mailbox.drain())
            .collect()
    }

    /// Get the list of configured port names.
    pub fn port_names(&self) -> Vec<String> {
        let mut port_names: Vec<String> = self.ports.lock().keys().cloned().collect();
        for island_fed_port_name in self.input_ports_fed_by_function_node_islands.read().keys() {
            if !port_names.contains(island_fed_port_name) {
                port_names.push(island_fed_port_name.clone());
            }
        }
        for pinned_port_name in self
            .input_ports_read_as_their_pin_value_while_unlinked
            .read()
            .keys()
        {
            if !port_names.contains(pinned_port_name) {
                port_names.push(pinned_port_name.clone());
            }
        }
        port_names
    }
}

impl Default for InputMailboxesInner {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// InputMailboxes
// =============================================================================

pub struct InputMailboxes {
    pub(crate) handle: *const c_void,
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<InputMailboxesInner>();
};

// SAFETY: `handle` points at an `Arc<InputMailboxesInner>`, and the assertion
// above holds the compiler to that inner being `Send + Sync`.
unsafe impl Send for InputMailboxes {}
unsafe impl Sync for InputMailboxes {}

impl InputMailboxes {
    /// Build a handle from an `Arc<InputMailboxesInner>`. The strong
    /// reference is consumed; the handle owns it for its lifetime and
    /// releases on Drop.
    pub fn from_inner_arc(inner: Arc<InputMailboxesInner>) -> Self {
        let handle = Arc::into_raw(inner) as *const c_void;
        Self { handle }
    }

    /// Engine-internal borrow of the `InputMailboxesInner`, or `None`
    /// when unwired.
    fn host_inner(&self) -> Option<&InputMailboxesInner> {
        if self.handle.is_null() {
            return None;
        }
        // SAFETY: `handle` is `Arc::into_raw(Arc<InputMailboxesInner>)`.
        Some(unsafe { &*(self.handle as *const InputMailboxesInner) })
    }

    /// Build an empty pre-wiring handle (null). The host patches in
    /// the real `Arc` via `set_iceoryx2_resources`.
    pub fn empty() -> Self {
        Self {
            handle: std::ptr::null(),
        }
    }

    /// Returns true iff this has been wired to a real inner.
    pub fn is_configured(&self) -> bool {
        !self.handle.is_null()
    }

    /// Borrow the `Arc<InputMailboxesInner>` this handle points at.
    /// Returns `None` for unwired handles. Bumps the strong count; the
    /// returned Arc balances with one Drop on the inner.
    pub fn inner_arc(&self) -> Option<Arc<InputMailboxesInner>> {
        if !self.is_configured() {
            return None;
        }
        // SAFETY: `handle` is `Arc::into_raw(Arc<InputMailboxesInner>)`; bump
        // the strong count and reconstruct an owning `Arc` from the raw handle.
        unsafe {
            Arc::increment_strong_count(self.handle as *const InputMailboxesInner);
            Some(Arc::from_raw(self.handle as *const InputMailboxesInner))
        }
    }

    /// The next message on `port` deserialized from msgpack into `T`, following
    /// the port's read mode; an empty mailbox is an error.
    pub fn read_msgpack<T: DeserializeOwned>(&self, port: &str) -> Result<T> {
        let message = self
            .read_raw(port)?
            .ok_or_else(|| Error::Link(format!("No data available on port: {}", port)))?;
        deserialize_msgpack_payload(&message.payload)
    }

    /// The next message on `port`, its payload untouched, or `None` when the
    /// mailbox is empty.
    pub fn read_raw(&self, port: &str) -> Result<Option<MessageReadFromInputPort>> {
        let Some(inner) = self.host_inner() else {
            return Ok(None);
        };
        inner.read_raw(&cast_exposed_name_to_url_safe(port)?)
    }

    /// The next message on `port` with the inbound link it arrived on.
    ///
    /// What a destination taking many links on one port reads with: each
    /// inbound link is one producer, named by the source channel name it
    /// subscribed to, so a sink can tell N streams apart without the producers
    /// having to identify themselves in their messages.
    ///
    /// Messages from one link keep that link's order; no interleaving is
    /// promised between two links. A message no link delivered is refused by
    /// name, and the refusal consumes it.
    pub fn read_raw_from_inbound_link(
        &self,
        port: &str,
    ) -> Result<Option<(MessageReadFromInputPort, InboundLinkName)>> {
        let Some(inner) = self.host_inner() else {
            return Ok(None);
        };
        inner.read_raw_from_inbound_link(&cast_exposed_name_to_url_safe(port)?)
    }

    /// The next msgpack message on `port` deserialized into `T`, with the inbound
    /// link it arrived on, or `None` when the mailbox is empty.
    pub fn read_msgpack_from_inbound_link<T: DeserializeOwned>(
        &self,
        port: &str,
    ) -> Result<Option<(T, InboundLinkName)>> {
        let Some(inner) = self.host_inner() else {
            return Ok(None);
        };
        inner.read_msgpack_from_inbound_link(&cast_exposed_name_to_url_safe(port)?)
    }

    /// Count the message last read from `port` as lost on the inbound link
    /// that delivered it, once: what a reader does when what the message names
    /// is gone, such as a frame its producer recycled before the reader's
    /// claim.
    pub fn count_the_message_last_read_as_lost(&self, port: &str) -> Result<()> {
        let Some(inner) = self.host_inner() else {
            return Err(Error::Link(format!(
                "InputMailboxes not wired (port='{port}'): nothing has been read, so there is \
                 no message to count as lost"
            )));
        };
        inner.count_the_message_last_read_as_lost(&cast_exposed_name_to_url_safe(port)?)
    }

    /// Every inbound link feeding `port`, in wiring order.
    ///
    /// Readable in `setup()` — WIRE runs before it — which is how a sink learns
    /// how many producers it owes before the first bag arrives. A port with no
    /// links lists none.
    pub fn inbound_link_names(&self, port: &str) -> Vec<InboundLinkName> {
        match (self.host_inner(), cast_exposed_name_to_url_safe(port)) {
            (Some(inner), Ok(port)) => inner.inbound_link_names(&port),
            _ => Vec::new(),
        }
    }

    /// The current stream facts of the output port feeding `port`, or `None`
    /// when nothing feeds it or that port states none.
    ///
    /// A port fed by more than one link names the one to read with
    /// `inbound_link`; left `None`, such a port is refused naming its links.
    pub fn upstream_stream_facts(
        &self,
        port: &str,
        inbound_link: Option<&InboundLinkName>,
    ) -> Result<Option<PortStreamFacts>> {
        match self.host_inner() {
            Some(inner) => inner.upstream_stream_facts(port, inbound_link),
            None => Err(Error::Link(format!(
                "InputMailboxes not wired (port='{port}'): host has not yet installed iceoryx2 \
                 resources on this processor instance"
            ))),
        }
    }

    /// Whether `port` has been configured — a port has a mailbox only once a
    /// link is wired into it.
    pub fn has_port(&self, port: &str) -> bool {
        match (self.host_inner(), cast_exposed_name_to_url_safe(port)) {
            (Some(inner), Ok(port)) => inner.has_port(&port),
            _ => false,
        }
    }

    /// Check if a port has any payloads available.
    pub fn has_data(&self, port: &str) -> bool {
        match (self.host_inner(), cast_exposed_name_to_url_safe(port)) {
            (Some(inner), Ok(port)) => inner.has_data(&port),
            _ => false,
        }
    }

    pub fn settle_a_ports_device_matched_audio_window_contract(
        &self,
        _full_access_gate: &crate::core::context::RuntimeContextFullAccess<'_>,
        port: &str,
        matching: &AudioWindowContractMatchingADeviceStream,
    ) -> Result<()> {
        let Some(inner) = self.host_inner() else {
            return Err(Error::Configuration(format!(
                "input port '{port}' declares `audio_window = match_device` but this \
                 processor's inputs are not wired, so there is nothing to settle the \
                 contract on. Connect the port, or drop the contract from it"
            )));
        };
        inner.settle_a_ports_device_matched_audio_window_contract(
            &cast_exposed_name_to_url_safe(port)?,
            matching,
        )
    }
}

impl Default for InputMailboxes {
    fn default() -> Self {
        Self::empty()
    }
}

impl Clone for InputMailboxes {
    fn clone(&self) -> Self {
        if !self.is_configured() {
            return Self::empty();
        }
        // SAFETY: `handle` is `Arc::into_raw(Arc<InputMailboxesInner>)`; bump
        // the strong count so both handles own one reference.
        unsafe {
            Arc::increment_strong_count(self.handle as *const InputMailboxesInner);
        }
        Self {
            handle: self.handle,
        }
    }
}

impl Drop for InputMailboxes {
    fn drop(&mut self) {
        if !self.is_configured() {
            return;
        }
        // SAFETY: `handle` is `Arc::into_raw(Arc<InputMailboxesInner>)`.
        unsafe {
            drop(Arc::from_raw(self.handle as *const InputMailboxesInner));
        }
        self.handle = std::ptr::null();
    }
}

#[cfg(test)]
mod tests;
