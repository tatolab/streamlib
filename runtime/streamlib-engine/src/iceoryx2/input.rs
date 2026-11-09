// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Input mailboxes for receiving frames from upstream processors.
//!
//! # Two-type split: handle vs. inner
//!
//! - [`InputMailboxesInner`] holds the actual state — the
//!   `HashMap<port, PortConfig>` of per-port mailboxes plus the
//!   channel subscribers and listener behind their own lock. All
//!   per-frame `receive_pending` + mailbox push/pop work runs here.
//! - [`InputMailboxes`] is the public handle that processor structs
//!   hold via the macro-emitted `inputs: InputMailboxes` field. It
//!   wraps an `Arc<InputMailboxesInner>` behind an opaque handle;
//!   `process()` reaches input data through `read_raw` / `read_msgpack` /
//!   `has_data`, which borrow the inner and invoke it directly.
//!
//! Host-side wiring code that mutates the inner (`add_port`,
//! `add_channel_subscriber`, `set_listener`, `listener_fd`,
//! `drain_listener`, etc.) operates on `Arc<InputMailboxesInner>`
//! directly via the methods declared on the inner type.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;

use iceoryx2::identifiers::UniquePublisherId;
use iceoryx2::port::listener::Listener;
use iceoryx2::prelude::*;
use serde::de::DeserializeOwned;

use super::audio_window::{
    AudioWindowAccumulator, AudioWindowContractMatchingADeviceStream, AudioWindowStageFlush,
    DeviceMatchedAudioWindowContractsByInputPort, LatestQueuedSourceAudioFormat,
    ResolvedAudioWindowContract, queued_audio_window_frame_measure,
};
use super::channel_name::InboundLinkName;
use super::function_node_island_input_port::{
    FunctionNodeIslandEvaluatedAtEachRead, InputPortFedByAFunctionNodeIsland,
    PinValueReadByAnUnlinkedInputPort,
};
use super::helper_process_loss_count_board::InboundLinkLossCountBoardSlotMirror;
use super::input_port_borrow_quota::DEFAULT_INPUT_PORT_BORROW_QUOTA;
use super::input_port_message_payload::{
    BorrowedChannelSample, InputPortMessagePayload, SamplesOneInboundLinkHolds,
};
use super::loss_counters::{
    DiscardedSampleCountsByInboundLink, DroppedBagCountsByInboundLink,
    InboundLinkDiscardedSampleCounter, InboundLinkDroppedBagCounter,
};
use super::mailbox::{PortMailbox, PortMailboxEvictionNotice};
use super::read_mode::ReadMode;
use super::{
    ChannelDataServiceSubscriber, DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind,
    MessageGroupBoundary,
};
use crate::core::descriptors::PortStreamFacts;
use crate::core::error::{Error, Result};
use crate::core::graph::cast_exposed_name_to_url_safe;
use crate::core::output_port_stream_facts::UpstreamStreamFactsByInboundLink;
use streamlib_ipc_types::BagsAGapInTheNumberingSaysWereLost;

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

/// Where a bag taken off a port's mailbox goes.
#[derive(Clone, Copy)]
enum WhereABagTakenOffTheMailboxGoes<'stage> {
    /// To the reader, told of the losses ahead of it.
    ToTheReader,
    /// Into the windowed port's stage, while it is still the stage installed;
    /// the window cut from the bag is told of the losses instead.
    IntoTheStageWhileItIsInstalled(&'stage SharedAudioWindowStage),
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
    /// The port declared `audio_window = match_device` and nothing has settled
    /// it yet.
    ///
    /// The port exists and its mailbox counts, because the wiring path runs
    /// before any processor reaches `setup()` and a bag arriving in between
    /// must land somewhere countable. What it will not do is hand a reader
    /// anything: it has no contract to window by, and the raw bags underneath
    /// are exactly what the contract exists to stop reaching `process()`.
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

/// One windowed port's mailbox, stage and read mode, built from the contract
/// that drives them.
///
/// Written once because two callers must produce exactly the same port: the
/// wiring path, for a contract already settled when the link is wired, and the
/// settle, for one that arrives at `setup()`.
fn windowed_port_config(
    port: &str,
    read_mode: ReadMode,
    contract: ResolvedAudioWindowContract,
) -> PortConfig {
    // Shared by the mailbox's measure, which writes every arriving bag's
    // format into it, and the stage, which reads it so its readiness floor is
    // exact before it has consumed anything.
    let latest_queued_source_audio_format = Arc::new(LatestQueuedSourceAudioFormat::default());
    PortConfig {
        mailbox: PortMailbox::new(contract.windowed_port_mailbox_depth(), read_mode)
            .measuring_every_queued_frame_with(queued_audio_window_frame_measure(
                contract,
                Arc::clone(&latest_queued_source_audio_format),
            )),
        audio_windowing: InstalledInputPortAudioWindowing::Windowed(Arc::new(
            parking_lot::Mutex::new(AudioWindowAccumulator::new(
                port,
                contract,
                latest_queued_source_audio_format,
            )),
        )),
        dropped_bag_counter_of_the_bag_last_handed_to_the_reader: None,
        borrow_quota: DEFAULT_INPUT_PORT_BORROW_QUOTA,
    }
}

/// The one-shot notice a `match_device` port installs on the mailbox it holds
/// while its contract is unsettled.
///
/// Every other port that evicts is a consumer falling behind, and the per-link
/// count says so on its own. This one is not: an unsettled port has no contract
/// to window by, so it hands its reader nothing however much arrives, and the
/// climbing count reads as a consumer that is merely slow.
///
/// The state is reached only by a port being drained while unsettled, and the
/// dominant way that happens is a link wired onto the port after its processor's
/// `setup()` has been and gone without settling it — the branch the spawn-time
/// refusal cannot cover, because a port with no link at `setup()` time is not in
/// the map to be refused. Such a port stays unsettled for the rest of the run.
/// Said once per port rather than once per bag: nothing about the state changes
/// between one evicted bag and the next.
fn notice_that_a_bag_was_lost_at_a_port_whose_match_device_contract_is_unsettled(
    port: &str,
    depth: usize,
) -> PortMailboxEvictionNotice {
    let port_name = port.to_string();
    let already_said = std::sync::atomic::AtomicBool::new(false);
    Arc::new(move || {
        if already_said.swap(true, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        tracing::warn!(
            port = %port_name,
            mailbox_depth = depth,
            "input port evicted a bag while its `audio_window = match_device` contract is \
             unsettled: with no contract to window by it hands its reader nothing, so this \
             loss is not a consumer falling behind. If this port's processor already ran \
             `setup()` without settling it — a link wired onto the port after the fact — \
             it stays this way for the rest of the run"
        );
    })
}

/// Host-side inner state for input mailboxes. Owns the per-port
/// mailbox map plus the channel subscribers and listener. All
/// per-frame `receive_pending` + queue-pop work runs here.
///
/// Held via `Arc<InputMailboxesInner>`; the [`InputMailboxes`] handle
/// stores a separate `Arc::into_raw`-encoded strong reference to the
/// same inner.
///
/// Wiring against wiring is serialized by its caller — the compiler under
/// the processor mutex, a helper one wiring command at a time — because a
/// check-then-install pair such as `has_listener` then `set_listener` is not
/// atomic. Reads need no such caller.
pub struct InputMailboxesInner {
    /// Taken by every access to a subscriber or the listener, from whichever
    /// thread: wiring and unwiring, a processor reading on its own thread, a
    /// helper reading with no GIL attached.
    ///
    /// Lock order: this, then `ports` or the dropped-bag counts, then a
    /// windowed port's stage. Never take an earlier one while holding a later.
    inbound_link_subscribers_and_listener: parking_lot::Mutex<InboundLinkSubscribersAndListener>,
    ports: parking_lot::Mutex<HashMap<String, PortConfig>>,
    /// Counts every listener installed, so a runner can tell a listener
    /// created after its last link went away from the one it registered.
    listener_generation: std::sync::atomic::AtomicU64,
    dropped_bag_counts: Arc<DroppedBagCountsByInboundLink>,
    discarded_sample_counts: Arc<DiscardedSampleCountsByInboundLink>,
    device_matched_audio_window_contracts: Arc<DeviceMatchedAudioWindowContractsByInputPort>,
    /// The upstream stream facts the node's inbound links reach, reached
    /// once the processor is created for its node.
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

    /// Feed `port` from `island`, evaluated at each read of the port in place
    /// of a mailbox; an island already feeding `port` is replaced, and the
    /// port's sample positions carry on from it.
    pub fn feed_an_input_port_from_a_function_node_island(
        &self,
        port: &str,
        island: Arc<dyn FunctionNodeIslandEvaluatedAtEachRead>,
    ) {
        let mut input_ports_fed_by_function_node_islands =
            self.input_ports_fed_by_function_node_islands.write();
        let island_port = match input_ports_fed_by_function_node_islands.get(port) {
            Some(island_port_it_replaces) => {
                InputPortFedByAFunctionNodeIsland::evaluated_by_in_place_of(
                    island,
                    island_port_it_replaces,
                )
            }
            None => InputPortFedByAFunctionNodeIsland::evaluated_by(island),
        };
        input_ports_fed_by_function_node_islands.insert(port.to_string(), Arc::new(island_port));
    }

    /// Stop feeding `port` from the island feeding it; whether one did.
    pub fn stop_feeding_an_input_port_from_a_function_node_island(&self, port: &str) -> bool {
        self.input_ports_fed_by_function_node_islands
            .write()
            .remove(port)
            .is_some()
    }

    /// The function node the island feeding `port` runs as Python, which a
    /// read without the Python interpreter's lock cannot call; `None` when no
    /// island feeds `port` or its every node runs as native code.
    pub fn the_function_node_run_as_python_by_the_island_feeding(
        &self,
        port: &str,
    ) -> Option<String> {
        self.function_node_island_feeding(port)
            .and_then(|island_port| {
                island_port
                    .the_first_function_node_it_runs_as_python()
                    .map(str::to_string)
            })
    }

    /// The state each stateful node of the island feeding `port` last
    /// returned, by its breadcrumb number; empty when no island feeds `port`.
    pub fn the_state_each_stateful_function_node_of_the_island_feeding_last_returned(
        &self,
        port: &str,
    ) -> Vec<(u32, Vec<f64>)> {
        self.function_node_island_feeding(port)
            .map(|island_port| island_port.the_state_each_stateful_function_node_last_returned())
            .unwrap_or_default()
    }

    fn function_node_island_feeding(
        &self,
        port: &str,
    ) -> Option<Arc<InputPortFedByAFunctionNodeIsland>> {
        self.input_ports_fed_by_function_node_islands
            .read()
            .get(port)
            .cloned()
    }

    /// Read each port of `pin_values` as its pin value while no link feeds
    /// it, at the time `stream_clock_now_ns` gives, in place of the pin values
    /// read before; a port left out reads only its links again. A port kept
    /// counts its reads on from where it was.
    pub fn read_each_unlinked_input_port_as_its_pin_value(
        &self,
        pin_values: HashMap<String, f64>,
        stream_clock_now_ns: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) {
        let mut input_ports_read_as_their_pin_value = self
            .input_ports_read_as_their_pin_value_while_unlinked
            .write();
        let pinned_ports = pin_values
            .into_iter()
            .map(|(port, pin_value)| {
                let pin_value: Arc<dyn FunctionNodeIslandEvaluatedAtEachRead> =
                    Arc::new(PinValueReadByAnUnlinkedInputPort::read_at_the_time_of(
                        pin_value,
                        Arc::clone(&stream_clock_now_ns),
                    ));
                let pinned_port = match input_ports_read_as_their_pin_value.get(&port) {
                    Some(pinned_port_it_replaces) => {
                        InputPortFedByAFunctionNodeIsland::evaluated_by_in_place_of(
                            pin_value,
                            pinned_port_it_replaces,
                        )
                    }
                    None => InputPortFedByAFunctionNodeIsland::evaluated_by(pin_value),
                };
                (port, Arc::new(pinned_port))
            })
            .collect();
        *input_ports_read_as_their_pin_value = pinned_ports;
    }

    /// The pin value `port` reads, when one is set on it and no link feeds it.
    fn pin_value_read_by_an_unlinked(
        &self,
        port: &str,
    ) -> Option<Arc<InputPortFedByAFunctionNodeIsland>> {
        let pinned_port = self
            .input_ports_read_as_their_pin_value_while_unlinked
            .read()
            .get(port)
            .cloned()?;
        let no_link_feeds_the_port = self
            .inbound_link_subscribers_and_listener
            .lock()
            .bound_to_local_port(port)
            .next()
            .is_none();
        no_link_feeds_the_port.then_some(pinned_port)
    }

    /// Reach the upstream stream facts of the node this processor was
    /// created for.
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

    /// Add a mailbox for the given port with the specified buffer
    /// size and read mode.
    pub fn add_port(&self, port: &str, buffer_size: usize, read_mode: ReadMode) {
        tracing::debug!(
            port = port,
            buffer_size = buffer_size,
            read_mode = ?read_mode,
            "InputMailboxes: add_port"
        );
        self.ports.lock().insert(
            port.to_string(),
            PortConfig {
                mailbox: PortMailbox::new(buffer_size, read_mode),
                audio_windowing: InstalledInputPortAudioWindowing::NotWindowed,
                dropped_bag_counter_of_the_bag_last_handed_to_the_reader: None,
                borrow_quota: DEFAULT_INPUT_PORT_BORROW_QUOTA,
            },
        );
    }

    /// Add a mailbox for a port whose window contract is `match_device` and is
    /// not settled yet.
    ///
    /// This is what a `match_device` port looks like while nothing has settled
    /// it: countable, and readable by nobody. Two things reach the state — the
    /// compiler wiring every link before it releases any processor into
    /// `setup()`, and a link wired onto the port after that `setup()` has been
    /// and gone without settling it. Only the second is ever drained while it
    /// lasts, and it lasts for the rest of the run. A bag arriving here is
    /// queued and, on overrun, evicted and counted against its own inbound
    /// link, exactly as at any other port — and, unlike at any other port, said
    /// out loud once, because an unsettled port hands its reader nothing and
    /// the count alone reads as a consumer that is merely slow.
    ///
    /// `depth` comes from the caller the way [`Self::add_port`]'s does, rather
    /// than being assumed from the profile a window contract forces: there is
    /// no contract to size it from yet, and the settle re-sizes it from one and
    /// carries anything queued across.
    pub fn add_port_awaiting_its_device_stream_format(
        &self,
        port: &str,
        depth: usize,
        read_mode: ReadMode,
    ) {
        tracing::debug!(
            port = port,
            buffer_size = depth,
            read_mode = ?read_mode,
            "InputMailboxes: add_port_awaiting_its_device_stream_format"
        );
        self.ports.lock().insert(
            port.to_string(),
            PortConfig {
                mailbox: PortMailbox::new(depth, read_mode).reporting_every_eviction_to(
                    notice_that_a_bag_was_lost_at_a_port_whose_match_device_contract_is_unsettled(
                        port, depth,
                    ),
                ),
                audio_windowing: InstalledInputPortAudioWindowing::AwaitingItsDeviceStreamFormat,
                dropped_bag_counter_of_the_bag_last_handed_to_the_reader: None,
                borrow_quota: DEFAULT_INPUT_PORT_BORROW_QUOTA,
            },
        );
    }

    /// Add a mailbox for a port that declared a window contract, with the
    /// stage that honours it.
    ///
    /// The depth is the contract's rather than the delivery profile's: a
    /// window is a span of queued blocks, and `ORDERED_DEPTH` cannot hold a
    /// one-second rolling window's worth. Still engine-chosen, still not
    /// authorable — the contract is a declaration, not a depth dial.
    pub fn add_windowed_port(
        &self,
        port: &str,
        read_mode: ReadMode,
        contract: ResolvedAudioWindowContract,
    ) {
        tracing::debug!(
            port = port,
            buffer_size = contract.windowed_port_mailbox_depth(),
            read_mode = ?read_mode,
            sample_rate = contract.sample_rate,
            channels = %contract.rendered_channel_count(),
            window_size = contract.window_size,
            hop = contract.hop,
            "InputMailboxes: add_windowed_port"
        );
        self.ports.lock().insert(
            port.to_string(),
            windowed_port_config(port, read_mode, contract),
        );
    }

    /// Settle a `match_device` port's contract from the format of the device
    /// stream its own processor just opened, installing the stage that honours
    /// it.
    ///
    /// Called from `setup()`, where the capability typestate is Full — the same
    /// phase in which a processor requests a window.
    ///
    /// **Only a port that declared the sentinel and is still waiting on it can
    /// be settled.** A wired port carries what its author declared, so the port
    /// itself is the check: one that declared its values, or none at all, or
    /// one already settled, is refused naming what it is. Without that a
    /// processor could window a port whose author asked for nothing, or replace
    /// a declared contract with its own — and `graph` would go on rendering the
    /// author's declaration while the stage ran something else.
    ///
    /// The settled values outlive the port itself, so a link wired later —
    /// after `setup()` already ran — finds them rather than a sentinel, and
    /// `graph` renders what the device gave rather than what the declaration
    /// asked for. A port not yet wired has no declaration here to check
    /// against; the wiring path is what reads the settled values, and it reads
    /// them only for a port whose declaration is the sentinel.
    pub(crate) fn settle_a_ports_device_matched_audio_window_contract(
        &self,
        port: &str,
        matching: &AudioWindowContractMatchingADeviceStream,
    ) -> Result<()> {
        let contract = ResolvedAudioWindowContract::from_a_device_stream_format(matching).map_err(
            |refusal| {
                Error::Configuration(format!(
                    "input port '{port}' resolving `audio_window = match_device` from its \
                     device stream produced {refusal}"
                ))
            },
        )?;

        let mut ports = self.ports.lock();
        if let Some(existing) = ports.get_mut(port) {
            if let Some(refusal) = existing.audio_windowing.why_it_cannot_be_settled() {
                return Err(Error::Configuration(format!(
                    "input port '{port}' cannot be settled from a device stream: {refusal}. \
                     Only a port declaring `audio_window = match_device` resolves from the \
                     device its processor opened"
                )));
            }
            let mut settled = windowed_port_config(port, existing.mailbox.read_mode(), contract);
            settled.borrow_quota = existing.borrow_quota;
            // Anything that arrived while the contract was unsettled moves into
            // the mailbox the contract sized, rather than being dropped where
            // no counter would see it.
            existing
                .mailbox
                .hand_every_queued_frame_over_to(&settled.mailbox);
            *existing = settled;
        }
        self.device_matched_audio_window_contracts
            .settle_for_input_port(port, contract);
        drop(ports);

        tracing::info!(
            port = port,
            sample_rate = contract.sample_rate,
            channels = %contract.rendered_channel_count(),
            window_size = contract.window_size,
            hop = contract.hop,
            "InputMailboxes: `audio_window = match_device` settled from the device stream"
        );
        Ok(())
    }

    /// This processor's settled `match_device` contracts, shared with the graph
    /// node so `graph` renders the resolved values off the port that resolved
    /// them.
    pub(crate) fn device_matched_audio_window_contracts(
        &self,
    ) -> Arc<DeviceMatchedAudioWindowContractsByInputPort> {
        Arc::clone(&self.device_matched_audio_window_contracts)
    }

    /// The input ports still holding an unsettled `match_device` sentinel,
    /// named.
    ///
    /// Asked once, after `setup()` returns: by then the only processor that
    /// could have settled one has had its chance, so a port still here belongs
    /// to a processor that opens no device stream and is a wiring error.
    pub(crate) fn input_ports_still_awaiting_their_device_stream_format(&self) -> Vec<String> {
        self.ports
            .lock()
            .iter()
            .filter(|(_, config)| {
                matches!(
                    config.audio_windowing,
                    InstalledInputPortAudioWindowing::AwaitingItsDeviceStreamFormat
                )
            })
            .map(|(port, _)| port.clone())
            .collect()
    }

    /// Whether any channel subscriber has been configured yet.
    pub fn has_subscribers(&self) -> bool {
        !self
            .inbound_link_subscribers_and_listener
            .lock()
            .subscribers
            .is_empty()
    }

    /// Bind an iceoryx2 channel Subscriber to the local input port it feeds.
    ///
    /// One call per inbound `connect()` link — a destination consuming N
    /// channels holds N subscribers. The receive path routes every frame a
    /// subscriber delivers into `local_port`'s mailbox (binding-based routing;
    /// see [`PortBoundSubscriber`]).
    ///
    /// Bind after adding the port: a link into a windowed port is given its
    /// discarded-sample count here, from the port it finds.
    pub fn add_channel_subscriber(
        &self,
        local_port: &str,
        link_id: &str,
        inbound_link_name: &InboundLinkName,
        subscriber: ChannelDataServiceSubscriber,
    ) {
        let dropped_bag_counter = self.dropped_bag_counts.counter_for_inbound_link(link_id);
        let into_a_windowed_port = self
            .ports
            .lock()
            .get(local_port)
            .is_some_and(|port| port.audio_windowing.is_windowed());
        let discarded_sample_counter = into_a_windowed_port.then(|| {
            self.discarded_sample_counts
                .counter_for_inbound_link(link_id)
        });
        self.inbound_link_subscribers_and_listener
            .lock()
            .subscribers
            .push(PortBoundSubscriber {
                link_id: link_id.to_string(),
                local_port: local_port.to_string(),
                inbound_link_name: inbound_link_name.clone(),
                subscriber,
                dropped_bag_counter,
                discarded_sample_counter,
                bags_the_ring_overwrote: Default::default(),
                samples_this_link_holds: SamplesOneInboundLinkHolds::default(),
            });
    }

    /// Write every new total of `link_id`'s dropped-bag, taken-bag and
    /// discarded-sample counts onto `slot`, as the counts move.
    ///
    /// How a helper process's counts reach its parent. Refused for a link no
    /// subscriber is bound for, and for one already mirrored.
    pub fn mirror_an_inbound_links_loss_counts_into(
        &self,
        link_id: &str,
        inbound_link_board_slot_mirror: InboundLinkLossCountBoardSlotMirror,
    ) -> Result<()> {
        let (dropped_bag_counter, discarded_sample_counter) = self
            .inbound_link_subscribers_and_listener
            .lock()
            .subscribers
            .iter()
            .find(|bound| bound.link_id == link_id)
            .map(|bound| {
                (
                    bound.dropped_bag_counter.clone(),
                    bound.discarded_sample_counter.clone(),
                )
            })
            .ok_or_else(|| {
                Error::Link(format!(
                    "inbound link '{link_id}' has no subscriber bound, so there are no loss \
                     counts of it to mirror"
                ))
            })?;
        let already_mirrored = || {
            Error::Link(format!(
                "inbound link '{link_id}' already mirrors its loss counts onto a board slot"
            ))
        };
        let dropped_bags_board_slot_mirror = inbound_link_board_slot_mirror.clone();
        dropped_bag_counter
            .mirror_every_new_total_into(Box::new(move |dropped_bags| {
                dropped_bags_board_slot_mirror.mirror_dropped_bags(dropped_bags)
            }))
            .map_err(|_| already_mirrored())?;
        let bags_off_the_mailbox_board_slot_mirror = inbound_link_board_slot_mirror.clone();
        let bags_counted_lost_after_taking_board_slot_mirror =
            inbound_link_board_slot_mirror.clone();
        dropped_bag_counter
            .mirror_every_new_taken_bag_total_into(
                Box::new(move |bags_off_the_mailbox| {
                    bags_off_the_mailbox_board_slot_mirror
                        .mirror_bags_off_the_mailbox_for_its_reader(bags_off_the_mailbox)
                }),
                Box::new(move |bags_counted_lost_after_taking| {
                    bags_counted_lost_after_taking_board_slot_mirror
                        .mirror_bags_its_reader_counted_lost_after_taking_them(
                            bags_counted_lost_after_taking,
                        )
                }),
            )
            .map_err(|_| already_mirrored())?;
        if let Some(discarded_sample_counter) = discarded_sample_counter {
            discarded_sample_counter
                .mirror_every_new_total_into(Box::new(move |discarded_samples| {
                    inbound_link_board_slot_mirror.mirror_discarded_samples(discarded_samples)
                }))
                .map_err(|_| already_mirrored())?;
        }
        Ok(())
    }

    /// Every inbound link feeding `port`, in wiring order.
    ///
    /// Readable from `setup()`: a port's mailbox exists only once a link is
    /// wired into it, and WIRE runs before `setup()`, so a destination asking
    /// here learns how many producers it owes before the first bag arrives. A
    /// port with no links lists none rather than refusing — an unconnected
    /// input is a legal graph, not an error.
    pub fn inbound_link_names(&self, port: &str) -> Vec<InboundLinkName> {
        self.inbound_link_subscribers_and_listener
            .lock()
            .bound_to_local_port(port)
            .map(|bound| bound.inbound_link_name.clone())
            .collect()
    }

    /// Whether `stage` is still the stage installed on `port`, and the one link
    /// feeding the port if it is.
    ///
    /// Taken under both locks at once, so the link is the one wired with that
    /// stage: a window is cut from bags rather than being one, so no entry
    /// carries the link, but a windowed port takes exactly one (a second is
    /// refused at wire time).
    fn installation_of_a_windowed_ports_stage(
        &self,
        port: &str,
        stage: &SharedAudioWindowStage,
    ) -> WindowedPortStageInstallation {
        let subscribers_and_listener = self.inbound_link_subscribers_and_listener.lock();
        if !self
            .ports
            .lock()
            .get(port)
            .is_some_and(|port_config| port_config.audio_windowing.installs_stage(stage))
        {
            return WindowedPortStageInstallation::Replaced;
        }
        WindowedPortStageInstallation::StillInstalled {
            feeding_link: subscribers_and_listener
                .the_only_subscriber_bound_to_local_port(port)
                .map(|only| WindowedPortInboundLink {
                    link_id: only.link_id.clone(),
                    inbound_link_name: only.inbound_link_name.clone(),
                    dropped_bag_counter: only.dropped_bag_counter.clone(),
                    discarded_sample_counter: only.discarded_sample_counter.clone(),
                }),
        }
    }

    /// Count a windowed port's flush on the link feeding it, and say so.
    ///
    /// A port no single link feeds — a manually injected frame's — has nowhere
    /// to count it, and still says what went.
    fn count_a_flush_on_the_link_feeding(
        port: &str,
        feeding_link: Option<&WindowedPortInboundLink>,
        flush: AudioWindowStageFlush,
    ) {
        let AudioWindowStageFlush {
            cause,
            discarded_per_channel_samples_at_the_declared_rate: discarded_samples,
        } = flush;
        if let Some(counter) =
            feeding_link.and_then(|feeding| feeding.discarded_sample_counter.as_ref())
        {
            counter.record_discarded_samples(discarded_samples);
        }
        tracing::warn!(
            port,
            link = feeding_link.map(|feeding| feeding.link_id.as_str()),
            channel = feeding_link.map(|feeding| feeding.inbound_link_name.as_str()),
            discarded_samples,
            "audio window stage: {cause}, so the accumulator and the resampler's filter state \
             were flushed, discarding {discarded_samples} samples no reader had received"
        );
    }

    /// This processor's per-inbound-link dropped-bag counts, shared with the
    /// graph node's [`ProcessorMetrics`] so `graph` reads them live.
    ///
    /// [`ProcessorMetrics`]: crate::core::graph::ProcessorMetrics
    pub fn dropped_bag_counts_by_inbound_link(&self) -> Arc<DroppedBagCountsByInboundLink> {
        Arc::clone(&self.dropped_bag_counts)
    }

    /// This processor's per-inbound-link counts of samples its windowed ports'
    /// flushes discarded, shared with the graph node's [`ProcessorMetrics`].
    ///
    /// [`ProcessorMetrics`]: crate::core::graph::ProcessorMetrics
    pub fn discarded_sample_counts_by_inbound_link(
        &self,
    ) -> Arc<DiscardedSampleCountsByInboundLink> {
        Arc::clone(&self.discarded_sample_counts)
    }

    /// Reclaim the destination-side ports for one disconnected `connect()` link.
    ///
    /// Drops the `link_id`-tagged subscriber; when its local input port has no
    /// remaining subscribers the mailbox is removed, and when the destination has
    /// none at all the shared listener is dropped, so this destination holds
    /// nothing against a channel or notify service no link uses.
    pub fn remove_channel_link(&self, link_id: &str) {
        let mut subscribers_and_listener = self.inbound_link_subscribers_and_listener.lock();
        let Some(local_port) = subscribers_and_listener.remove_by_link(link_id) else {
            return;
        };
        self.dropped_bag_counts.forget_inbound_link(link_id);
        self.discarded_sample_counts.forget_inbound_link(link_id);
        if !subscribers_and_listener.local_port_still_bound(&local_port) {
            self.ports.lock().remove(&local_port);
        }
        if subscribers_and_listener.subscribers.is_empty() {
            subscribers_and_listener.listener = None;
        }
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

    /// Returns the underlying listener fd if a listener has been configured.
    ///
    /// The fd is owned by the [`Listener`] — callers must NOT `close()` it and
    /// MUST stop using it before [`InputMailboxesInner`] is dropped. Suitable
    /// for registering with an epoll set, a kqueue, or `poll` from the
    /// processor's execution thread — never `select`, which refuses a
    /// descriptor of 1024 or above.
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

    /// Receive all pending samples from every channel subscriber and queue them
    /// on mailboxes by the subscriber's local-port binding.
    ///
    /// A plain port queues the sample itself, so a read borrows it in place. A
    /// windowed port queues a copy, because its stage may hold more blocks than
    /// a link may borrow.
    ///
    /// Every loss this pass can see is counted on the link it happened to: the
    /// bags a jump in the sequence number says the ring overwrote, on an
    /// `ordered` port — a `newest` port passing over bags is the profile working
    /// — a sample bound to a port with no mailbox, and the newest sample of a
    /// link already holding its port's mailbox depth plus borrow quota, each of
    /// which consumed a number and reaches no reader. The messages a writer
    /// declared lost upstream of it are told to an `ordered` port's reader and
    /// counted on no link.
    ///
    /// This is called automatically by `read()` and `has_data()`, but can be
    /// called explicitly if needed.
    pub fn receive_pending(&self) {
        let mut subscribers_and_listener = self.inbound_link_subscribers_and_listener.lock();
        for bound in &mut subscribers_and_listener.subscribers {
            loop {
                let sample = match bound.subscriber.receive() {
                    Ok(Some(sample)) => sample,
                    Ok(None) => break, // no more samples on this subscriber
                    Err(iceoryx2::port::ReceiveError::ExceedsMaxBorrows) => {
                        tracing::trace!(
                            link = %bound.inbound_link_name,
                            "InputMailboxes: the link holds every sample its service lets one \
                             subscriber borrow; the rest wait in its ring"
                        );
                        break;
                    }
                    Err(e) => {
                        tracing::error!("InputMailboxes: subscriber.receive() FAILED: {:?}", e);
                        break;
                    }
                };
                let channel_user_header = *sample.user_header();
                let bags_the_ring_overwrote =
                    bound.bags_the_ring_overwrote.how_many_were_lost_before(
                        sample.origin(),
                        channel_user_header.sequence_number,
                    );
                let borrowed =
                    BorrowedChannelSample::holding(sample, &bound.samples_this_link_holds);
                let ports = self.ports.lock();
                let port_config = ports.get(&bound.local_port);
                if port_config
                    .is_some_and(|port| port.mailbox.read_mode().a_bag_passed_over_is_lost())
                {
                    if bags_the_ring_overwrote > 0 {
                        bound
                            .dropped_bag_counter
                            .record_dropped_bags(bags_the_ring_overwrote);
                    }
                    if channel_user_header.messages_lost_upstream_before_this_one > 0 {
                        bound
                            .dropped_bag_counter
                            .record_messages_the_writer_declared_lost_upstream(
                                channel_user_header.messages_lost_upstream_before_this_one,
                            );
                    }
                }
                let Some(port_config) = port_config else {
                    bound.dropped_bag_counter.record_one_dropped_bag();
                    tracing::warn!(
                        port = %bound.local_port,
                        link = %bound.inbound_link_name,
                        "InputMailboxes: channel delivered a frame but its bound local port has \
                         no mailbox; dropped and counted"
                    );
                    continue;
                };
                let payload = if port_config.audio_windowing.is_windowed() {
                    let area = borrowed.sample_payload_area_bytes();
                    InputPortMessagePayload::OwnedBytes(
                        channel_user_header
                            .payload_within(area)
                            .unwrap_or(area)
                            .to_vec(),
                    )
                } else {
                    let most_samples_the_link_may_hold =
                        port_config.mailbox.capacity() + port_config.borrow_quota;
                    if bound.samples_this_link_holds.held_sample_count()
                        > most_samples_the_link_may_hold
                    {
                        bound.dropped_bag_counter.record_one_dropped_bag();
                        tracing::trace!(
                            port = %bound.local_port,
                            link = %bound.inbound_link_name,
                            borrow_quota = port_config.borrow_quota,
                            "InputMailboxes: the link's reader holds past its borrow quota; the \
                             newest frame is dropped and counted"
                        );
                        continue;
                    }
                    InputPortMessagePayload::BorrowedChannelSample(borrowed)
                };
                port_config.mailbox.push_frame_from_inbound_link(
                    payload,
                    channel_user_header,
                    &bound.dropped_bag_counter,
                    &bound.inbound_link_name,
                );
            }
        }
    }

    /// The next bag body `port` hands a reader, following its read mode, or
    /// `None` when there is none — the one read seam every reader shares, and
    /// where a windowed port's stage cuts its windows.
    fn next_bag_for_the_reader(&self, port: &str) -> Result<Option<BagBodyForTheReader>> {
        self.receive_pending();

        let audio_windowing = self
            .ports
            .lock()
            .get(port)
            .map(|port_config| port_config.audio_windowing.clone())
            .ok_or_else(|| unknown_input_port(port))?;

        // The stage's decode, channel convert, resample and framing all run
        // here, with the `ports` mutex released — it guards the port map, and a
        // resampler pass is not port-map work.
        match audio_windowing {
            InstalledInputPortAudioWindowing::NotWindowed => {
                self.pop_one_bag_off_the_mailbox(port, WhereABagTakenOffTheMailboxGoes::ToTheReader)
            }
            // Nothing, and deliberately not the bag underneath: a port whose
            // contract is unsettled has no exact size to cut one to.
            InstalledInputPortAudioWindowing::AwaitingItsDeviceStreamFormat => Ok(None),
            InstalledInputPortAudioWindowing::Windowed(stage) => {
                self.next_window_out_of_the_stage(port, &stage)
            }
        }
    }

    /// Pop the next queued frame for `port` per its read mode and strip its
    /// header, or `None` when the mailbox is empty — or, bound for a stage, when
    /// that stage is no longer the one installed on the port, so a read that
    /// outlived a rewire takes none of the new wiring's bags.
    ///
    /// A bag bound for the reader is told of its losses under the port map
    /// lock, the lock every push and pop of a mailbox runs under, so no
    /// eviction or other pop lands between the pop and the telling.
    fn pop_one_bag_off_the_mailbox(
        &self,
        port: &str,
        where_the_bag_goes: WhereABagTakenOffTheMailboxGoes<'_>,
    ) -> Result<Option<BagBodyForTheReader>> {
        let (delivered, lost_since_last_read) = {
            let mut ports = self.ports.lock();
            let port_config = ports
                .get_mut(port)
                .ok_or_else(|| unknown_input_port(port))?;
            if let WhereABagTakenOffTheMailboxGoes::IntoTheStageWhileItIsInstalled(stage) =
                where_the_bag_goes
                && !port_config.audio_windowing.installs_stage(stage)
            {
                return Ok(None);
            }
            let popped = match port_config.mailbox.read_mode() {
                ReadMode::SkipToLatest => port_config.mailbox.pop_latest(),
                ReadMode::ReadNextInOrder => port_config.mailbox.pop(),
            };
            let Some(delivered) = popped else {
                return Ok(None);
            };
            let lost_since_last_read = match where_the_bag_goes {
                WhereABagTakenOffTheMailboxGoes::ToTheReader => {
                    port_config.dropped_bag_counter_of_the_bag_last_handed_to_the_reader =
                        delivered.dropped_bag_counter.clone();
                    delivered.dropped_bag_counter.as_ref().map_or(
                        0,
                        InboundLinkDroppedBagCounter::take_lost_messages_the_reader_has_not_been_told_of,
                    )
                }
                WhereABagTakenOffTheMailboxGoes::IntoTheStageWhileItIsInstalled(_) => 0,
            };
            (delivered, lost_since_last_read)
        };
        let inbound_link_name = delivered.inbound_link_name;
        let channel_user_header = delivered.channel_user_header;
        let payload_area_byte_len = match &delivered.payload {
            InputPortMessagePayload::BorrowedChannelSample(borrowed) => {
                borrowed.sample_payload_area_bytes().len()
            }
            InputPortMessagePayload::OwnedBytes(bytes) => bytes.len(),
        };
        let stamped_payload_bytes = channel_user_header.payload_byte_len as usize;
        if stamped_payload_bytes > payload_area_byte_len {
            return Err(Error::ChannelSampleStampsAPayloadPastTheSample {
                port: port.to_string(),
                stamped_payload_bytes,
                available_payload_bytes: payload_area_byte_len,
            });
        }
        match channel_user_header.payload_descriptor.known_kind() {
            Ok(
                DataChannelPayloadDescriptorKind::Bytes
                | DataChannelPayloadDescriptorKind::GpuPoolSlotFrame
                | DataChannelPayloadDescriptorKind::ImageFrame
                | DataChannelPayloadDescriptorKind::ChannelsBlock
                | DataChannelPayloadDescriptorKind::PacketsUnit
                | DataChannelPayloadDescriptorKind::RecordsMessage
                | DataChannelPayloadDescriptorKind::ArrayFrame,
            ) => {}
            Err(refusal) => {
                return Err(Error::Link(format!(
                    "a message on input port '{port}' was refused: {refusal}"
                )));
            }
        }
        let payload = match delivered.payload {
            InputPortMessagePayload::OwnedBytes(mut bytes) => {
                bytes.truncate(stamped_payload_bytes);
                InputPortMessagePayload::OwnedBytes(bytes)
            }
            borrowed => borrowed,
        };
        Ok(Some(BagBodyForTheReader {
            message: MessageReadFromInputPort {
                payload,
                timestamp_ns: channel_user_header.timestamp_ns,
                group_boundary: channel_user_header.group_boundary(),
                lost_since_last_read,
                payload_descriptor: channel_user_header.payload_descriptor,
            },
            inbound_link_name,
        }))
    }

    /// The next full window from a windowed port's stage, feeding it consumed
    /// bags until it can emit one or the mailbox runs dry.
    ///
    /// The emitted window carries the timestamp the stage derived from the
    /// device's own stamp on the source block, not the wire frame's — the
    /// frame header stamps when a bag was published, and a window's first
    /// sample is an instant inside the block it came from.
    fn next_window_out_of_the_stage(
        &self,
        port: &str,
        stage: &SharedAudioWindowStage,
    ) -> Result<Option<BagBodyForTheReader>> {
        let WindowedPortStageInstallation::StillInstalled { feeding_link } =
            self.installation_of_a_windowed_ports_stage(port, stage)
        else {
            return Ok(None);
        };
        loop {
            let ready_window = stage.lock().next_ready_window()?;
            if let Some(window) = ready_window {
                let (lost_since_last_read, inbound_link_name) =
                    feeding_link.as_ref().map_or((0, None), |feeding| {
                        (
                            feeding
                                .dropped_bag_counter
                                .take_lost_messages_the_reader_has_not_been_told_of(),
                            Some(feeding.inbound_link_name.clone()),
                        )
                    });
                return Ok(Some(BagBodyForTheReader {
                    message: MessageReadFromInputPort {
                        lost_since_last_read,
                        ..window.message
                    },
                    inbound_link_name,
                }));
            }
            let Some(bag) = self.pop_one_bag_off_the_mailbox(
                port,
                WhereABagTakenOffTheMailboxGoes::IntoTheStageWhileItIsInstalled(stage),
            )?
            else {
                return Ok(None);
            };
            let flush = stage.lock().accept(&bag.message)?;
            if let Some(flush) = flush {
                Self::count_a_flush_on_the_link_feeding(port, feeding_link.as_ref(), flush);
            }
        }
    }

    /// The next message on `port`, its payload untouched, or `None` when the
    /// mailbox is empty.
    pub fn read_raw(&self, port: &str) -> Result<Option<MessageReadFromInputPort>> {
        if let Some(island_port) = self.function_node_island_feeding(port) {
            return island_port.read_the_island_output();
        }
        if let Some(pinned_port) = self.pin_value_read_by_an_unlinked(port) {
            return pinned_port.read_the_island_output();
        }
        Ok(self
            .next_bag_for_the_reader(port)?
            .map(|bag_for_the_reader| bag_for_the_reader.message))
    }

    /// The next frame for `port` with the inbound link it arrived on.
    ///
    /// The read a destination fanning in N links uses: the mailbox already
    /// queues every frame holding the link it came in on — that is how an
    /// eviction is charged to the right one — and this hands that identity back
    /// rather than leaving a reader unable to tell its producers apart.
    ///
    /// Bags from one link arrive in the order that link sent them. Nothing is
    /// promised about how two links interleave: that follows the receive pass,
    /// not the stamps, so a reader that needs time order reasons per link.
    ///
    /// A bag no link delivered has no link to name. It is refused by name
    /// rather than given another link's, and the refusal **consumes** it: the
    /// mailbox admits no peek, so a frame is off the queue before its link is
    /// looked at. [`Self::read_raw`] names no link and is the read for such a
    /// bag.
    pub fn read_raw_from_inbound_link(
        &self,
        port: &str,
    ) -> Result<Option<(MessageReadFromInputPort, InboundLinkName)>> {
        if self.function_node_island_feeding(port).is_some() {
            return Err(Error::Link(format!(
                "input port '{port}' is fed by a function node island, evaluated at each read \
                 rather than delivered over a link, so there is no link to name. Read it with \
                 `read_raw`, which names no link"
            )));
        }
        if self.pin_value_read_by_an_unlinked(port).is_some() {
            return Err(Error::Link(format!(
                "input port '{port}' has no link and reads its pin value, so there is no link \
                 to name. Read it with `read_raw`, which names no link"
            )));
        }
        let Some(mut message_body_for_the_reader) = self.next_bag_for_the_reader(port)? else {
            return Ok(None);
        };
        let Some(inbound_link_name) = message_body_for_the_reader.inbound_link_name.take() else {
            return Err(Error::Link(format!(
                "a bag on input port '{port}' arrived with no inbound link behind it, so \
                 there is no link to name and the bag is consumed. Read such a port with \
                 `read_raw`, which names no link"
            )));
        };
        Ok(Some((
            message_body_for_the_reader.message,
            inbound_link_name,
        )))
    }

    /// The next msgpack message on `port` deserialized into `T`, with the inbound
    /// link it arrived on, or `None` when the mailbox is empty.
    ///
    /// `None` rather than an error on an empty mailbox, unlike
    /// [`InputMailboxes::read_msgpack`]: a destination fanning in N links reads
    /// until its port runs dry, and an empty port is that loop ending.
    pub fn read_msgpack_from_inbound_link<T: DeserializeOwned>(
        &self,
        port: &str,
    ) -> Result<Option<(T, InboundLinkName)>> {
        let Some((message, inbound_link_name)) = self.read_raw_from_inbound_link(port)? else {
            return Ok(None);
        };
        Ok(Some((
            deserialize_msgpack_payload(&message.payload)?,
            inbound_link_name,
        )))
    }

    /// Count the message last read from `port` as lost on the inbound link
    /// that delivered it, once.
    ///
    /// For a reader that took a message and found what it names gone — a frame
    /// its producer recycled before the reader's claim. The loss lands in the
    /// link's dropped count `graph` renders and in the next message's
    /// `lost_since_last_read` from that link, never as an error. Refused by name
    /// when nothing read from the port since its last count came over a link:
    /// no read yet, that message already counted, a bag no link delivered, or a
    /// window.
    pub fn count_the_message_last_read_as_lost(&self, port: &str) -> Result<()> {
        let dropped_bag_counter = self
            .ports
            .lock()
            .get_mut(port)
            .ok_or_else(|| unknown_input_port(port))?
            .dropped_bag_counter_of_the_bag_last_handed_to_the_reader
            .take()
            .ok_or_else(|| {
                Error::Link(format!(
                    "no message read from input port '{port}' since its last counted loss came \
                     over a link, so there is no link to count this loss on — nothing was read, \
                     the message was already counted, no link delivered it, or it was a window"
                ))
            })?;
        dropped_bag_counter.record_one_bag_its_reader_counted_lost_after_taking_it();
        Ok(())
    }

    /// Check if a port has any payloads available. This first
    /// receives any pending data from the iceoryx2 Subscriber.
    ///
    /// On a port declaring a window contract this means a full window, not an
    /// arrived bag: the contract promises `process()` exact-size blocks, so a
    /// reactive processor that woke and found nothing would contradict it.
    pub fn has_data(&self, port: &str) -> bool {
        if self.function_node_island_feeding(port).is_some()
            || self.pin_value_read_by_an_unlinked(port).is_some()
        {
            return true;
        }
        self.receive_pending();
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
    pub fn any_port_has_data(&self) -> bool {
        self.receive_pending();
        self.any_ports_read_would_return_something()
    }

    /// What one port would hand a reader, gathered under the `ports` lock so
    /// the judgement itself is made with that lock released.
    fn readiness_of_one_port(&self, port: &str) -> Option<PortReadiness> {
        self.ports.lock().get(port).map(PortReadiness::of)
    }

    /// Whether any port would hand a reader something.
    ///
    /// Answers under the lock for every port that needs no judging, and
    /// collects only the windowed ones — normally none — so a processor with no
    /// window contract pays no allocation on a path the reactive runner walks
    /// once per wake and once per drain iteration.
    fn any_ports_read_would_return_something(&self) -> bool {
        let windowed: Vec<PortReadiness> = {
            let ports = self.ports.lock();
            let mut windowed = Vec::new();
            for readiness in ports.values().map(PortReadiness::of) {
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

/// Public input mailboxes handle. The macro emits
/// `pub inputs: InputMailboxes` on every processor struct that
/// declares input ports.
///
/// The sole field is an opaque pointer to the host's
/// [`InputMailboxesInner`]. `Clone` bumps the `Arc<InputMailboxesInner>`
/// strong count; `Drop` decrements it.
///
/// A port is named in any spelling that casts to the name it registered
/// under, so `"videoOut"` reaches the port registered as `videoout`.
pub struct InputMailboxes {
    /// Opaque handle: `Arc::into_raw(Arc<InputMailboxesInner>)`. Null
    /// on a freshly-constructed processor before
    /// `set_iceoryx2_resources` fires.
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

    /// Settle this processor's own input port declaring
    /// `audio_window = match_device`, from the format of the device stream it
    /// just opened.
    ///
    /// The `RuntimeContextFullAccess` is the gate rather than a value this
    /// reads: the Full capability typestate exists only in `setup()` and
    /// `teardown()`, which is where the plan puts a processor's resource
    /// requests, and taking it here is what keeps a dynamic contract off the
    /// `process()` surface. There is no Python spelling of this and no control
    /// -plane verb for it — only a processor that opens a device stream holds
    /// the format to settle one with.
    ///
    /// Refuses a handle that was never given its inner — a processor with no
    /// iceoryx2 input resources at all, which cannot have the port this names.
    /// An input port that simply has no link is not that: its contract settles
    /// and waits, and the port materialises resolved when a link is wired.
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
mod tests {
    use super::*;
    use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
    use crate::core::test_support::CapturedTracingWarnings;
    use crate::iceoryx2::channel_sample_test_support::TestChannelSample;
    use crate::iceoryx2::{
        FunctionNodeIslandEvaluatedAtEachRead, FunctionNodeIslandOutputForOneRead,
        FunctionNodeIslandOutputValue,
    };

    struct IslandAnsweringOneHalf;

    impl FunctionNodeIslandEvaluatedAtEachRead for IslandAnsweringOneHalf {
        fn evaluate_the_island_output_for_this_read(
            &self,
        ) -> Result<FunctionNodeIslandOutputForOneRead> {
            Ok(FunctionNodeIslandOutputForOneRead {
                value: FunctionNodeIslandOutputValue::ControlValue(0.5),
                evaluated_at_ns: 0,
            })
        }

        fn the_first_function_node_it_runs_as_python(&self) -> Option<&str> {
            None
        }

        fn the_state_each_stateful_function_node_last_returned(&self) -> Vec<(u32, Vec<f64>)> {
            Vec::new()
        }
    }

    struct IslandRunningOneNodeAsPython;

    impl FunctionNodeIslandEvaluatedAtEachRead for IslandRunningOneNodeAsPython {
        fn evaluate_the_island_output_for_this_read(
            &self,
        ) -> Result<FunctionNodeIslandOutputForOneRead> {
            Ok(FunctionNodeIslandOutputForOneRead {
                value: FunctionNodeIslandOutputValue::ControlValue(0.5),
                evaluated_at_ns: 0,
            })
        }

        fn the_first_function_node_it_runs_as_python(&self) -> Option<&str> {
            Some("tally")
        }

        fn the_state_each_stateful_function_node_last_returned(&self) -> Vec<(u32, Vec<f64>)> {
            vec![(7, vec![3.0])]
        }
    }

    #[test]
    fn the_function_node_an_island_runs_as_python_is_named_by_the_port_it_feeds() {
        let mailboxes = InputMailboxesInner::new();
        mailboxes.feed_an_input_port_from_a_function_node_island(
            "brightness",
            Arc::new(IslandAnsweringOneHalf),
        );
        mailboxes.feed_an_input_port_from_a_function_node_island(
            "count",
            Arc::new(IslandRunningOneNodeAsPython),
        );
        assert_eq!(
            mailboxes.the_function_node_run_as_python_by_the_island_feeding("brightness"),
            None
        );
        assert_eq!(
            mailboxes.the_function_node_run_as_python_by_the_island_feeding("count"),
            Some("tally".to_string())
        );
        assert_eq!(
            mailboxes.the_function_node_run_as_python_by_the_island_feeding("unfed"),
            None
        );
    }

    #[test]
    fn an_unlinked_input_port_reads_its_pin_value_until_a_link_feeds_it() {
        let mailboxes = InputMailboxesInner::new();
        let stream_clock_now_ns: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(|| 1_000);
        mailboxes.read_each_unlinked_input_port_as_its_pin_value(
            HashMap::from([("level".to_string(), 0.25)]),
            Arc::clone(&stream_clock_now_ns),
        );
        assert!(mailboxes.has_port("level"));
        assert!(mailboxes.has_data("level"));
        assert!(!mailboxes.any_port_has_data());
        let message = mailboxes.read_raw("level").unwrap().unwrap();
        assert_eq!(message.payload.bytes(), 0.25f64.to_le_bytes());
        assert_eq!(message.timestamp_ns, 1_000);
        assert_eq!(start_sample_of_the_next_read(&mailboxes, "level"), 1);

        mailboxes.read_each_unlinked_input_port_as_its_pin_value(
            HashMap::from([("level".to_string(), 0.75)]),
            Arc::clone(&stream_clock_now_ns),
        );
        let message = mailboxes.read_raw("level").unwrap().unwrap();
        assert_eq!(message.payload.bytes(), 0.75f64.to_le_bytes());
        assert_eq!(
            streamlib_ipc_types::ChannelsBlockDescriptorBody::from_descriptor_body(
                &message.payload_descriptor.body,
            )
            .start_sample,
            2,
            "a pin edit counts reads on from where the port was"
        );

        let (_publisher, subscriber) = open_channel_for_one_link("pin/linked", 4);
        mailboxes.add_port("level", 8, ReadMode::SkipToLatest);
        mailboxes.add_channel_subscriber(
            "level",
            "L-lfo",
            &InboundLinkName::from("plfo/result"),
            subscriber,
        );
        assert!(
            mailboxes.read_raw("level").unwrap().is_none(),
            "a linked port reads its link, which has sent nothing"
        );
        mailboxes.remove_channel_link("L-lfo");
        assert!(mailboxes.read_raw("level").unwrap().is_some());

        mailboxes
            .read_each_unlinked_input_port_as_its_pin_value(HashMap::new(), stream_clock_now_ns);
        assert!(
            !mailboxes.has_port("level"),
            "a port with no pin value and no link left is not one this processor reads"
        );
    }

    fn start_sample_of_the_next_read(mailboxes: &InputMailboxesInner, port: &str) -> u64 {
        let message = mailboxes.read_raw(port).unwrap().unwrap();
        streamlib_ipc_types::ChannelsBlockDescriptorBody::from_descriptor_body(
            &message.payload_descriptor.body,
        )
        .start_sample
    }

    #[test]
    fn an_island_replacing_the_one_feeding_a_port_carries_its_sample_positions_on() {
        let mailboxes = InputMailboxesInner::new();
        mailboxes.feed_an_input_port_from_a_function_node_island(
            "brightness",
            Arc::new(IslandAnsweringOneHalf),
        );
        assert_eq!(start_sample_of_the_next_read(&mailboxes, "brightness"), 0);
        assert_eq!(start_sample_of_the_next_read(&mailboxes, "brightness"), 1);
        mailboxes.feed_an_input_port_from_a_function_node_island(
            "brightness",
            Arc::new(IslandRunningOneNodeAsPython),
        );
        assert_eq!(start_sample_of_the_next_read(&mailboxes, "brightness"), 2);
        assert_eq!(
            mailboxes.the_function_node_run_as_python_by_the_island_feeding("brightness"),
            Some("tally".to_string()),
            "the replacing island answers the port's reads"
        );
        assert_eq!(
            mailboxes.the_state_each_stateful_function_node_of_the_island_feeding_last_returned(
                "brightness"
            ),
            vec![(7, vec![3.0])]
        );
        assert!(
            mailboxes
                .the_state_each_stateful_function_node_of_the_island_feeding_last_returned("unfed")
                .is_empty()
        );
    }

    #[test]
    fn a_port_an_island_stops_feeding_is_no_longer_named_among_the_ports() {
        let mailboxes = InputMailboxesInner::new();
        mailboxes.feed_an_input_port_from_a_function_node_island(
            "brightness",
            Arc::new(IslandAnsweringOneHalf),
        );
        assert!(mailboxes.stop_feeding_an_input_port_from_a_function_node_island("brightness"));
        assert!(!mailboxes.has_port("brightness"));
        assert!(!mailboxes.stop_feeding_an_input_port_from_a_function_node_island("brightness"));
    }

    #[test]
    fn a_port_an_island_feeds_is_named_among_the_ports_once() {
        let mailboxes = InputMailboxesInner::new();
        mailboxes.feed_an_input_port_from_a_function_node_island(
            "brightness",
            Arc::new(IslandAnsweringOneHalf),
        );
        assert!(mailboxes.has_port("brightness"));
        assert_eq!(mailboxes.port_names(), vec!["brightness".to_string()]);
    }

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

    /// Driving the iceoryx2 Event service end-to-end: notify must transition
    /// the Listener fd to readable within a short bounded window so an epoll
    /// or poll wait wakes promptly.
    #[test]
    fn listener_fd_is_valid_and_readable_after_notify() {
        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let name = unique_suffix("notify");

        let svc = node
            .service_builder(&ServiceName::new(&name).unwrap())
            .event()
            .max_notifiers(2)
            .max_listeners(1)
            .open_or_create()
            .unwrap();
        let notifier = svc.notifier_builder().create().unwrap();
        let listener =
            crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&svc)
                .unwrap();

        let mailboxes = InputMailboxesInner::new();
        mailboxes.set_listener(listener);
        let fd = mailboxes
            .listener_fd()
            .expect("listener_fd should be set after set_listener");
        assert!(fd >= 0, "listener fd should be a valid posix fd, got {fd}");

        // Pre-flight: not readable.
        assert!(!poll_readable(fd, 0));

        notifier.notify().unwrap();

        // Bounded wait: the issue requires the fd to report readable within
        // 50 ms. Using a 50 ms poll matches that contract.
        assert!(
            poll_readable(fd, 50),
            "listener fd should be readable within 50 ms of notify()"
        );

        // After draining, the fd transitions back to not-readable so the
        // next wait blocks again instead of spinning.
        mailboxes.drain_listener();
        assert!(!poll_readable(fd, 0));
    }

    fn poll_readable(fd: i32, timeout_ms: i32) -> bool {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: fd is a valid POSIX fd for the lifetime of this call;
        // pfd is on the stack and not aliased.
        let n = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        n > 0 && (pfd.revents & libc::POLLIN) != 0
    }

    /// Regression lock for the reactive-scheduler burst-drain path:
    /// `any_port_has_data()` must reflect total queued depth across all
    /// configured ports.
    #[test]
    fn any_port_has_data_reflects_total_queued_depth() {
        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("port_a", 64, ReadMode::ReadNextInOrder);
        mailboxes.add_port("port_b", 64, ReadMode::ReadNextInOrder);

        assert!(
            !mailboxes.any_port_has_data(),
            "empty mailboxes report no data"
        );

        let link_into_port_a = a_test_link_into(&mailboxes, "port_a");
        let link_into_port_b = a_test_link_into(&mailboxes, "port_b");
        let frame = TestChannelSample::carrying(0, &[1, 2, 3, 4]);

        // Burst: three frames on port_a, two on port_b.
        for _ in 0..3 {
            link_into_port_a.send_numbered(&frame);
        }
        for _ in 0..2 {
            link_into_port_b.send_numbered(&frame);
        }

        // All 5 are queued; any_port_has_data sees them.
        assert!(
            mailboxes.any_port_has_data(),
            "five queued frames must report has_data"
        );

        // Drain port_a entirely via read_raw (skips msgpack deserialization
        // of the synthetic payload).
        for _ in 0..3 {
            assert!(
                mailboxes
                    .read_raw("port_a")
                    .expect("read_raw port_a ok")
                    .is_some(),
                "port_a should still have a frame",
            );
        }
        assert!(
            mailboxes.any_port_has_data(),
            "port_a empty but port_b still has 2 frames",
        );

        // Drain the other.
        for _ in 0..2 {
            assert!(
                mailboxes
                    .read_raw("port_b")
                    .expect("read_raw port_b ok")
                    .is_some(),
                "port_b should still have a frame",
            );
        }
        assert!(
            !mailboxes.any_port_has_data(),
            "both ports drained — must report no data",
        );
    }

    /// N→1 fan-in delivery: a destination consuming TWO inbound
    /// channels binds two subscribers to ONE local input port; `receive_pending`
    /// routes every frame from both channels into that shared mailbox.
    ///
    /// The two source channels stamp DIFFERENT source ports, so the routing must
    /// be by the subscriber→local-port binding, not the frame's stamped key.
    /// Revert lock: route by the stamped source port instead (as `route()` does)
    /// and both frames look for mailboxes named after the source ports — which
    /// don't exist on this destination — so the "in" mailbox stays empty and the
    /// two-frame assertion fails.
    #[test]
    fn two_channel_subscribers_fan_into_one_local_port() {
        let (publisher_a, sub_a) = open_channel_for_one_link("fanin/a", 1);
        let (publisher_b, sub_b) = open_channel_for_one_link("fanin/b", 1);
        publish_one_frame(&publisher_a, "src_a_out", b"frame-from-a");
        publish_one_frame(&publisher_b, "src_b_out", b"frame-from-b");

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", 64, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "in",
            "L-fanin-a",
            &InboundLinkName::from("pfanin-a/out"),
            sub_a,
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-fanin-b",
            &InboundLinkName::from("pfanin-b/out"),
            sub_b,
        );

        let mut payloads: Vec<Vec<u8>> = Vec::new();
        while let Some(message) = mailboxes.read_raw("in").unwrap() {
            payloads.push(message.payload.to_vec());
        }
        payloads.sort();
        assert_eq!(
            payloads,
            vec![b"frame-from-a".to_vec(), b"frame-from-b".to_vec()],
            "both inbound channels must fan into the one local input port's mailbox",
        );
    }

    /// The counting contract under fan-in: a stalled `ordered` consumer whose
    /// port two links feed reports each link's OWN losses, and the counts sum
    /// to published minus delivered — the 78-of-378 audio arrangement, made
    /// visible.
    ///
    /// The attribution is what makes this more than a total. Frames arrive
    /// A×5 then B×5 into a depth-2 mailbox, so B's arrivals evict A's bags:
    /// charging the pushing link would report A=3/B=5, charging the link the
    /// evicted bag came in on reports A=5/B=3. The tag rides the entry for
    /// exactly this reason.
    ///
    /// The stall here is the consumer never reading — the transport is pumped
    /// after every publish on purpose, so what the counts account for is
    /// eviction at the mailbox alone. A consumer parked deeper, pumping no
    /// receive at all, overflows the iceoryx2 subscriber ring instead, which
    /// the sequence-number gap counts on the same link.
    #[test]
    fn each_inbound_link_reports_its_own_losses_at_a_stalled_ordered_port() {
        const MAILBOX_DEPTH: usize = 2;
        const FRAMES_PER_LINK: usize = 5;

        let (publisher_a, subscriber_a) =
            open_channel_for_one_link("drop-count/a", FRAMES_PER_LINK);
        let (publisher_b, subscriber_b) =
            open_channel_for_one_link("drop-count/b", FRAMES_PER_LINK);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", MAILBOX_DEPTH, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "in",
            "L-first",
            &InboundLinkName::from("pfirst/out"),
            subscriber_a,
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-second",
            &InboundLinkName::from("psecond/out"),
            subscriber_b,
        );

        for _ in 0..FRAMES_PER_LINK {
            publish_one_frame(&publisher_a, "src_a_out", b"from-a");
            mailboxes.receive_pending();
        }
        for _ in 0..FRAMES_PER_LINK {
            publish_one_frame(&publisher_b, "src_b_out", b"from-b");
            mailboxes.receive_pending();
        }

        let counts = mailboxes
            .dropped_bag_counts_by_inbound_link()
            .dropped_bag_count_snapshot_by_inbound_link();
        assert_eq!(
            counts,
            std::collections::BTreeMap::from([
                ("L-first".to_string(), 5),
                ("L-second".to_string(), 3),
            ]),
            "each link must carry the bags IT lost, not the ones it displaced",
        );

        let delivered = mailboxes.drain("in").len();
        assert_eq!(delivered, MAILBOX_DEPTH);
        assert_eq!(
            counts.values().sum::<u64>() + delivered as u64,
            (FRAMES_PER_LINK * 2) as u64,
            "counted plus delivered must account for every bag published",
        );
    }

    /// A helper's losses reach its board as they are counted, with nothing
    /// pumping them there: a mailbox eviction lands on the evicted bag's link's
    /// slot, and a windowed port's flush on its one link's slot.
    ///
    /// Fail-without-fix: mirror nothing, or only on a pass the helper's loop
    /// makes, and both slots still read zero when the loss has already
    /// happened.
    #[test]
    fn every_loss_counted_on_a_mirrored_link_reaches_its_board_slot_as_it_is_counted() {
        let (board, board_writer, _helper_node) =
            crate::iceoryx2::a_loss_count_board_and_its_helpers_writer_for_this_test_process(
                &[],
                &[],
            );
        let mailboxes = InputMailboxesInner::new();

        let (plain_publisher, plain_subscriber) = open_channel_for_one_link("mirrored/plain", 5);
        mailboxes.add_port("in", 2, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "in",
            "L-plain",
            &InboundLinkName::from("pplain/out"),
            plain_subscriber,
        );
        mailboxes
            .mirror_an_inbound_links_loss_counts_into(
                "L-plain",
                board_writer
                    .claim_inbound_link_slot(
                        crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                            slot: 0,
                            wiring_generation: 11,
                        },
                    )
                    .unwrap(),
            )
            .expect("a bound link's counts mirror");

        let (windowed_publisher, windowed_subscriber) =
            open_channel_for_one_link("mirrored/windowed", 4);
        mailboxes.add_windowed_port(
            "audio",
            ReadMode::ReadNextInOrder,
            a_512_512_contract_at(16_000, 1),
        );
        mailboxes.add_channel_subscriber(
            "audio",
            "L-windowed",
            &InboundLinkName::from("pwindowed/out"),
            windowed_subscriber,
        );
        mailboxes
            .mirror_an_inbound_links_loss_counts_into(
                "L-windowed",
                board_writer
                    .claim_inbound_link_slot(
                        crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                            slot: 1,
                            wiring_generation: 12,
                        },
                    )
                    .unwrap(),
            )
            .expect("a windowed link's counts mirror");

        for _ in 0..5 {
            publish_one_frame(&plain_publisher, "src_out", b"bag");
            mailboxes.receive_pending();
        }
        assert_eq!(
            board.inbound_link_slot(0),
            Some(streamlib_ipc_types::InboundLinkLossCountBoardSlot {
                wiring_generation: 11,
                dropped_bags: 3,
                ..Default::default()
            }),
        );
        let _ = mailboxes.read_raw("in").unwrap();
        assert_eq!(
            board
                .inbound_link_slot(0)
                .map(|slot| slot.bags_off_the_mailbox_for_its_reader),
            Some(1),
            "a bag read reaches the slot as taken as it is read"
        );

        publish_one_mono_channels_block(&windowed_publisher, 300, 16_000, 0);
        assert!(mailboxes.read_raw("audio").unwrap().is_none());
        publish_one_mono_channels_block(&windowed_publisher, 300, 16_000, 1_000_000_000);
        let _ = mailboxes.read_raw("audio").unwrap();
        assert_eq!(
            board.inbound_link_slot(1).map(|slot| (
                slot.wiring_generation,
                slot.dropped_bags,
                slot.discarded_samples
            )),
            Some((12, 0, 300)),
        );

        assert!(
            mailboxes
                .mirror_an_inbound_links_loss_counts_into(
                    "L-plain",
                    board_writer
                        .claim_inbound_link_slot(
                            crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                                slot: 2,
                                wiring_generation: 13,
                            }
                        )
                        .unwrap(),
                )
                .is_err(),
            "a link mirrors onto one slot"
        );
        assert!(
            mailboxes
                .mirror_an_inbound_links_loss_counts_into(
                    "L-never-wired",
                    board_writer
                        .claim_inbound_link_slot(
                            crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                                slot: 3,
                                wiring_generation: 14,
                            }
                        )
                        .unwrap(),
                )
                .is_err(),
            "a link with no subscriber has no counts to mirror"
        );
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

    /// A publisher that replaced the one a live subscriber was hearing from
    /// numbers its sends from zero again, and the first of them the subscriber
    /// receives is a new baseline, never a gap measured against the old one.
    ///
    /// The replacement's first two bags are overwritten in a four-deep ring, so
    /// its first received number is two past the old publisher's last.
    /// Fail-without-fix: compare numbers without the publisher that sent them
    /// and the link reports one lost bag the replacement never counted as sent
    /// to it.
    #[test]
    fn a_replacement_publisher_on_a_live_subscriber_reads_as_a_new_baseline_not_a_gap() {
        const RING_DEPTH: usize = 4;
        let channel = crate::iceoryx2::Iceoryx2Node::for_this_test_process()
            .open_or_create_service(&unique_suffix("replaced-publisher"), 2, 16)
            .unwrap();
        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", 16, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "in",
            "L-replaced",
            &InboundLinkName::from("psource/out"),
            channel.create_subscriber(RING_DEPTH).unwrap(),
        );

        let output_writer = output_writer_publishing_onto(&channel, "out");
        output_writer
            .write_bytes_with_timestamp("out", b"old", 0, MessageGroupBoundary::StartsGroup)
            .unwrap();
        mailboxes.receive_pending();

        assert!(output_writer.remove_channel_link("out", "L-to-the-destination"));
        install_a_fresh_publisher(&output_writer, &channel, "out");
        for bag in 0..RING_DEPTH + 2 {
            output_writer
                .write_bytes_with_timestamp(
                    "out",
                    b"new",
                    bag as i64,
                    MessageGroupBoundary::StartsGroup,
                )
                .unwrap();
        }
        mailboxes.receive_pending();

        assert_eq!(
            dropped_bag_counts_of(&mailboxes),
            std::collections::BTreeMap::from([("L-replaced".to_string(), 0)]),
            "the replacement's first received bag is a baseline"
        );
        assert_eq!(mailboxes.drain("in").len(), 1 + RING_DEPTH);
    }

    /// A message written as bytes reaches `read_raw` as its producer wrote it:
    /// the payload untouched, its stamp, and its group flag cleared then set.
    #[test]
    fn a_message_written_as_bytes_reaches_read_raw_with_its_stamp_and_group_flag() {
        let channel = crate::iceoryx2::Iceoryx2Node::for_this_test_process()
            .open_or_create_service(&unique_suffix("bytes-to-read-raw"), 2, 16)
            .unwrap();
        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", 16, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "in",
            "L-bytes",
            &InboundLinkName::from("psource/out"),
            channel.create_subscriber(16).unwrap(),
        );
        let output_writer = output_writer_publishing_onto(&channel, "out");

        output_writer
            .write_bytes_with_timestamp(
                "out",
                b"continues-a-group",
                111,
                MessageGroupBoundary::ContinuesGroup,
            )
            .unwrap();
        output_writer
            .write_bytes_with_timestamp(
                "out",
                b"starts-a-group",
                222,
                MessageGroupBoundary::StartsGroup,
            )
            .unwrap();

        assert_eq!(
            mailboxes.read_raw("in").unwrap(),
            Some(MessageReadFromInputPort {
                payload: b"continues-a-group".to_vec().into(),
                timestamp_ns: 111,
                group_boundary: MessageGroupBoundary::ContinuesGroup,
                lost_since_last_read: 0,
                payload_descriptor: DataChannelPayloadDescriptor::bytes(),
            })
        );
        assert_eq!(
            mailboxes.read_raw_from_inbound_link("in").unwrap(),
            Some((
                MessageReadFromInputPort {
                    payload: b"starts-a-group".to_vec().into(),
                    timestamp_ns: 222,
                    group_boundary: MessageGroupBoundary::StartsGroup,
                    lost_since_last_read: 0,
                    payload_descriptor: DataChannelPayloadDescriptor::bytes(),
                },
                InboundLinkName::from("psource/out"),
            ))
        );
        assert_eq!(mailboxes.read_raw("in").unwrap(), None);
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

    /// A read hands back the channel sample itself, and the bytes it borrows
    /// stay as they were while the publisher keeps sending into the same
    /// channel.
    ///
    /// Fail-without-fix: copy the sample out at the receive and the payload is
    /// owned bytes, not the borrowed sample.
    #[test]
    fn a_borrowed_message_keeps_its_bytes_while_the_publisher_keeps_sending() {
        let (mailboxes, output_writer) =
            a_port_read_as_fed_by_a_native_writer("borrow-held", ReadMode::ReadNextInOrder);
        write_starting_a_group(&output_writer, "held");
        let held = mailboxes
            .read_raw("in")
            .unwrap()
            .expect("the first message");
        assert!(
            matches!(
                held.payload,
                InputPortMessagePayload::BorrowedChannelSample(_)
            ),
            "a read must borrow the sample, not copy it: {:?}",
            held.payload
        );
        let held_address = held.payload.as_ptr();
        assert_eq!(
            held_address as usize % 64,
            0,
            "the payload starts cache-line aligned"
        );

        for later in 0..40 {
            write_starting_a_group(&output_writer, &format!("later-{later:02}"));
            let _ = mailboxes.read_raw("in").unwrap();
        }

        assert_eq!(held.payload, b"held");
        assert_eq!(held.payload.as_ptr(), held_address);
    }

    /// A reader holding its quota past a full mailbox drops and counts only its
    /// own link's newest frames, and a second link on the same output, read as
    /// it arrives, receives every frame.
    ///
    /// Fail-without-fix: hold no per-link budget and the stalled reader's
    /// subscriber exhausts its borrow limit, so its frames are lost uncounted
    /// in its ring.
    #[test]
    fn a_link_past_its_quota_drops_only_its_own_frames() {
        const MAILBOX_DEPTH: usize = 4;
        const FRAMES_PAST_THE_FULL_MAILBOX: usize = 5;
        let channel = crate::iceoryx2::Iceoryx2Node::for_this_test_process()
            .open_or_create_service(&unique_suffix("quota-two-links"), 3, 16)
            .unwrap();
        let stalled = InputMailboxesInner::new();
        stalled.add_port("in", MAILBOX_DEPTH, ReadMode::ReadNextInOrder);
        stalled.add_channel_subscriber(
            "in",
            "L-stalled",
            &InboundLinkName::from("psource/out"),
            channel.create_subscriber(16).unwrap(),
        );
        let keeping_up = InputMailboxesInner::new();
        keeping_up.add_port("in", MAILBOX_DEPTH, ReadMode::ReadNextInOrder);
        keeping_up.add_channel_subscriber(
            "in",
            "L-keeping-up",
            &InboundLinkName::from("psource/out"),
            channel.create_subscriber(16).unwrap(),
        );
        let output_writer = output_writer_publishing_onto(&channel, "out");
        let quota = DEFAULT_INPUT_PORT_BORROW_QUOTA;

        let mut held_by_the_stalled_reader = Vec::new();
        let mut read_by_the_keeping_up_reader = 0;
        let total_frames = quota + MAILBOX_DEPTH + FRAMES_PAST_THE_FULL_MAILBOX;
        for frame in 0..total_frames {
            write_starting_a_group(&output_writer, &format!("frame-{frame}"));
            if held_by_the_stalled_reader.len() < quota {
                held_by_the_stalled_reader
                    .push(stalled.read_raw("in").unwrap().expect("a frame to hold"));
            } else {
                stalled.receive_pending();
            }
            while keeping_up.read_raw("in").unwrap().is_some() {
                read_by_the_keeping_up_reader += 1;
            }
        }

        assert_eq!(
            dropped_bag_counts_of(&stalled),
            std::collections::BTreeMap::from([(
                "L-stalled".to_string(),
                FRAMES_PAST_THE_FULL_MAILBOX as u64
            )]),
        );
        assert_eq!(
            dropped_bag_counts_of(&keeping_up),
            std::collections::BTreeMap::from([("L-keeping-up".to_string(), 0)]),
        );
        assert_eq!(read_by_the_keeping_up_reader, total_frames);

        drop(held_by_the_stalled_reader);
        write_starting_a_group(&output_writer, "after-the-release");
        stalled.receive_pending();
        let mut queued = stalled.drain("in");
        assert_eq!(queued.len(), MAILBOX_DEPTH);
        assert_eq!(queued.pop().unwrap(), b"after-the-release");
    }

    /// A port's quota is what it was set to: with a quota of zero, a message its
    /// reader holds takes a place in the mailbox, so the frame that would fill
    /// it is the newest one dropped.
    #[test]
    fn a_port_with_no_quota_drops_the_newest_frame_while_its_reader_holds_one() {
        let (mailboxes, output_writer) =
            a_port_read_as_fed_by_a_native_writer("quota-zero", ReadMode::ReadNextInOrder);
        mailboxes.set_input_port_borrow_quota("in", 0).unwrap();
        write_starting_a_group(&output_writer, "held");
        let _held = mailboxes.read_raw("in").unwrap().expect("the held message");
        for frame in 0..15 {
            write_starting_a_group(&output_writer, &format!("queued-{frame}"));
        }
        write_starting_a_group(&output_writer, "past-the-quota");
        mailboxes.receive_pending();

        assert_eq!(
            dropped_bag_counts_of(&mailboxes),
            std::collections::BTreeMap::from([("L-declared".to_string(), 1)]),
        );
        assert!(
            mailboxes
                .set_input_port_borrow_quota("not-a-port", 1)
                .is_err()
        );
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

    /// Groups a writer declared given up upstream reach an `ordered` reader on
    /// the next message as `lost_since_last_read`, and never as the link's
    /// drop: the link lost nothing.
    #[test]
    fn a_declared_upstream_loss_reaches_lost_since_last_read_on_an_ordered_port() {
        let (mailboxes, output_writer) =
            a_port_read_as_fed_by_a_native_writer("declared/ordered", ReadMode::ReadNextInOrder);

        write_starting_a_group(&output_writer, "before");
        output_writer
            .declare_groups_given_up_upstream_before_the_next_write("out", 2)
            .unwrap();
        write_starting_a_group(&output_writer, "after");
        write_starting_a_group(&output_writer, "later");

        assert_eq!(
            payloads_and_losses_read_off(&mailboxes, "in"),
            [("before", 0), ("after", 2), ("later", 0)]
                .map(|(payload, lost)| (payload.to_string(), lost)),
        );
        assert_eq!(
            dropped_bag_counts_of(&mailboxes),
            std::collections::BTreeMap::from([("L-declared".to_string(), 0)]),
            "a loss upstream of the writer is never the link's drop"
        );
    }

    /// A link's first sample is its numbering's baseline, yet a loss declared
    /// on it is told: the count rides its own header field.
    #[test]
    fn a_declared_upstream_loss_on_a_links_first_sample_is_still_counted() {
        let (mailboxes, output_writer) =
            a_port_read_as_fed_by_a_native_writer("declared/first", ReadMode::ReadNextInOrder);

        output_writer
            .declare_groups_given_up_upstream_before_the_next_write("out", 1)
            .unwrap();
        write_starting_a_group(&output_writer, "first");

        assert_eq!(
            payloads_and_losses_read_off(&mailboxes, "in"),
            [("first".to_string(), 1)],
        );
    }

    /// A `newest` port passing over messages is the profile working, so a loss
    /// declared upstream is not one either.
    #[test]
    fn a_newest_port_does_not_count_a_declared_upstream_loss() {
        let (mailboxes, output_writer) =
            a_port_read_as_fed_by_a_native_writer("declared/newest", ReadMode::SkipToLatest);

        output_writer
            .declare_groups_given_up_upstream_before_the_next_write("out", 3)
            .unwrap();
        write_starting_a_group(&output_writer, "latest");

        assert_eq!(
            payloads_and_losses_read_off(&mailboxes, "in"),
            [("latest".to_string(), 0)],
        );
        assert_eq!(
            dropped_bag_counts_of(&mailboxes),
            std::collections::BTreeMap::from([("L-declared".to_string(), 0)]),
        );
    }

    /// Every message a port hands a reader, as its payload and its
    /// `lost_since_last_read`, until the port runs dry.
    fn payloads_and_losses_read_off(
        mailboxes: &InputMailboxesInner,
        port: &str,
    ) -> Vec<(String, u64)> {
        std::iter::from_fn(|| mailboxes.read_raw(port).unwrap())
            .map(|message| {
                (
                    String::from_utf8(message.payload.to_vec()).unwrap(),
                    message.lost_since_last_read,
                )
            })
            .collect()
    }

    /// A pool slot resource nothing in this process holds, so its ring rehands
    /// it as soon as its turn comes round.
    struct PoolSlotNothingInThisProcessHolds;

    impl crate::core::context::lease_aware_pool_slot_ring::LeaseAwarePoolSlotResource
        for PoolSlotNothingInThisProcessHolds
    {
        fn is_held_in_this_process(&self) -> bool {
            false
        }
    }

    /// A consumer held until its producer has lapped a three-slot output ring
    /// reads the oldest queued frame, finds its id recycled at the claim, and
    /// counts it lost: one drop on the link `graph` renders, told to the next
    /// read from that link, and nothing raised.
    ///
    /// Fail-without-fix: with no read-side count, the recycled frame is lost
    /// with the link reporting no drops and the next read told of none.
    #[test]
    fn a_frame_its_producer_recycled_before_the_read_is_counted_lost_on_its_link() {
        use crate::core::context::lease_aware_pool_slot_ring::{
            ANY_ONE_UNHELD_SLOT_IS_REUSABLE, LeaseAwarePoolMintedFrameGenerations,
            LeaseAwarePoolSlotRing,
        };

        let mut producer_output_ring = LeaseAwarePoolSlotRing::default();
        for pool_slot_key in ["slot-a", "slot-b", "slot-c"] {
            producer_output_ring
                .push_fresh_slot(pool_slot_key.to_string(), PoolSlotNothingInThisProcessHolds);
        }
        let minted_frame_generations = LeaseAwarePoolMintedFrameGenerations::default();
        let (mailboxes, output_writer) = a_port_read_as_fed_by_a_native_writer(
            "recycled-before-the-read",
            ReadMode::ReadNextInOrder,
        );

        for _ in 0..=producer_output_ring.slot_count() {
            let slot_index = producer_output_ring
                .hand_off_a_reusable_slot(
                    ANY_ONE_UNHELD_SLOT_IS_REUSABLE,
                    None,
                    &minted_frame_generations,
                )
                .expect("nothing holds a slot, so the producer never waits");
            write_starting_a_group(
                &output_writer,
                &producer_output_ring
                    .slot(slot_index)
                    .currently_published_frame_id(),
            );
        }

        let read_claimed_and_lost_since_last_read: Vec<(String, bool, u64)> =
            std::iter::from_fn(|| mailboxes.read_raw("in").unwrap())
                .map(|message| {
                    let surface_id = String::from_utf8(message.payload.to_vec()).unwrap();
                    let claim = minted_frame_generations
                        .refusal_of_a_retired_frame_id_named(&surface_id)
                        .expect("every id names a slot the ring owns");
                    let claimed = match claim {
                        Ok(()) => true,
                        Err(Error::SurfaceFrameRecycled { .. }) => {
                            mailboxes
                                .count_the_message_last_read_as_lost("in")
                                .expect("a message a link delivered is countable");
                            false
                        }
                        Err(other) => panic!("only a recycled frame is refused: {other}"),
                    };
                    (surface_id, claimed, message.lost_since_last_read)
                })
                .collect();

        assert_eq!(
            read_claimed_and_lost_since_last_read,
            [
                ("slot-a#1", false, 0),
                ("slot-b#1", true, 1),
                ("slot-c#1", true, 0),
                ("slot-a#2", true, 0),
            ]
            .map(|(surface_id, claimed, lost)| (surface_id.to_string(), claimed, lost)),
        );
        assert_eq!(
            dropped_bag_counts_of(&mailboxes),
            std::collections::BTreeMap::from([("L-declared".to_string(), 1)]),
        );
    }

    /// A loss is counted once per message a link delivered: before any read,
    /// and a second time for one message, there is nothing to count, and each
    /// is refused naming the port.
    #[test]
    fn counting_a_message_lost_needs_one_a_link_delivered_and_counts_it_once() {
        let (mailboxes, output_writer) =
            a_port_read_as_fed_by_a_native_writer("counted-once", ReadMode::ReadNextInOrder);
        let refusal_before_any_read = mailboxes
            .count_the_message_last_read_as_lost("in")
            .expect_err("nothing has been read");
        assert!(
            refusal_before_any_read.to_string().contains("'in'"),
            "got: {refusal_before_any_read}"
        );

        write_starting_a_group(&output_writer, "only");
        mailboxes.read_raw("in").unwrap().expect("one message");
        mailboxes
            .count_the_message_last_read_as_lost("in")
            .expect("the message a link delivered is countable");
        assert!(
            mailboxes.count_the_message_last_read_as_lost("in").is_err(),
            "one message is one loss"
        );
        assert_eq!(
            dropped_bag_counts_of(&mailboxes),
            std::collections::BTreeMap::from([("L-declared".to_string(), 1)]),
        );
        assert!(matches!(
            mailboxes.count_the_message_last_read_as_lost("not-a-port"),
            Err(Error::Link(refusal)) if refusal.contains("Unknown input port")
        ));
    }

    /// Bags the ring overwrote behind bags already queued are told to the first
    /// bag read after them, never to the queued bags ahead of them.
    ///
    /// Two bags sit in the mailbox when the ring overwrites three more; the read
    /// of the first receives the ring's survivors before it pops. Fail-without-fix:
    /// tell each read the link's count as it stands at the read, and the first
    /// queued bag — which preceded the gap — is told of all three, so a decoder
    /// resuming at it decodes across the gap.
    #[test]
    fn a_ring_gap_behind_queued_bags_is_told_to_the_first_bag_after_it() {
        const RING_DEPTH: usize = 4;
        const BAGS_THE_RING_OVERWROTE: usize = 3;
        let (publisher, subscriber) =
            open_channel_for_one_link("loss-placement/ring-gap", RING_DEPTH);
        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", 64, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "in",
            "L-ring-gap",
            &InboundLinkName::from("psource/out"),
            subscriber,
        );

        publish_one_frame(&publisher, "out", b"queued-0");
        publish_one_frame(&publisher, "out", b"queued-1");
        mailboxes.receive_pending();
        for index in 0..RING_DEPTH + BAGS_THE_RING_OVERWROTE {
            publish_one_frame(&publisher, "out", format!("after-{index}").as_bytes());
        }

        assert_eq!(
            payloads_and_losses_read_off(&mailboxes, "in"),
            [
                ("queued-0", 0),
                ("queued-1", 0),
                ("after-3", BAGS_THE_RING_OVERWROTE as u64),
                ("after-4", 0),
                ("after-5", 0),
                ("after-6", 0),
            ]
            .map(|(payload, lost)| (payload.to_string(), lost)),
        );
    }

    /// A bag the mailbox evicted is told to the next bag read from its link,
    /// though that bag was queued before the eviction was counted.
    ///
    /// Four bags reach a two-deep mailbox in one receive, so the third and the
    /// fourth each evict one. Fail-without-fix: tell each bag the link's whole
    /// count as it stood when the bag was queued, and the third is told of one
    /// loss and the fourth of the other — the fourth was never behind a loss.
    #[test]
    fn an_evicted_bag_is_told_to_the_next_bag_read_from_its_link() {
        let (publisher, subscriber) = open_channel_for_one_link("loss-placement/eviction", 8);
        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", 2, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "in",
            "L-eviction",
            &InboundLinkName::from("psource/out"),
            subscriber,
        );

        for index in 0..4 {
            publish_one_frame(&publisher, "out", format!("bag-{index}").as_bytes());
        }

        assert_eq!(
            payloads_and_losses_read_off(&mailboxes, "in"),
            [("bag-2".to_string(), 2), ("bag-3".to_string(), 0)],
        );
        assert_eq!(
            mailboxes
                .dropped_bag_counts_by_inbound_link()
                .dropped_bag_count_snapshot_by_inbound_link()["L-eviction"],
            2,
            "telling the reader zeroes nothing: `graph` still renders the cumulative count"
        );
    }

    /// Under fan-in each read is told its own link's losses, never the other
    /// link's. Each link's first receive is the baseline its ring gaps are read
    /// against, so both deliver once before one is overrun.
    ///
    /// Fail-without-fix: keep one reader-side tally per port and the first bag
    /// read from the healthy link is told of the overrun link's losses.
    #[test]
    fn a_fan_in_read_is_told_only_the_losses_of_the_link_it_arrived_on() {
        const RING_DEPTH: usize = 2;
        let (overrun_publisher, overrun_subscriber) =
            open_channel_for_one_link("loss-placement/fan-in/overrun", RING_DEPTH);
        let (healthy_publisher, healthy_subscriber) =
            open_channel_for_one_link("loss-placement/fan-in/healthy", RING_DEPTH);
        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", 64, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "in",
            "L-overrun",
            &InboundLinkName::from("poverrun/out"),
            overrun_subscriber,
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-healthy",
            &InboundLinkName::from("phealthy/out"),
            healthy_subscriber,
        );

        publish_one_frame(&overrun_publisher, "out", b"overrun-first");
        publish_one_frame(&healthy_publisher, "out", b"healthy-first");
        mailboxes.receive_pending();
        for index in 0..RING_DEPTH + 3 {
            publish_one_frame(
                &overrun_publisher,
                "out",
                format!("overrun-{index}").as_bytes(),
            );
        }
        publish_one_frame(&healthy_publisher, "out", b"healthy-second");

        let mut losses_told_by_link: std::collections::BTreeMap<String, Vec<u64>> =
            std::collections::BTreeMap::new();
        while let Some((message, inbound_link_name)) =
            mailboxes.read_raw_from_inbound_link("in").unwrap()
        {
            losses_told_by_link
                .entry(inbound_link_name.as_str().to_string())
                .or_default()
                .push(message.lost_since_last_read);
        }
        assert_eq!(
            losses_told_by_link,
            std::collections::BTreeMap::from([
                ("phealthy/out".to_string(), vec![0, 0]),
                ("poverrun/out".to_string(), vec![0, 3, 0]),
            ]),
        );
    }

    /// A frame for a link whose local port has no mailbox consumed a sequence
    /// number and reaches no reader, so it is counted on its link.
    #[test]
    fn a_frame_bound_to_a_port_with_no_mailbox_is_counted_on_its_link() {
        let (publisher, subscriber) = open_channel_for_one_link("no-mailbox", 4);
        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_channel_subscriber(
            "in",
            "L-portless",
            &InboundLinkName::from("psource/out"),
            subscriber,
        );

        publish_one_frame(&publisher, "out", b"nowhere to go");
        publish_one_frame(&publisher, "out", b"nowhere to go either");
        mailboxes.receive_pending();

        assert_eq!(
            dropped_bag_counts_of(&mailboxes),
            std::collections::BTreeMap::from([("L-portless".to_string(), 2)]),
        );
    }

    // =========================================================================
    // Naming the inbound link a bag arrived on. The mailbox already knew — the
    // per-link drop counter is keyed by it — and these gate that a read hands
    // that identity back without disturbing anything it was already doing.
    // =========================================================================

    /// The read a many-track sink is built on: two producers on one `ordered`
    /// port, and every bag comes back naming the link it arrived on.
    ///
    /// Asserted as a set rather than a sequence on purpose. Bags from one link
    /// keep that link's order; how two links interleave follows the receive
    /// pass, and nothing promises otherwise — a test that pinned the
    /// interleaving would be pinning an artifact.
    #[test]
    fn two_inbound_links_hand_a_reader_the_link_each_bag_arrived_on() {
        let (publisher_a, sub_a) = open_channel_for_one_link("naming/a", 4);
        let (publisher_b, sub_b) = open_channel_for_one_link("naming/b", 4);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", 64, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "in",
            "L-camera",
            &InboundLinkName::from("pcamera/video_out"),
            sub_a,
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-microphone",
            &InboundLinkName::from("pmicrophone/audio_out"),
            sub_b,
        );

        publish_one_frame(&publisher_a, "video_out", b"from-the-camera");
        publish_one_frame(&publisher_b, "audio_out", b"from-the-microphone");
        publish_one_frame(&publisher_a, "video_out", b"from-the-camera-again");

        let mut named: Vec<(String, String)> = Vec::new();
        while let Some((message, inbound_link_name)) = mailboxes
            .read_raw_from_inbound_link("in")
            .expect("a delivered bag names its link")
        {
            named.push((
                String::from_utf8(message.payload.to_vec()).unwrap(),
                inbound_link_name.as_str().to_string(),
            ));
        }
        named.sort();

        assert_eq!(
            named,
            vec![
                (
                    "from-the-camera".to_string(),
                    "pcamera/video_out".to_string()
                ),
                (
                    "from-the-camera-again".to_string(),
                    "pcamera/video_out".to_string()
                ),
                (
                    "from-the-microphone".to_string(),
                    "pmicrophone/audio_out".to_string()
                ),
            ],
            "each bag must come back named by the source channel its own link \
             subscribed to, never by the link that pushed last",
        );
    }

    /// The new read is a read, not a second accounting path: the same overrun
    /// that charges each link its own losses charges exactly the same ones when
    /// the survivors are drained by name.
    ///
    /// Mirrors `each_inbound_link_reports_its_own_losses_at_a_stalled_ordered_port`
    /// bag for bag — A×5 then B×5 into a depth-2 mailbox — so a divergence
    /// between the two is the naming read having disturbed the counting.
    #[test]
    fn naming_the_inbound_link_a_bag_arrived_on_leaves_the_per_link_drop_counts_alone() {
        const MAILBOX_DEPTH: usize = 2;
        const FRAMES_PER_LINK: usize = 5;

        let (publisher_a, subscriber_a) =
            open_channel_for_one_link("naming-counts/a", FRAMES_PER_LINK);
        let (publisher_b, subscriber_b) =
            open_channel_for_one_link("naming-counts/b", FRAMES_PER_LINK);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", MAILBOX_DEPTH, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "in",
            "L-first",
            &InboundLinkName::from("pfirst/out"),
            subscriber_a,
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-second",
            &InboundLinkName::from("psecond/out"),
            subscriber_b,
        );

        for _ in 0..FRAMES_PER_LINK {
            publish_one_frame(&publisher_a, "src_a_out", b"from-a");
            mailboxes.receive_pending();
        }
        for _ in 0..FRAMES_PER_LINK {
            publish_one_frame(&publisher_b, "src_b_out", b"from-b");
            mailboxes.receive_pending();
        }

        let mut delivered = 0;
        while let Some((_, inbound_link_name)) = mailboxes
            .read_raw_from_inbound_link("in")
            .expect("a delivered bag names its link")
        {
            assert_eq!(
                inbound_link_name.as_str(),
                "psecond/out",
                "the survivors of this overrun are the second link's, and each must \
                 say so rather than inheriting the name of the link it displaced",
            );
            delivered += 1;
        }

        let counts = mailboxes
            .dropped_bag_counts_by_inbound_link()
            .dropped_bag_count_snapshot_by_inbound_link();
        assert_eq!(
            counts,
            std::collections::BTreeMap::from([
                ("L-first".to_string(), 5),
                ("L-second".to_string(), 3),
            ]),
            "reading by name must charge exactly what the plain read charges",
        );
        assert_eq!(delivered, MAILBOX_DEPTH);
        assert_eq!(
            counts.values().sum::<u64>() + delivered as u64,
            (FRAMES_PER_LINK * 2) as u64,
            "counted plus delivered must still account for every bag published",
        );
    }

    /// The typed read, and the one place it diverges from
    /// [`InputMailboxes::read_msgpack`]: a drained port is `Ok(None)` here where `read_msgpack`
    /// is an error. A destination fanning in N links reads until its port runs
    /// dry, and an empty port is that loop ending rather than a failure.
    #[test]
    fn the_typed_read_deserializes_the_bag_and_a_drained_port_is_not_an_error() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct OneTrackBag {
            track: String,
        }

        let (publisher, subscriber) = open_channel_for_one_link("naming/typed", 4);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", 8, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "in",
            "L-camera",
            &InboundLinkName::from("pcamera/video_out"),
            subscriber,
        );
        publish_one_frame(
            &publisher,
            "video_out",
            &rmp_serde::to_vec_named(&OneTrackBag {
                track: "front".to_string(),
            })
            .expect("the bag encodes"),
        );

        let (bag, inbound_link_name) = mailboxes
            .read_msgpack_from_inbound_link::<OneTrackBag>("in")
            .expect("a well-formed bag deserializes")
            .expect("one bag was published");
        assert_eq!(
            bag,
            OneTrackBag {
                track: "front".to_string()
            }
        );
        assert_eq!(inbound_link_name.as_str(), "pcamera/video_out");

        assert!(
            mailboxes
                .read_msgpack_from_inbound_link::<OneTrackBag>("in")
                .expect("a drained port is not a failure")
                .is_none(),
            "a drained port must end a sink's read loop rather than raise",
        );
    }

    /// A processor names its own port in the spelling it declared, and the
    /// handle casts it to the name the port was registered under.
    #[test]
    fn a_port_is_read_by_any_spelling_that_casts_to_its_name() {
        let (publisher, subscriber) = open_channel_for_one_link("naming/cast", 4);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("videoin", 8, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "videoin",
            "L-camera",
            &InboundLinkName::from("pcamera/videoout"),
            subscriber,
        );
        let mailboxes = InputMailboxes::from_inner_arc(Arc::new(mailboxes));

        assert!(mailboxes.has_port("videoIn"));
        assert_eq!(
            mailboxes.inbound_link_names("VideoIn"),
            vec![InboundLinkName::from("pcamera/videoout")]
        );
        assert!(!mailboxes.has_data("videoIn"));

        publish_one_frame(&publisher, "videoout", b"first");
        publish_one_frame(&publisher, "videoout", b"second");
        publish_one_frame(
            &publisher,
            "videoout",
            &rmp_serde::to_vec_named("third").expect("the bag encodes"),
        );

        assert!(mailboxes.has_data("videoIn"));
        let payload = mailboxes
            .read_raw("videoIn")
            .expect("a cast-alike spelling names the port")
            .expect("three bags were published")
            .payload;
        assert_eq!(payload, b"first");
        let (message, inbound_link_name) = mailboxes
            .read_raw_from_inbound_link("VideoIn")
            .expect("a cast-alike spelling names the port")
            .expect("two bags remain");
        assert_eq!(message.payload, b"second");
        assert_eq!(inbound_link_name.as_str(), "pcamera/videoout");
        let (bag, _inbound_link_name) = mailboxes
            .read_msgpack_from_inbound_link::<String>("VIDEOIN")
            .expect("a cast-alike spelling names the port")
            .expect("one bag remains");
        assert_eq!(bag, "third");

        let refusal = mailboxes.read_raw("%%").unwrap_err();
        assert!(
            matches!(refusal, Error::ExposedNameCastsToNothing(_)),
            "a name that casts to nothing names no port; got {refusal}"
        );
    }

    /// What a sink asks in `setup()` to learn how many tracks it owes. WIRE
    /// precedes `setup()`, so by the time it asks, every link is there.
    ///
    /// A port with no link at all answers with none rather than refusing: an
    /// unconnected input is a legal graph, and a sink that refuses one should
    /// do so in its own words.
    #[test]
    fn a_port_lists_the_inbound_links_wired_into_it_and_a_port_with_none_lists_none() {
        let (_publisher_a, sub_a) = open_channel_for_one_link("listing/a", 1);
        let (_publisher_b, sub_b) = open_channel_for_one_link("listing/b", 1);
        let (_publisher_c, sub_c) = open_channel_for_one_link("listing/c", 1);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("tracks", 8, ReadMode::ReadNextInOrder);
        mailboxes.add_port("control", 8, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "tracks",
            "L-camera",
            &InboundLinkName::from("pcamera/video_out"),
            sub_a,
        );
        mailboxes.add_channel_subscriber(
            "tracks",
            "L-microphone",
            &InboundLinkName::from("pmicrophone/audio_out"),
            sub_b,
        );
        mailboxes.add_channel_subscriber(
            "control",
            "L-operator",
            &InboundLinkName::from("poperator/commands"),
            sub_c,
        );

        assert_eq!(
            mailboxes
                .inbound_link_names("tracks")
                .iter()
                .map(InboundLinkName::as_str)
                .collect::<Vec<_>>(),
            vec!["pcamera/video_out", "pmicrophone/audio_out"],
            "a port lists its own links in wiring order, and no other port's",
        );
        assert_eq!(
            mailboxes
                .inbound_link_names("control")
                .iter()
                .map(InboundLinkName::as_str)
                .collect::<Vec<_>>(),
            vec!["poperator/commands"],
        );
        assert!(
            mailboxes.inbound_link_names("unconnected").is_empty(),
            "a port nothing feeds lists none rather than refusing",
        );
    }

    /// A window is cut from bags rather than being one, so no queued entry
    /// carries its name — but a windowed port takes exactly one link, so the
    /// port answers for it and the read works there too.
    #[test]
    fn a_windowed_ports_read_names_the_one_link_that_feeds_it() {
        let (publisher, subscriber) = open_channel_for_one_link("naming/windowed", 8);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_windowed_port(
            "in",
            ReadMode::ReadNextInOrder,
            a_512_512_contract_at(16_000, 1),
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-only",
            &InboundLinkName::from("pmicrophone/audio_out"),
            subscriber,
        );

        for block in 0..4u64 {
            publish_one_mono_channels_block(
                &publisher,
                160,
                16_000,
                (block * 160 * 1_000_000_000 / 16_000) as i64,
            );
        }

        let (_window, inbound_link_name) = mailboxes
            .read_raw_from_inbound_link("in")
            .expect("640 samples completes a 512 window")
            .expect("a full window reads out");
        assert_eq!(
            inbound_link_name.as_str(),
            "pmicrophone/audio_out",
            "a window names the one link its bags arrived on",
        );
    }

    /// A read that took a windowed port's stage before the port was unwired and
    /// wired again feeds that stage nothing from the new wiring, so the new link
    /// is never charged for a flush of the old wiring's samples and its bags stay
    /// queued for the stage installed with it.
    ///
    /// Fail-without-fix: the read pops the rewired port's late block into the
    /// stage it took, flushes the 300 samples the old wiring left there, counts
    /// them on the replacement link, and the replacement's own stage never sees
    /// that block.
    #[test]
    fn a_read_holding_a_stage_from_before_a_rewire_feeds_it_nothing_from_the_new_wiring() {
        let (first_publisher, first_subscriber) = open_channel_for_one_link("rewire/first", 8);
        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_windowed_port(
            "in",
            ReadMode::ReadNextInOrder,
            a_512_512_contract_at(16_000, 1),
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-first",
            &InboundLinkName::from("pfirst/audio_out"),
            first_subscriber,
        );
        publish_one_mono_channels_block(&first_publisher, 300, 16_000, 0);
        assert!(
            mailboxes
                .read_raw("in")
                .expect("the read succeeds")
                .is_none(),
            "300 of 512 samples is not a window"
        );
        let stage_the_read_took = match &mailboxes.ports.lock()["in"].audio_windowing {
            InstalledInputPortAudioWindowing::Windowed(stage) => Arc::clone(stage),
            _ => panic!("the port windows"),
        };

        mailboxes.remove_channel_link("L-first");
        let (second_publisher, second_subscriber) = open_channel_for_one_link("rewire/second", 8);
        mailboxes.add_windowed_port(
            "in",
            ReadMode::ReadNextInOrder,
            a_512_512_contract_at(16_000, 1),
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-second",
            &InboundLinkName::from("psecond/audio_out"),
            second_subscriber,
        );
        let a_second_later_ns = 1_000_000_000;
        publish_one_mono_channels_block(&second_publisher, 300, 16_000, a_second_later_ns);
        mailboxes.receive_pending();

        assert!(
            mailboxes
                .next_window_out_of_the_stage("in", &stage_the_read_took)
                .expect("the read succeeds")
                .is_none(),
            "the stage the read took is no longer the port's"
        );
        let discarded_samples_by_link = || {
            mailboxes
                .discarded_sample_counts_by_inbound_link()
                .discarded_sample_count_snapshot_by_inbound_link()
        };
        assert_eq!(
            discarded_samples_by_link(),
            std::collections::BTreeMap::from([("L-second".to_string(), 0)]),
        );

        publish_one_mono_channels_block(
            &second_publisher,
            300,
            16_000,
            a_second_later_ns + 18_750_000,
        );
        assert!(
            mailboxes
                .read_raw("in")
                .expect("the read succeeds")
                .is_some(),
            "both of the new wiring's blocks reach its own stage and make a window"
        );
        assert_eq!(
            discarded_samples_by_link(),
            std::collections::BTreeMap::from([("L-second".to_string(), 0)]),
        );
    }

    // =========================================================================
    // The audio window contract at the read seam, through real iceoryx2
    // services. These need /dev/shm and nothing else — no GPU, no audio device.
    // =========================================================================

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

    /// A 48 kHz source into a 16 kHz port takes the resampling arm, where the
    /// readiness floor has to know the source rate to give back the right
    /// priming — and the only place it can learn one before consuming a bag is
    /// the mailbox's own measure.
    ///
    /// The identity arm cannot catch a measure that reports no rate: with the
    /// rates equal there is no filter and no priming to give back.
    #[test]
    fn a_resampling_windowed_port_reports_data_only_once_a_full_window_can_be_emitted() {
        let (publisher, subscriber) =
            open_channel_for_one_link_loaning("window/resampled", 32, 16_384);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_windowed_port(
            "in",
            ReadMode::ReadNextInOrder,
            a_512_512_contract_at(16_000, 1),
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-only",
            &InboundLinkName::from("ponly/out"),
            subscriber,
        );

        // 160-frame quanta, so the queue crosses one window's worth in steps
        // smaller than the priming and chunk slack. A floor that cannot see
        // that slack says yes somewhere in that gap and the read then produces
        // nothing — the exact shape the contract exists to rule out, and one a
        // coarser quantum steps straight over.
        const SOURCE_FRAMES_PER_BLOCK: u64 = 160;

        let mut windows = 0;
        for block in 0..24u64 {
            publish_one_mono_channels_block(
                &publisher,
                SOURCE_FRAMES_PER_BLOCK as usize,
                48_000,
                (block * SOURCE_FRAMES_PER_BLOCK * 1_000_000_000 / 48_000) as i64,
            );
            while mailboxes.any_port_has_data() {
                assert!(
                    mailboxes.read_raw("in").expect("reads").is_some(),
                    "the gate promised a window at 48 kHz into a 16 kHz port; the read \
                     must produce one"
                );
                windows += 1;
            }
        }

        assert!(
            windows >= 2,
            "24 blocks of 160 frames at 48 kHz is 1280 frames at 16 kHz, so at least \
             two 512-sample windows; got {windows}"
        );
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

    /// One device stream's format, as a processor's own `setup()` reads it off
    /// the stream it just opened.
    fn a_device_stream_matching(
        sample_rate: u32,
        channels: u32,
        window_and_hop: u32,
    ) -> AudioWindowContractMatchingADeviceStream {
        AudioWindowContractMatchingADeviceStream {
            device_stream_format: crate::core::context::AudioStreamFormat {
                sample_rate,
                channels,
                sample_format: crate::core::context::AudioSampleFormat::F32,
            },
            window_size_in_per_channel_samples: window_and_hop,
            hop_in_per_channel_samples: window_and_hop,
        }
    }

    /// The samples one emitted window carries, interleaved, with its rate and
    /// channel count, read back out of the Channels block the stage wrote.
    fn scalars_of_one_window(window: &MessageReadFromInputPort) -> (Vec<f32>, u32, u32) {
        let block = crate::iceoryx2::audio_window::read_a_channels_block_for_the_window_stage(
            &window.payload_descriptor,
            &window.payload,
            window.timestamp_ns,
        )
        .expect("the stage writes a Channels block");
        (
            block.interleaved_samples_as_f32().collect(),
            block.sample_rate,
            block.channels,
        )
    }

    /// The whole gap the sentinel opens, held at the seam that has to survive
    /// it: the compiler wires this port before its processor reaches `setup()`,
    /// so bags arrive against a contract nobody has settled yet.
    ///
    /// Nothing may reach a reader while that lasts — a raw 16 kHz mono bag
    /// handed to a consumer expecting exact 48 kHz stereo windows is precisely
    /// the plausible-looking wrong audio this contract exists to rule out.
    #[test]
    fn a_port_awaiting_its_device_hands_a_reader_nothing_however_much_is_queued() {
        let (publisher, subscriber) =
            open_channel_for_one_link_loaning("window/awaiting", 8, 16_384);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port_awaiting_its_device_stream_format(
            "in",
            crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
            ReadMode::ReadNextInOrder,
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-only",
            &InboundLinkName::from("ponly/out"),
            subscriber,
        );

        for block in 0..4u64 {
            publish_one_mono_channels_block(&publisher, 480, 16_000, (block * 30_000_000) as i64);
        }

        assert!(
            !mailboxes.has_data("in"),
            "a port with no settled contract has no exact size to cut a window to"
        );
        assert!(!mailboxes.any_port_has_data());
        assert!(
            mailboxes
                .read_raw("in")
                .expect("reading an unsettled port is not an error")
                .is_none(),
            "the raw bag underneath is exactly what the contract exists to keep out of \
             process()"
        );
        assert_eq!(
            mailboxes.input_ports_still_awaiting_their_device_stream_format(),
            vec!["in".to_string()]
        );
    }

    /// An unsettled port's loss, said in words. The count this link carries is
    /// the very same count a stalled consumer produces, and this line is the
    /// only thing that separates the two.
    ///
    /// Mentally revert it and a port that will never settle — one a link was
    /// wired onto after its processor's `setup()` had been and gone — reads
    /// exactly like a consumer that cannot keep up, while it delivers nothing
    /// at all for the rest of the run.
    #[test]
    fn a_bag_evicted_at_a_port_with_an_unsettled_contract_says_so_once_naming_the_port() {
        const MAILBOX_DEPTH: usize = 2;
        const FRAMES_PUBLISHED: usize = 6;

        let (publisher, subscriber) = open_channel_for_one_link("window/awaiting-drop", 4);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port_awaiting_its_device_stream_format(
            "in",
            MAILBOX_DEPTH,
            ReadMode::ReadNextInOrder,
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-only",
            &InboundLinkName::from("ponly/out"),
            subscriber,
        );

        let ((), warnings) = CapturedTracingWarnings::captured_while(|| {
            for _ in 0..FRAMES_PUBLISHED {
                publish_one_frame(&publisher, "mic_out", b"before-the-device-spoke");
                mailboxes.receive_pending();
            }
        });

        assert_eq!(
            mailboxes
                .dropped_bag_counts_by_inbound_link()
                .dropped_bag_count_snapshot_by_inbound_link()
                .get("L-only")
                .copied(),
            Some((FRAMES_PUBLISHED - MAILBOX_DEPTH) as u64),
            "the loss is still counted against the link that lost it"
        );
        let about_the_unsettled_contract: Vec<&String> = warnings
            .iter()
            .filter(|said| said.contains("contract is unsettled"))
            .collect();
        assert_eq!(
            about_the_unsettled_contract.len(),
            1,
            "four bags were evicted and the port diagnoses the run once, not once per \
             bag; got {warnings:?}"
        );
        let said = about_the_unsettled_contract[0];
        assert!(
            said.contains("port=in") && said.contains("match_device"),
            "the record must name the port and the contract that never settled; got {said}"
        );
    }

    /// The notice belongs to the mailbox the sentinel holds, not to the port:
    /// once the contract settles the port is sized from it like any other, and
    /// an overrun there is an ordinary one that must read as one.
    #[test]
    fn a_settled_ports_evictions_are_not_reported_as_an_unsettled_contract() {
        const FRAMES_PUBLISHED: usize = 24;

        let (publisher, subscriber) = open_channel_for_one_link("window/settled-drop", 4);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port_awaiting_its_device_stream_format(
            "in",
            crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
            ReadMode::ReadNextInOrder,
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-only",
            &InboundLinkName::from("ponly/out"),
            subscriber,
        );
        mailboxes
            .settle_a_ports_device_matched_audio_window_contract(
                "in",
                &a_device_stream_matching(48_000, 2, 48),
            )
            .expect("a device format settles the contract");

        let ((), warnings) = CapturedTracingWarnings::captured_while(|| {
            for _ in 0..FRAMES_PUBLISHED {
                publish_one_frame(&publisher, "mic_out", b"after-the-device-spoke");
                mailboxes.receive_pending();
            }
        });

        assert!(
            mailboxes
                .dropped_bag_counts_by_inbound_link()
                .dropped_bag_count_snapshot_by_inbound_link()
                .get("L-only")
                .copied()
                .unwrap_or(0)
                > 0,
            "the run has to overrun the settled depth, or it proves nothing"
        );
        assert!(
            !warnings
                .iter()
                .any(|said| said.contains("contract is unsettled")),
            "a settled port's overrun is an ordinary one; got {warnings:?}"
        );
    }

    /// The rung's flagship case, proven without a device: a mono-preferring
    /// microphone's blocks are already queued when the speaker's `setup()`
    /// settles its port from a stereo-preferring playback stream, and they come
    /// back out converted rather than refused.
    ///
    /// Held against a control rather than against a number: the same bags into
    /// a port that windowed from the start must yield the same windows. That is
    /// what the swap owes — the mailbox it replaces is a different queue, and a
    /// bag dropped in the move would be lost where nothing counts it, because
    /// the link it arrived on is still wired and still delivering. A count
    /// asserted on its own would also pass on two empty runs, so the control
    /// carries a floor.
    #[test]
    fn settling_a_port_converts_the_bags_that_arrived_before_the_device_was_known() {
        /// Publish four 16 kHz mono blocks onto port `"in"` and draw them into
        /// its mailbox, so what follows has something queued to work on.
        fn publish_four_mono_blocks_into(mailboxes: &InputMailboxesInner, tag: &str) {
            let (publisher, subscriber) = open_channel_for_one_link_loaning(tag, 16, 16_384);
            mailboxes.add_channel_subscriber(
                "in",
                "L-only",
                &InboundLinkName::from("ponly/out"),
                subscriber,
            );
            for block in 0..4u64 {
                publish_one_mono_channels_block(
                    &publisher,
                    480,
                    16_000,
                    (block * 30_000_000) as i64,
                );
            }
            mailboxes.receive_pending();
        }

        /// Every window port `"in"` hands back, each held to the device format
        /// its contract settled to.
        fn windows_read_out_of(mailboxes: &InputMailboxesInner) -> Vec<Vec<f32>> {
            let mut windows = Vec::new();
            while let Some(message) = mailboxes.read_raw("in").expect("the port reads") {
                let (scalars, sample_rate, channels) = scalars_of_one_window(&message);
                assert_eq!(sample_rate, 48_000, "the window is at the device's rate");
                assert_eq!(channels, 2, "the window is in the device's channel count");
                assert_eq!(
                    scalars.len(),
                    480 * 2,
                    "a window carries window_size × channels scalars"
                );
                windows.push(scalars);
            }
            windows
        }

        let matching = a_device_stream_matching(48_000, 2, 480);

        let settled_after_the_bags_arrived = InputMailboxesInner::new();
        settled_after_the_bags_arrived.add_port_awaiting_its_device_stream_format(
            "in",
            crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
            ReadMode::ReadNextInOrder,
        );
        publish_four_mono_blocks_into(&settled_after_the_bags_arrived, "window/settled");
        settled_after_the_bags_arrived
            .settle_a_ports_device_matched_audio_window_contract("in", &matching)
            .expect("a playback stream's own format settles the contract");
        assert!(
            settled_after_the_bags_arrived
                .input_ports_still_awaiting_their_device_stream_format()
                .is_empty(),
            "a settled port is no longer awaiting anything"
        );
        let across_the_settle = windows_read_out_of(&settled_after_the_bags_arrived);

        let windowed_from_the_start = InputMailboxesInner::new();
        windowed_from_the_start.add_windowed_port(
            "in",
            ReadMode::ReadNextInOrder,
            ResolvedAudioWindowContract::from_a_device_stream_format(&matching)
                .expect("the same contract, stated up front"),
        );
        publish_four_mono_blocks_into(&windowed_from_the_start, "window/control");
        let never_awaited = windows_read_out_of(&windowed_from_the_start);

        assert!(
            never_awaited.len() >= 8,
            "1 920 mono frames at 16 kHz are 5 760 at 48 kHz, so a port that windowed them \
             all along must hand back most of twelve 480-sample windows; got {}",
            never_awaited.len()
        );
        assert_eq!(
            across_the_settle, never_awaited,
            "settling a port after its bags arrived must lose none of them and change none \
             of their samples — the swap moved the queue, it did not re-cut the audio"
        );
    }

    /// A port with no contract is untouched by any of this, and a settled one
    /// stops being listed the moment it settles — the two answers the
    /// post-`setup()` refusal is built on.
    #[test]
    fn only_a_port_still_holding_the_sentinel_is_listed_as_awaiting_its_device() {
        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("plain", 4, ReadMode::ReadNextInOrder);
        mailboxes.add_windowed_port(
            "declared",
            ReadMode::ReadNextInOrder,
            a_512_512_contract_at(16_000, 1),
        );
        mailboxes.add_port_awaiting_its_device_stream_format(
            "matched",
            crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
            ReadMode::ReadNextInOrder,
        );

        assert_eq!(
            mailboxes.input_ports_still_awaiting_their_device_stream_format(),
            vec!["matched".to_string()],
            "neither a contract-less port nor one stating five values is waiting on a device"
        );

        mailboxes
            .settle_a_ports_device_matched_audio_window_contract(
                "matched",
                &a_device_stream_matching(48_000, 2, 480),
            )
            .expect("the contract settles");

        assert!(
            mailboxes
                .input_ports_still_awaiting_their_device_stream_format()
                .is_empty()
        );
    }

    /// The settle is not a general window setter: only a port that declared the
    /// sentinel and is still waiting on it takes one.
    ///
    /// Mentally revert the guard and a processor can window a port whose author
    /// declared nothing, or replace five declared values with its own device's —
    /// and `graph` would go on rendering the author's declaration while the
    /// stage ran something else, which is the plausible-looking wrong answer
    /// this contract exists to rule out.
    #[test]
    fn only_a_port_awaiting_its_device_can_be_settled_and_the_refusal_says_which_it_is() {
        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("plain", 4, ReadMode::ReadNextInOrder);
        mailboxes.add_windowed_port(
            "declared",
            ReadMode::ReadNextInOrder,
            a_512_512_contract_at(16_000, 1),
        );
        mailboxes.add_port_awaiting_its_device_stream_format(
            "matched",
            crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
            ReadMode::ReadNextInOrder,
        );
        let matching = a_device_stream_matching(48_000, 2, 480);

        for (port, expected) in [
            ("plain", "declares no window contract"),
            ("declared", "already settled"),
        ] {
            let refusal = mailboxes
                .settle_a_ports_device_matched_audio_window_contract(port, &matching)
                .expect_err("only a waiting port is settled")
                .to_string();
            assert!(
                refusal.contains(port) && refusal.contains(expected),
                "the refusal must name the port and what it is; got {refusal}"
            );
        }

        mailboxes
            .settle_a_ports_device_matched_audio_window_contract("matched", &matching)
            .expect("the waiting port settles");
        let settled_twice = mailboxes
            .settle_a_ports_device_matched_audio_window_contract("matched", &matching)
            .expect_err("a settled port is not settled again")
            .to_string();
        assert!(
            settled_twice.contains("already settled"),
            "a second settle must say the contract is already settled; got {settled_twice}"
        );
    }

    /// A link wired after `setup()` already ran — a live-graph edit — finds the
    /// settled values rather than a sentinel, so its port windows from the
    /// first bag instead of waiting for a `setup()` that has been and gone.
    #[test]
    fn a_contract_settled_before_its_port_existed_is_still_there_for_the_wiring() {
        let mailboxes = InputMailboxesInner::new();
        mailboxes
            .settle_a_ports_device_matched_audio_window_contract(
                "in",
                &a_device_stream_matching(44_100, 2, 441),
            )
            .expect("a contract settles whether or not the port is wired yet");

        let settled = mailboxes
            .device_matched_audio_window_contracts()
            .settled_declaration_for_input_port("in")
            .expect("the wiring path finds what setup() settled");
        assert_eq!(settled.sample_rate, 44_100);
        assert_eq!(settled.channels, Some(2));
        assert_eq!(settled.window_size, 441);
        assert_eq!(settled.hop, 441);
    }

    /// A device stream the stage could not honour is refused where it is
    /// settled, naming the port — never accepted and left to fail at the first
    /// read.
    #[test]
    fn a_device_format_the_stage_cannot_honour_is_refused_at_the_settle_naming_the_port() {
        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port_awaiting_its_device_stream_format(
            "in",
            crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
            ReadMode::ReadNextInOrder,
        );

        let refusal = mailboxes
            .settle_a_ports_device_matched_audio_window_contract(
                "in",
                &a_device_stream_matching(48_000, 2, 0),
            )
            .expect_err("a zero window is not a contract")
            .to_string();

        assert!(
            refusal.contains("'in'") && refusal.contains("window_size"),
            "the refusal must name the port and the field; got {refusal}"
        );
        assert_eq!(
            mailboxes.input_ports_still_awaiting_their_device_stream_format(),
            vec!["in".to_string()],
            "a refused settle leaves the port where it was rather than half-settled"
        );
    }

    /// A windowed port reports data only when a full window can be emitted, so
    /// a reactive processor is never dispatched with nothing to read.
    #[test]
    fn a_windowed_port_reports_data_only_once_a_full_window_can_be_emitted() {
        let (publisher, subscriber) = open_channel_for_one_link("window/readiness", 8);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_windowed_port(
            "in",
            ReadMode::ReadNextInOrder,
            a_512_512_contract_at(16_000, 1),
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-only",
            &InboundLinkName::from("ponly/out"),
            subscriber,
        );

        // A third of a window is not a window.
        publish_one_mono_channels_block(&publisher, 160, 16_000, 0);
        assert!(
            !mailboxes.has_data("in"),
            "160 of 512 samples is not a window, and a wake here would dispatch a \
             process() with nothing to read"
        );
        assert!(!mailboxes.any_port_has_data());

        // Enough to complete one.
        for block in 1..4u64 {
            publish_one_mono_channels_block(
                &publisher,
                160,
                16_000,
                (block * 160 * 1_000_000_000 / 16_000) as i64,
            );
        }
        assert!(
            mailboxes.has_data("in"),
            "640 samples completes a 512 window"
        );
        assert!(mailboxes.any_port_has_data());

        let window = mailboxes
            .read_raw("in")
            .expect("a window reads")
            .expect("the gate said a window was ready")
            .payload;
        assert!(!window.is_empty());
    }

    /// One 1024-sample quantum against a 512/512 contract satisfies exactly two
    /// windows — the count the reactive drain loop dispatches `process()`.
    #[test]
    fn one_1024_sample_quantum_reads_out_of_a_512_512_port_exactly_twice() {
        let (publisher, subscriber) =
            open_channel_for_one_link_loaning("window/quantum", 4, 16_384);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_windowed_port(
            "in",
            ReadMode::ReadNextInOrder,
            a_512_512_contract_at(16_000, 1),
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-only",
            &InboundLinkName::from("ponly/out"),
            subscriber,
        );
        publish_one_mono_channels_block(&publisher, 1024, 16_000, 0);

        let mut windows = 0;
        while mailboxes.any_port_has_data() {
            assert!(
                mailboxes.read_raw("in").expect("reads").is_some(),
                "the gate promised a window; the read must produce one"
            );
            windows += 1;
            assert!(
                windows <= 4,
                "a 1024-sample quantum cannot fill more windows"
            );
        }
        assert_eq!(windows, 2);
    }

    /// A port with no contract is unchanged in every respect: the bytes a
    /// reader gets back are the bytes the producer published.
    #[test]
    fn a_contract_less_port_still_reads_the_bag_the_producer_published_byte_for_byte() {
        let (publisher, subscriber) =
            open_channel_for_one_link_loaning("window/untouched", 4, 16_384);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", 8, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "in",
            "L-only",
            &InboundLinkName::from("ponly/out"),
            subscriber,
        );

        let published = TestChannelSample::of_a_mono_channels_block(1024, 16_000, 77);
        publisher.send_numbered(&published);

        assert!(
            mailboxes.has_data("in"),
            "an arrived bag is data on an unwindowed port"
        );
        let read_back = mailboxes
            .read_raw("in")
            .expect("reads")
            .expect("a bag")
            .payload;
        assert_eq!(read_back, published.carried_body);
    }

    /// A loss declared upstream reaches an `audio_window` port's reader on the
    /// window cut from the block that carried it.
    #[test]
    fn a_declared_upstream_loss_reaches_the_window_cut_from_the_block_carrying_it() {
        let channel = crate::iceoryx2::Iceoryx2Node::for_this_test_process()
            .open_or_create_service(&unique_suffix("declared/window"), 2, 16)
            .unwrap();
        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_windowed_port(
            "in",
            ReadMode::ReadNextInOrder,
            a_512_512_contract_at(16_000, 1),
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-declared",
            &InboundLinkName::from("pmic/audio"),
            channel.create_subscriber(16).unwrap(),
        );
        let output_writer = output_writer_publishing_onto(&channel, "audio");
        let write_block_at = |first_sample_timestamp_ns: i64| {
            let block =
                TestChannelSample::of_a_mono_channels_block(512, 16_000, first_sample_timestamp_ns);
            output_writer
                .write_bytes_described_by(
                    "audio",
                    &block.carried_body,
                    first_sample_timestamp_ns,
                    MessageGroupBoundary::StartsGroup,
                    block.payload_descriptor,
                )
                .unwrap();
        };

        write_block_at(0);
        output_writer
            .declare_groups_given_up_upstream_before_the_next_write("audio", 1)
            .unwrap();
        write_block_at(64_000_000);

        let losses_told_per_window: Vec<u64> =
            std::iter::from_fn(|| mailboxes.read_raw("in").unwrap())
                .map(|window| window.lost_since_last_read)
                .collect();
        assert_eq!(losses_told_per_window, [0, 1]);
    }

    /// The accumulator is not a second drop site: bags stay in the counted
    /// mailbox until a read takes one, so a windowed port under overrun counts
    /// exactly what an unwindowed one does.
    #[test]
    fn a_windowed_ports_per_link_drop_counts_match_an_unwindowed_ports_under_the_same_overrun() {
        let contract = a_512_512_contract_at(16_000, 1);
        let depth = contract.windowed_port_mailbox_depth();
        let published_bags = depth + 7;

        fn losses_under_overrun(
            tag: &str,
            depth: usize,
            published_bags: usize,
            contract: Option<ResolvedAudioWindowContract>,
        ) -> u64 {
            let (publisher, subscriber) = open_channel_for_one_link(tag, depth);
            let mailboxes = InputMailboxesInner::new();
            match contract {
                Some(contract) => {
                    mailboxes.add_windowed_port("in", ReadMode::ReadNextInOrder, contract)
                }
                None => mailboxes.add_port("in", depth, ReadMode::ReadNextInOrder),
            }
            mailboxes.add_channel_subscriber(
                "in",
                "L-only",
                &InboundLinkName::from("ponly/out"),
                subscriber,
            );

            for block in 0..published_bags {
                publish_one_mono_channels_block(
                    &publisher,
                    160,
                    16_000,
                    (block as i64) * 10_000_000,
                );
                // One at a time so the subscriber ring never overflows and every
                // loss lands at the mailbox, where it is counted.
                mailboxes.receive_pending();
            }
            mailboxes
                .dropped_bag_counts_by_inbound_link()
                .dropped_bag_count_snapshot_by_inbound_link()
                .values()
                .sum()
        }

        let windowed =
            losses_under_overrun("window/counted", depth, published_bags, Some(contract));
        let unwindowed = losses_under_overrun("window/uncounted", depth, published_bags, None);

        assert_eq!(
            windowed, 7,
            "a mailbox {depth} deep loses the {published_bags} minus {depth}"
        );
        assert_eq!(
            windowed, unwindowed,
            "windowing must not add or hide a loss the counters do not see"
        );
    }

    /// A window wider than the delivery profile's depth sizes its own mailbox,
    /// because `ORDERED_DEPTH` cannot hold a one-second rolling window's quanta.
    #[test]
    fn a_long_windows_mailbox_is_sized_from_its_contract_rather_than_the_profiles_depth() {
        let one_second_rolling = ResolvedAudioWindowContract::from_declared_values(
            &crate::core::descriptors::AudioWindowContractDeclaredValues {
                sample_rate: 16_000,
                channels: Some(1),
                dtype: "f32".to_string(),
                window_size: 16_000,
                hop: 160,
            },
        )
        .expect("a one-second rolling window is legal");

        let (publisher, subscriber) = open_channel_for_one_link("window/depth", 4);
        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_windowed_port("in", ReadMode::ReadNextInOrder, one_second_rolling);
        mailboxes.add_channel_subscriber(
            "in",
            "L-only",
            &InboundLinkName::from("ponly/out"),
            subscriber,
        );

        // Deeper than ORDERED_DEPTH: publishing that many must lose nothing.
        for block in 0..(crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH + 8) {
            publish_one_mono_channels_block(&publisher, 160, 16_000, (block as i64) * 10_000_000);
            mailboxes.receive_pending();
        }

        let lost: u64 = mailboxes
            .dropped_bag_counts_by_inbound_link()
            .dropped_bag_count_snapshot_by_inbound_link()
            .values()
            .sum();
        assert_eq!(
            lost, 0,
            "a one-second window's port must hold more than the profile's 16 blocks"
        );
    }

    /// A bag the stage cannot read is refused by name at the read, naming the
    /// port — never reshaped into a plausible wrong answer.
    #[test]
    fn a_bag_the_stage_cannot_read_is_refused_at_the_read_naming_the_port() {
        let (publisher, subscriber) = open_channel_for_one_link("window/refusal", 4);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_windowed_port(
            "in",
            ReadMode::ReadNextInOrder,
            a_512_512_contract_at(16_000, 1),
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-only",
            &InboundLinkName::from("ponly/out"),
            subscriber,
        );

        let not_an_audio_block =
            rmp_serde::to_vec_named(&std::collections::BTreeMap::from([("width", 1920)]))
                .expect("encodes");
        publish_one_frame(&publisher, "mic_out", &not_an_audio_block);
        mailboxes.receive_pending();

        let refusal = mailboxes
            .read_raw("in")
            .expect_err("a bag with no audio-block keys is refused");
        let rendered = refusal.to_string();
        assert!(
            rendered.contains("'in'") && rendered.contains("audio block"),
            "the refusal must name the port and what it could not read; got {rendered}"
        );
    }

    /// The negative half: a consumer that keeps up loses nothing, and its
    /// wired links say so with a zero rather than by going missing. A counter
    /// that only appears once a link has lost something is indistinguishable
    /// from a link nobody wired.
    #[test]
    fn a_port_that_keeps_up_reports_a_zero_for_every_wired_link() {
        let (publisher_a, subscriber_a) = open_channel_for_one_link("no-drop/a", 4);
        let (publisher_b, subscriber_b) = open_channel_for_one_link("no-drop/b", 4);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", 8, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "in",
            "L-first",
            &InboundLinkName::from("pfirst/out"),
            subscriber_a,
        );
        mailboxes.add_channel_subscriber(
            "in",
            "L-second",
            &InboundLinkName::from("psecond/out"),
            subscriber_b,
        );

        for _ in 0..2 {
            publish_one_frame(&publisher_a, "src_a_out", b"from-a");
            mailboxes.receive_pending();
            publish_one_frame(&publisher_b, "src_b_out", b"from-b");
            mailboxes.receive_pending();
        }

        assert_eq!(
            mailboxes
                .dropped_bag_counts_by_inbound_link()
                .dropped_bag_count_snapshot_by_inbound_link(),
            std::collections::BTreeMap::from([
                ("L-first".to_string(), 0),
                ("L-second".to_string(), 0),
            ]),
        );
        assert_eq!(mailboxes.drain("in").len(), 4);
    }

    /// A disconnected link's count leaves with it: `graph` renders counts
    /// beside links it still has, and a count naming a link the graph dropped
    /// is a reader's dead end.
    #[test]
    fn a_disconnected_links_count_goes_with_the_link() {
        let (publisher, subscriber) = open_channel_for_one_link("reclaim-count", 4);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", 1, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "in",
            "L-departing",
            &InboundLinkName::from("pdeparting/out"),
            subscriber,
        );
        for _ in 0..3 {
            publish_one_frame(&publisher, "src_out", b"body");
            mailboxes.receive_pending();
        }
        assert_eq!(
            mailboxes
                .dropped_bag_counts_by_inbound_link()
                .dropped_bag_count_snapshot_by_inbound_link()["L-departing"],
            2,
        );

        mailboxes.remove_channel_link("L-departing");

        assert!(
            mailboxes
                .dropped_bag_counts_by_inbound_link()
                .dropped_bag_count_snapshot_by_inbound_link()
                .is_empty(),
        );
    }

    /// Per-link destination reclaim: a destination fanning two inbound
    /// links into ONE local port holds two tagged subscribers plus one shared
    /// listener. Disconnecting one link drops only its subscriber (the port
    /// mailbox and listener survive so the other link keeps delivering);
    /// disconnecting the last link removes the port mailbox AND drops the shared
    /// listener — releasing the notify service so a reconnect recreates it fresh.
    ///
    /// Fail-without-fix: make `remove_channel_link` a no-op and the final
    /// disconnect leaves the port and listener live, so `has_port` /
    /// `has_listener` stay true and the release assertions fail.
    #[test]
    fn remove_channel_link_reclaims_per_link_then_drops_port_and_listener() {
        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();

        let open_subscriber = |tag: &str| {
            crate::iceoryx2::Iceoryx2Node::for_this_test_process()
                .open_or_create_service(&unique_suffix(tag), 2, 4)
                .unwrap()
                .create_subscriber(4)
                .unwrap()
        };
        let listener =
            crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(
                &node
                    .service_builder(&ServiceName::new(&unique_suffix("reclaim/notify")).unwrap())
                    .event()
                    .max_notifiers(2)
                    .max_listeners(1)
                    .open_or_create()
                    .unwrap(),
            )
            .unwrap();

        let inner = InputMailboxesInner::new();
        inner.add_port("in", 64, ReadMode::ReadNextInOrder);
        inner.add_channel_subscriber(
            "in",
            "L-link-a",
            &InboundLinkName::from("plink-a/out"),
            open_subscriber("reclaim/a"),
        );
        inner.add_channel_subscriber(
            "in",
            "L-link-b",
            &InboundLinkName::from("plink-b/out"),
            open_subscriber("reclaim/b"),
        );
        inner.set_listener(listener);
        assert!(inner.has_port("in"));
        assert!(inner.has_listener());

        // Disconnect one of two links into the shared port: port + listener stay.
        inner.remove_channel_link("L-link-a");
        assert!(
            inner.has_port("in"),
            "the local port must stay while link-b still feeds it",
        );
        assert!(
            inner.has_listener(),
            "the shared listener must stay while any inbound link remains",
        );

        // Unknown link id is a no-op.
        inner.remove_channel_link("L-does-not-exist");
        assert!(inner.has_port("in"));

        // Disconnect the last link: port mailbox removed, listener released.
        inner.remove_channel_link("L-link-b");
        assert!(
            !inner.has_port("in"),
            "the port mailbox must be reclaimed once its last subscriber is gone",
        );
        assert!(
            !inner.has_listener(),
            "the destination's listener (and its notify service) must be released \
             after the last inbound link disconnects so a reconnect recreates it",
        );
    }

    /// Empty (unwired) mailboxes should return Ok(None) from read_raw
    /// rather than crash. Mentally revert the is_configured guard
    /// and the test panics dereferencing a null handle.
    #[test]
    fn empty_mailboxes_returns_none_cleanly() {
        let mb = InputMailboxes::empty();
        assert!(!mb.is_configured());
        assert!(mb.read_raw("any").unwrap().is_none());
        assert!(!mb.has_data("any"));
    }

    /// The payload length is the last wire-derived number the read path trusts,
    /// and a frame claiming more than it carries has no safe default — it is
    /// unusable, so it must surface as a typed error naming the port and both
    /// numbers rather than slicing past the frame.
    ///
    /// Fail-without-fix: drop the bound and the read slices `[..4096]` out of
    /// a 64-byte sample.
    #[test]
    fn a_read_rejects_a_frame_stamping_more_payload_than_it_carries() {
        const STAMPED: u32 = 4096;
        const CARRIED: usize = 64;

        let inner = InputMailboxesInner::new();
        inner.add_port("in", 64, ReadMode::ReadNextInOrder);
        let link = a_test_link_into(&inner, "in");

        link.send_numbered(&TestChannelSample::stamping(42, STAMPED, &[0u8; CARRIED]));

        let err = match inner.read_raw("in") {
            Err(e) => e,
            Ok(_) => panic!("a frame stamping more than it carries must not read"),
        };
        assert!(
            matches!(
                &err,
                Error::ChannelSampleStampsAPayloadPastTheSample {
                    port,
                    stamped_payload_bytes,
                    available_payload_bytes,
                } if port == "in"
                    && *stamped_payload_bytes == STAMPED as usize
                    && *available_payload_bytes == CARRIED
            ),
            "expected the typed length error naming the port and both numbers, got {err:?}"
        );
        // Diagnosable without a debugger: the rendered message carries all three.
        let rendered = err.to_string();
        for expected in ["'in'", "4096", "64"] {
            assert!(
                rendered.contains(expected),
                "message must name {expected}: {rendered}"
            );
        }

        // The malformed frame is dropped — the port keeps serving.
        let body = [1u8, 2, 3, 4];
        link.send_numbered(&TestChannelSample::carrying(43, &body));

        let MessageReadFromInputPort {
            payload: data,
            timestamp_ns,
            ..
        } = inner
            .read_raw("in")
            .expect("read")
            .expect("the well-formed frame delivers");
        assert_eq!(data, body, "a well-formed frame still delivers intact");
        assert_eq!(timestamp_ns, 43);
    }

    /// A frame may carry more bytes than its header stamps — the wire
    /// contract trusts the stamped length, so the read delivers exactly that
    /// many payload bytes and the slack past them never reaches the caller.
    ///
    /// Fail-without-fix: hand the caller the frame's whole tail instead of
    /// slicing (or truncating) to the stamped length and the delivered
    /// payload grows by the slack bytes — the exact-length assertion fails.
    #[test]
    fn a_read_delivers_exactly_the_stamped_payload_from_an_over_carrying_frame() {
        const STAMPED: usize = 200;
        const CARRIED: usize = 300;

        let inner = InputMailboxesInner::new();
        inner.add_port("in", 8, ReadMode::ReadNextInOrder);
        let link = a_test_link_into(&inner, "in");

        let body: Vec<u8> = (0..CARRIED as u32).map(|i| (i % 251) as u8).collect();
        link.send_numbered(&TestChannelSample::stamping(77, STAMPED as u32, &body));

        let MessageReadFromInputPort {
            payload: data,
            timestamp_ns,
            ..
        } = inner
            .read_raw("in")
            .expect("read")
            .expect("the over-carrying frame delivers its stamped payload");
        assert_eq!(
            data.len(),
            STAMPED,
            "payload must stop at the stamped length"
        );
        assert_eq!(
            data[..],
            body[..STAMPED],
            "payload must be the stamped prefix"
        );
        assert_eq!(timestamp_ns, 77);

        // The slack was dropped with the frame — the port is empty.
        assert!(inner.read_raw("in").expect("read").is_none());
    }

    /// Clone bumps the strong count via the host-installed
    /// refcount fn; both clones drop independently.
    #[test]
    fn clone_balances_drop() {
        let inner = Arc::new(InputMailboxesInner::new());
        let inner_for_test = inner.clone();
        let mb1 = InputMailboxes::from_inner_arc(inner);
        assert_eq!(Arc::strong_count(&inner_for_test), 2);
        let mb2 = mb1.clone();
        assert_eq!(Arc::strong_count(&inner_for_test), 3);
        drop(mb2);
        assert_eq!(Arc::strong_count(&inner_for_test), 2);
        drop(mb1);
        assert_eq!(Arc::strong_count(&inner_for_test), 1);
    }

    /// A live connect and disconnect racing a read on the same destination, the
    /// shape of a native processor reading on its own thread, or of a helper's
    /// reads detached from the GIL, while `connect` / `disconnect` re-splice it.
    ///
    /// Nothing outside the mailboxes serializes the two threads here, as
    /// nothing does in either of those processors. Without a lock of the
    /// mailboxes' own, the subscriber list reallocates under an iterating
    /// receive and iceoryx2's single-consumer queue gets two callers: memory is
    /// corrupted and the test binary aborts, usually well inside the window.
    /// The two racers end together, past the window only until both floors are
    /// met, so a starved runner races longer and a dead racer fails the test.
    #[test]
    fn a_link_wired_and_unwired_in_a_loop_never_races_a_read_that_holds_no_processor_mutex() {
        const RACE_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);
        const RACE_HARD_CAP: std::time::Duration = std::time::Duration::from_secs(30);
        const WIRE_AND_UNWIRE_CYCLES_FLOOR: u64 = 100;
        const READ_PASSES_FLOOR: u64 = 100;
        const MAX_SUBSCRIBERS: usize = 2;
        const MAX_QUEUED_MESSAGES: usize = 16;
        const MAX_NOTIFIERS: usize = 1;
        const EXPECTED_PAYLOAD_BYTES: usize = 64;
        const RACERS: usize = 2;

        /// Counts a racer out when it ends, by returning or by panicking, so the
        /// publisher it feeds never outlives it.
        struct RacerStillRunning<'a>(&'a std::sync::atomic::AtomicUsize);
        impl Drop for RacerStillRunning<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
        }

        let node = crate::iceoryx2::Iceoryx2Node::for_this_test_process();
        let channel_service_name = unique_suffix("live-rewire-race/channel");
        let notify_service_name = unique_suffix("live-rewire-race/notify");
        let inbound_link_name = InboundLinkName::from(channel_service_name.as_str());
        let open_the_rewired_channel = || {
            node.open_or_create_service(&channel_service_name, MAX_SUBSCRIBERS, MAX_QUEUED_MESSAGES)
                .unwrap()
        };
        let mailboxes = InputMailboxesInner::new();
        let racers_still_running = std::sync::atomic::AtomicUsize::new(RACERS);
        let wiring_floor_met = std::sync::atomic::AtomicBool::new(false);
        let reading_floor_met = std::sync::atomic::AtomicBool::new(false);
        let race_started = std::time::Instant::now();
        let still_racing = || {
            let elapsed = race_started.elapsed();
            let both_floors_met = wiring_floor_met.load(std::sync::atomic::Ordering::Relaxed)
                && reading_floor_met.load(std::sync::atomic::Ordering::Relaxed);
            elapsed < RACE_HARD_CAP
                && racers_still_running.load(std::sync::atomic::Ordering::Relaxed) == RACERS
                && (elapsed < RACE_WINDOW || !both_floors_met)
        };

        let (wire_and_unwire_cycles, read_passes, frames_read) = std::thread::scope(|scope| {
            scope.spawn(|| {
                let publisher = TestChannelPublisherNumberingItsSends::numbering(
                    open_the_rewired_channel()
                        .create_publisher(EXPECTED_PAYLOAD_BYTES)
                        .unwrap(),
                );
                while racers_still_running.load(std::sync::atomic::Ordering::Relaxed) > 0
                    && race_started.elapsed() < RACE_HARD_CAP
                {
                    publish_one_frame(&publisher, "out", b"a-bag-published-mid-rewire");
                }
            });

            let wiring = scope.spawn(|| {
                let _still_running = RacerStillRunning(&racers_still_running);
                let mut wire_and_unwire_cycles = 0u64;
                while still_racing() {
                    if !mailboxes.has_port("in") {
                        mailboxes.add_port("in", MAX_QUEUED_MESSAGES, ReadMode::ReadNextInOrder);
                    }
                    mailboxes.add_channel_subscriber(
                        "in",
                        "L-rewired",
                        &inbound_link_name,
                        open_the_rewired_channel()
                            .create_subscriber(MAX_QUEUED_MESSAGES)
                            .unwrap(),
                    );
                    if !mailboxes.has_listener() {
                        mailboxes.set_listener(
                            node.open_or_create_notify_service(&notify_service_name, MAX_NOTIFIERS)
                                .unwrap()
                                .create_listener()
                                .unwrap(),
                        );
                    }
                    mailboxes.remove_channel_link("L-rewired");
                    wire_and_unwire_cycles += 1;
                    if wire_and_unwire_cycles >= WIRE_AND_UNWIRE_CYCLES_FLOOR {
                        wiring_floor_met.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                wire_and_unwire_cycles
            });

            let reading = scope.spawn(|| {
                let _still_running = RacerStillRunning(&racers_still_running);
                let mut read_passes = 0u64;
                let mut frames_read = 0u64;
                while still_racing() {
                    mailboxes.drain_listener();
                    let _ = mailboxes.listener_fd();
                    let _ = mailboxes.inbound_link_names("in");
                    if mailboxes.has_data("in") {
                        while let Ok(Some(_)) = mailboxes.read_raw("in") {
                            frames_read += 1;
                        }
                    }
                    let _ = mailboxes.any_port_has_data();
                    read_passes += 1;
                    if read_passes >= READ_PASSES_FLOOR && frames_read > 0 {
                        reading_floor_met.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                (read_passes, frames_read)
            });

            let wire_and_unwire_cycles = wiring.join().unwrap();
            let (read_passes, frames_read) = reading.join().unwrap();
            (wire_and_unwire_cycles, read_passes, frames_read)
        });

        assert!(
            wire_and_unwire_cycles >= WIRE_AND_UNWIRE_CYCLES_FLOOR,
            "the wiring thread must re-splice the link many times before the hard cap, \
             got {wire_and_unwire_cycles}"
        );
        assert!(
            read_passes >= READ_PASSES_FLOOR,
            "the reading thread must read many times before the hard cap, got {read_passes}"
        );
        assert!(
            frames_read > 0,
            "reads must have interleaved with a wired link, but none returned a bag \
             across {read_passes} passes and {wire_and_unwire_cycles} re-splices"
        );
    }
}
