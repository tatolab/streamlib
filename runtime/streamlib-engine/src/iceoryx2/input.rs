// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Input mailboxes for receiving frames from upstream processors.
//!
//! # Two-type split: handle vs. inner
//!
//! - [`InputMailboxesInner`] holds the actual state — the
//!   `HashMap<port, PortConfig>` of per-port mailboxes plus the
//!   thread-local `Subscriber` and `Listener` wrappers. All
//!   per-frame `receive_pending` + mailbox push/pop work runs here.
//! - [`InputMailboxes`] is the public handle that processor structs
//!   hold via the macro-emitted `inputs: InputMailboxes` field. It
//!   wraps an `Arc<InputMailboxesInner>` behind an opaque handle;
//!   `process()` reaches input data through `read` / `read_raw` /
//!   `has_data`, which borrow the inner and invoke it directly.
//!
//! Host-side wiring code that mutates the inner (`add_port`,
//! `add_channel_subscriber`, `set_listener`, `listener_fd`,
//! `drain_listener`, etc.) operates on `Arc<InputMailboxesInner>`
//! directly via the methods declared on the inner type.

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;

use iceoryx2::port::listener::Listener;
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::*;
use serde::de::DeserializeOwned;

use super::audio_window::{
    AudioWindowAccumulator, AudioWindowContractMatchingADeviceStream,
    DeviceMatchedAudioWindowContractsByInputPort, LatestQueuedSourceAudioFormat,
    ResolvedAudioWindowContract, queued_audio_window_frame_measure,
};
use super::channel_name::InboundLinkName;
use super::dropped_bag_counters::{DroppedBagCountsByInboundLink, InboundLinkDroppedBagCounter};
use super::mailbox::{PortMailbox, PortMailboxEvictionNotice};
use super::read_mode::ReadMode;
use super::{FRAME_HEADER_SIZE, FrameHeader};
use crate::core::error::{Error, Result};

/// One windowed port's stage, shared out of the `ports` map so the resample,
/// mixdown and framing work runs with that mutex released.
type SharedAudioWindowStage = Arc<parking_lot::Mutex<AudioWindowAccumulator>>;

/// One bag body ready to hand a reader, and the instant it is stamped with.
///
/// The two stamps are not the same thing and the type is what keeps them
/// straight: on a port with no contract it is the frame header's, which marks
/// when the bag was published; on a windowed port it is the one the stage
/// derived from the device's own stamp, which marks an instant inside the
/// source block the window came from.
pub(super) struct BagBodyForTheReader {
    pub(super) body: Vec<u8>,
    pub(super) first_sample_or_publish_timestamp_ns: i64,
    /// The link this body came in on. `None` where nothing delivered it — a
    /// manually injected frame — and filled in for a window by the port that
    /// staged it, since a window is cut from bags rather than being one.
    pub(super) inbound_link_name: Option<InboundLinkName>,
}

/// Decode one bag body into the type a reader named.
fn deserialize_bag_body<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
    rmp_serde::from_slice(body)
        .map_err(|failure| Error::Link(format!("Failed to deserialize frame: {failure}")))
}

/// The one spelling of "this port was never configured".
fn unknown_input_port(port: &str) -> Error {
    Error::Link(format!("Unknown input port: {port}"))
}

/// One channel subscriber bound to the local input port it feeds.
///
/// The transport inversion (#1419): a channel is keyed on its source output
/// port, so a destination consuming N inbound channels holds N subscribers.
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
    /// The source channel name this subscriber subscribed to — the name a read
    /// hands back for every frame it delivers, and the name `graph` and `tap`
    /// show for the same link.
    inbound_link_name: InboundLinkName,
    subscriber: Subscriber<ipc::Service, [u8], ()>,
    /// This link's share of the destination's dropped-bag counts. Every frame
    /// this subscriber delivers is queued holding it, so an eviction names the
    /// link the evicted bag came in on rather than the one that made room.
    dropped_bag_counter: InboundLinkDroppedBagCounter,
}

/// Thread-local set of channel subscribers.
///
/// # Safety
/// Safe to send between threads because:
/// 1. Subscribers are only ever pushed AFTER the processor is spawned on its
///    execution thread (during wiring).
/// 2. Once pushed, each subscriber is only accessed from that same thread.
/// 3. The set starts empty (safe to send) and is populated on the target thread.
struct SendableChannelSubscribers(UnsafeCell<Vec<PortBoundSubscriber>>);

// SAFETY: subscribers are only accessed from a single thread after being pushed;
// see the numbered discipline above.
unsafe impl Send for SendableChannelSubscribers {}
unsafe impl Sync for SendableChannelSubscribers {}

impl SendableChannelSubscribers {
    fn new() -> Self {
        Self(UnsafeCell::new(Vec::new()))
    }

    fn push(
        &self,
        link_id: String,
        local_port: String,
        inbound_link_name: InboundLinkName,
        subscriber: Subscriber<ipc::Service, [u8], ()>,
        dropped_bag_counter: InboundLinkDroppedBagCounter,
    ) {
        // SAFETY: Only called from the processor's execution thread during wiring.
        unsafe {
            (*self.0.get()).push(PortBoundSubscriber {
                link_id,
                local_port,
                inbound_link_name,
                subscriber,
                dropped_bag_counter,
            });
        }
    }

    /// Remove the subscriber serving `link_id`, returning the local input port it
    /// was bound to (so the caller can decide whether that port's mailbox is now
    /// orphaned). `None` if no subscriber matches — a no-op.
    fn remove_by_link(&self, link_id: &str) -> Option<String> {
        // SAFETY: sound because every caller (exec thread and compiler thread)
        // holds the owning ProcessorInstance mutex; never call without that lock.
        unsafe {
            let subscribers = &mut *self.0.get();
            let position = subscribers.iter().position(|b| b.link_id == link_id)?;
            Some(subscribers.remove(position).local_port)
        }
    }

    /// Whether any remaining subscriber is still bound to `local_port`.
    fn port_still_bound(&self, local_port: &str) -> bool {
        // SAFETY: sound because every caller (exec thread and compiler thread)
        // holds the owning ProcessorInstance mutex; never call without that lock.
        unsafe { (*self.0.get()).iter().any(|b| b.local_port == local_port) }
    }

    fn as_slice(&self) -> &[PortBoundSubscriber] {
        // SAFETY: Only called from the processor's execution thread.
        unsafe { &*self.0.get() }
    }

    /// The bindings feeding one local input port, in wiring order — a
    /// destination fanning in N links holds N of them on that one port.
    fn bound_to_local_port<'a>(
        &'a self,
        local_port: &'a str,
    ) -> impl Iterator<Item = &'a PortBoundSubscriber> {
        self.as_slice()
            .iter()
            .filter(move |bound| bound.local_port == local_port)
    }

    fn is_empty(&self) -> bool {
        // SAFETY: Only called from the processor's execution thread.
        unsafe { (*self.0.get()).is_empty() }
    }
}

/// Thread-local listener wrapper. Mirrors [`SendableSubscriber`] — the
/// [`Listener`] is set once on the processor's execution thread and accessed
/// only from that thread thereafter.
struct SendableListener(UnsafeCell<Option<Listener<ipc::Service>>>);

// SAFETY: same single-thread-after-set discipline as SendableSubscriber.
unsafe impl Send for SendableListener {}
unsafe impl Sync for SendableListener {}

impl SendableListener {
    fn new() -> Self {
        Self(UnsafeCell::new(None))
    }

    fn set(&self, listener: Listener<ipc::Service>) {
        // SAFETY: Only called from the processor's execution thread after spawn
        unsafe {
            *self.0.get() = Some(listener);
        }
    }

    fn get(&self) -> Option<&Listener<ipc::Service>> {
        // SAFETY: Only called from the processor's execution thread
        unsafe { (*self.0.get()).as_ref() }
    }

    /// Drop the listener, releasing the destination-keyed notify service's
    /// listener slot. Called when a destination's last inbound link disconnects
    /// so a reconnect recreates the notify service fresh.
    fn clear(&self) {
        // SAFETY: sound because every caller (exec thread and compiler thread)
        // holds the owning ProcessorInstance mutex; never call without that lock.
        unsafe {
            *self.0.get() = None;
        }
    }
}

/// Outcome of a bounded read for the grow-and-retry read protocol.
///
/// A publisher under PowerOfTwo growth can deliver a frame larger than any fixed
/// receive buffer; [`InputMailboxesInner::read_raw_bounded`] reports that as
/// [`BoundedReadOutcome::NeedsLargerBuffer`] (the frame is stashed, not dropped)
/// so the caller resizes and retries.
pub enum BoundedReadOutcome {
    /// The port's mailbox was empty.
    Empty,
    /// A frame fit the caller's buffer and is being returned.
    Frame {
        /// The frame's serialized body (header stripped).
        data: Vec<u8>,
        /// The frame's monotonic timestamp.
        timestamp_ns: i64,
        /// The inbound link it arrived on; `None` for a manually injected
        /// frame, which no link delivered.
        inbound_link_name: Option<InboundLinkName>,
    },
    /// The next frame is `required_bytes` long — larger than the caller's
    /// buffer. The caller must resize to at least this many bytes and read
    /// again; the frame is held for that retry.
    NeedsLargerBuffer {
        /// Byte length the caller's next buffer must reach.
        required_bytes: usize,
    },
}

/// What one port would hand a reader, read off its state under the `ports`
/// lock and judged with that lock released.
enum PortReadiness {
    /// A frame is queued, or one was staged by a bounded read that could not
    /// fit it — either way the next read returns something.
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
        if port_config.staged_oversized.is_some() {
            return PortReadiness::AFrameIsWaiting;
        }
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

/// Per-port configuration: mailbox and read mode.
///
/// Interior mutability: the host-side wiring path discovers
/// per-port configuration (read_mode, buffer_size) at the moment
/// the first downstream `connect` op runs and may need to
/// add ports after the inner is already shared as `Arc`. We use
/// `parking_lot::Mutex<HashMap>` for `ports` rather than threading
/// `&mut self` through `Arc<...>`.
struct PortConfig {
    mailbox: PortMailbox,
    read_mode: ReadMode,
    /// A frame popped by [`InputMailboxesInner::read_raw_bounded`] that did not
    /// fit the caller's buffer. It is stashed here (not lost) and re-delivered
    /// on the next call once the caller resizes — the grow-and-retry contract
    /// that lets a PowerOfTwo-grown oversized payload reach the reader without
    /// dropping it.
    staged_oversized: Option<BagBodyForTheReader>,
    /// How this port windows what arrives on it.
    audio_windowing: InstalledInputPortAudioWindowing,
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
        mailbox: PortMailbox::new(contract.windowed_port_mailbox_depth())
            .measuring_every_queued_frame_with(queued_audio_window_frame_measure(
                contract,
                Arc::clone(&latest_queued_source_audio_format),
            )),
        read_mode,
        staged_oversized: None,
        audio_windowing: InstalledInputPortAudioWindowing::Windowed(Arc::new(
            parking_lot::Mutex::new(AudioWindowAccumulator::new(
                port,
                contract,
                latest_queued_source_audio_format,
            )),
        )),
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
/// mailbox map plus the per-thread subscriber + listener. All
/// per-frame `receive_pending` + queue-pop work runs here.
///
/// Held via `Arc<InputMailboxesInner>`; the [`InputMailboxes`] handle
/// stores a separate `Arc::into_raw`-encoded strong reference to the
/// same inner.
pub struct InputMailboxesInner {
    ports: parking_lot::Mutex<HashMap<String, PortConfig>>,
    subscribers: SendableChannelSubscribers,
    listener: SendableListener,
    dropped_bag_counts: Arc<DroppedBagCountsByInboundLink>,
    device_matched_audio_window_contracts: Arc<DeviceMatchedAudioWindowContractsByInputPort>,
}

impl InputMailboxesInner {
    /// Create a new empty inner.
    pub fn new() -> Self {
        Self {
            ports: parking_lot::Mutex::new(HashMap::new()),
            subscribers: SendableChannelSubscribers::new(),
            listener: SendableListener::new(),
            dropped_bag_counts: Arc::new(DroppedBagCountsByInboundLink::default()),
            device_matched_audio_window_contracts: Arc::new(
                DeviceMatchedAudioWindowContractsByInputPort::default(),
            ),
        }
    }

    /// Check if a port has already been configured.
    pub fn has_port(&self, port: &str) -> bool {
        self.ports.lock().contains_key(port)
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
                mailbox: PortMailbox::new(buffer_size),
                read_mode,
                staged_oversized: None,
                audio_windowing: InstalledInputPortAudioWindowing::NotWindowed,
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
                mailbox: PortMailbox::new(depth).reporting_every_eviction_to(
                    notice_that_a_bag_was_lost_at_a_port_whose_match_device_contract_is_unsettled(
                        port, depth,
                    ),
                ),
                read_mode,
                staged_oversized: None,
                audio_windowing: InstalledInputPortAudioWindowing::AwaitingItsDeviceStreamFormat,
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
            let settled = windowed_port_config(port, existing.read_mode, contract);
            // Anything that arrived while the contract was unsettled moves into
            // the mailbox the contract sized, rather than being dropped where
            // no counter would see it. The staged frame moves with them: a
            // waiting port hands a reader nothing, so it is always `None` here,
            // and carrying it says so locally instead of leaving the next
            // reader to re-derive it from the read path.
            existing
                .mailbox
                .hand_every_queued_frame_over_to(&settled.mailbox);
            let staged_before_the_settle = existing.staged_oversized.take();
            *existing = PortConfig {
                staged_oversized: staged_before_the_settle,
                ..settled
            };
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
        !self.subscribers.is_empty()
    }

    /// Bind an iceoryx2 channel Subscriber to the local input port it feeds.
    ///
    /// One call per inbound `connect()` link — a destination consuming N
    /// channels holds N subscribers. The receive path routes every frame a
    /// subscriber delivers into `local_port`'s mailbox (binding-based routing;
    /// see [`PortBoundSubscriber`]).
    ///
    /// Note: This should only be called from the processor's execution thread.
    pub fn add_channel_subscriber(
        &self,
        local_port: &str,
        link_id: &str,
        inbound_link_name: &InboundLinkName,
        subscriber: Subscriber<ipc::Service, [u8], ()>,
    ) {
        self.subscribers.push(
            link_id.to_string(),
            local_port.to_string(),
            inbound_link_name.clone(),
            subscriber,
            self.dropped_bag_counts.counter_for_inbound_link(link_id),
        );
    }

    /// Every inbound link feeding `port`, in wiring order.
    ///
    /// Readable from `setup()`: a port's mailbox exists only once a link is
    /// wired into it, and WIRE runs before `setup()`, so a destination asking
    /// here learns how many producers it owes before the first bag arrives. A
    /// port with no links lists none rather than refusing — an unconnected
    /// input is a legal graph, not an error.
    ///
    /// Note: This should only be called from the processor's execution thread.
    pub fn inbound_link_names(&self, port: &str) -> Vec<InboundLinkName> {
        self.subscribers
            .bound_to_local_port(port)
            .map(|bound| bound.inbound_link_name.clone())
            .collect()
    }

    /// The one link feeding `port`, or `None` where it has none or several.
    ///
    /// What a windowed port reads its name off: a window is cut from bags
    /// rather than being one, so no entry carries the name — but a windowed
    /// port takes exactly one link (a second is refused at wire time), so the
    /// port itself answers.
    fn the_single_inbound_link_name_of(&self, port: &str) -> Option<InboundLinkName> {
        let mut feeding = self.subscribers.bound_to_local_port(port);
        let only = feeding.next()?;
        feeding
            .next()
            .is_none()
            .then(|| only.inbound_link_name.clone())
    }

    /// This processor's per-inbound-link dropped-bag counts, shared with the
    /// graph node's [`ProcessorMetrics`] so `graph` reads them live.
    ///
    /// [`ProcessorMetrics`]: crate::core::graph::ProcessorMetrics
    pub fn dropped_bag_counts_by_inbound_link(&self) -> Arc<DroppedBagCountsByInboundLink> {
        Arc::clone(&self.dropped_bag_counts)
    }

    /// Reclaim the destination-side ports for one disconnected `connect()` link.
    ///
    /// Drops the `link_id`-tagged subscriber; when its local input port has no
    /// remaining subscribers the mailbox is removed, and when the destination has
    /// none at all the shared listener is dropped — releasing the destination-keyed
    /// notify service so a reconnect recreates fresh-sized, refcounted ports rather
    /// than colliding with the stale service (`DoesNotSupportRequestedMinBufferSize`).
    ///
    /// Must be called from the processor's execution thread, in the same wiring
    /// phase a `connect` runs in.
    pub fn remove_channel_link(&self, link_id: &str) {
        let Some(local_port) = self.subscribers.remove_by_link(link_id) else {
            return;
        };
        self.dropped_bag_counts.forget_inbound_link(link_id);
        if !self.subscribers.port_still_bound(&local_port) {
            self.ports.lock().remove(&local_port);
        }
        if self.subscribers.is_empty() {
            self.listener.clear();
        }
    }

    /// Check if a listener has already been configured.
    pub fn has_listener(&self) -> bool {
        self.listener.get().is_some()
    }

    /// Set the iceoryx2 Listener for fd-multiplexed wakeups.
    ///
    /// Note: This should only be called from the processor's execution thread.
    pub fn set_listener(&self, listener: Listener<ipc::Service>) {
        self.listener.set(listener);
    }

    /// Returns the underlying listener fd if a listener has been configured.
    ///
    /// The fd is owned by the [`Listener`] — callers must NOT `close()` it and
    /// MUST stop using it before [`InputMailboxesInner`] is dropped. Suitable
    /// for registering with `epoll_ctl(EPOLL_CTL_ADD)` or `select` from the
    /// processor's execution thread.
    pub fn listener_fd(&self) -> Option<i32> {
        // SAFETY: native_handle() is unsafe per iceoryx2-bb-posix because storing
        // the value across the Listener's lifetime would dangle. We return the
        // raw int and document that callers must drop usage before the Listener
        // is dropped, mirroring the FileDescriptor lifetime contract.
        self.listener
            .get()
            .map(|l| unsafe { l.file_descriptor().native_handle() })
    }

    /// Drain any pending event-IDs from the listener so the fd transitions
    /// back to the not-readable state. No-op when no listener is configured.
    ///
    /// Call this after `epoll_wait` reports the fd readable, before the next
    /// `epoll_wait`, otherwise the wait returns immediately on the same event.
    pub fn drain_listener(&self) {
        if let Some(listener) = self.listener.get() {
            if let Err(e) = listener.try_wait_all(|_event_id| {}) {
                tracing::trace!(
                    "InputMailboxes: drain_listener try_wait_all failed: {:?}",
                    e
                );
            }
        }
    }

    /// Receive all pending payloads from every channel subscriber and route them
    /// to mailboxes by the subscriber's local-port binding.
    ///
    /// This is called automatically by `read()` and `has_data()`, but can be
    /// called explicitly if needed.
    ///
    /// Note: This should only be called from the thread that owns the subscribers.
    pub fn receive_pending(&self) {
        for bound in self.subscribers.as_slice() {
            loop {
                match bound.subscriber.receive() {
                    Ok(Some(sample)) => {
                        let slice: &[u8] = sample.payload();
                        if slice.len() < FRAME_HEADER_SIZE {
                            tracing::warn!(
                                "InputMailboxes: received slice too small ({} < {})",
                                slice.len(),
                                FRAME_HEADER_SIZE
                            );
                            continue;
                        }
                        let ports = self.ports.lock();
                        if let Some(port_config) = ports.get(&bound.local_port) {
                            // The read side's per-frame cost after #1822 is this
                            // `to_vec` plus the header-strip memmove in
                            // `read_raw_bounded`. The copy is irreducible without
                            // parking the iceoryx2 `Sample` here instead of bytes —
                            // pinning shm slots while frames sit queued and coupling
                            // the mailbox's drop-oldest depth to the subscriber
                            // ring's. The memmove could fold into this copy by
                            // parsing the header here and queuing payload +
                            // timestamp, but that moves the stamped-length refusal
                            // off the read path (today a typed, port-named error to
                            // the reading processor, not a receive-time drop) and
                            // reshapes the mailbox's raw-wire-frame element
                            // contract (`route`, `drain`, [`PortMailbox`]).
                            port_config.mailbox.push_frame_from_inbound_link(
                                slice.to_vec(),
                                &bound.dropped_bag_counter,
                                &bound.inbound_link_name,
                            );
                        } else {
                            tracing::warn!(
                                port = %bound.local_port,
                                "InputMailboxes: channel delivered a frame but its bound \
                                 local port has no mailbox"
                            );
                        }
                    }
                    Ok(None) => break, // no more samples on this subscriber
                    Err(e) => {
                        tracing::error!("InputMailboxes: subscriber.receive() FAILED: {:?}", e);
                        break;
                    }
                }
            }
        }
    }

    /// Read the next frame for `port` into a caller buffer bounded by `out_cap`
    /// bytes, following the port's read mode.
    ///
    /// This is the grow-and-retry primitive behind the out-of-process read
    /// path: with PowerOfTwo publisher growth a frame can exceed any fixed
    /// receive buffer, so a frame that would not fit `out_cap` is stashed
    /// ([`PortConfig::staged_oversized`]) rather than dropped and reported as
    /// [`BoundedReadOutcome::NeedsLargerBuffer`]. The caller resizes to
    /// `required_bytes` and calls again; the staged frame is re-delivered in
    /// order.
    pub fn read_raw_bounded(&self, port: &str, out_cap: usize) -> Result<BoundedReadOutcome> {
        self.receive_pending();

        let (staged, audio_windowing) = {
            let mut ports = self.ports.lock();
            let port_config = ports
                .get_mut(port)
                .ok_or_else(|| Error::Link(format!("Unknown input port: {}", port)))?;
            (
                port_config.staged_oversized.take(),
                port_config.audio_windowing.clone(),
            )
        };

        // The stage's decode, channel convert, resample and framing all run
        // here, with the `ports` mutex released — it guards the port map, and a
        // resampler pass is not port-map work.
        let candidate = match staged {
            Some(staged) => Some(staged),
            None => match audio_windowing {
                InstalledInputPortAudioWindowing::NotWindowed => {
                    self.pop_one_bag_off_the_mailbox(port)?
                }
                // Nothing, and deliberately not the bag underneath: a port
                // whose contract is unsettled has no exact size to cut one to.
                InstalledInputPortAudioWindowing::AwaitingItsDeviceStreamFormat => None,
                InstalledInputPortAudioWindowing::Windowed(stage) => {
                    self.next_window_out_of_the_stage(port, &stage)?
                }
            },
        };
        let Some(candidate) = candidate else {
            return Ok(BoundedReadOutcome::Empty);
        };

        if candidate.body.len() <= out_cap {
            return Ok(BoundedReadOutcome::Frame {
                data: candidate.body,
                timestamp_ns: candidate.first_sample_or_publish_timestamp_ns,
                inbound_link_name: candidate.inbound_link_name,
            });
        }

        let required_bytes = candidate.body.len();
        let mut ports = self.ports.lock();
        let port_config = ports
            .get_mut(port)
            .ok_or_else(|| unknown_input_port(port))?;
        port_config.staged_oversized = Some(candidate);
        Ok(BoundedReadOutcome::NeedsLargerBuffer { required_bytes })
    }

    /// Pop the next queued frame for `port` per its read mode and strip its
    /// header, or `None` when the mailbox is empty.
    fn pop_one_bag_off_the_mailbox(&self, port: &str) -> Result<Option<BagBodyForTheReader>> {
        let raw = {
            let ports = self.ports.lock();
            let port_config = ports.get(port).ok_or_else(|| unknown_input_port(port))?;
            match port_config.read_mode {
                ReadMode::SkipToLatest => port_config.mailbox.pop_latest(),
                ReadMode::ReadNextInOrder => port_config.mailbox.pop(),
            }
        };
        let Some(delivered) = raw else {
            return Ok(None);
        };
        let inbound_link_name = delivered.inbound_link_name;
        let mut frame_bytes_from_wire = delivered.payload;

        let header = FrameHeader::read_from_slice(&frame_bytes_from_wire);
        let stamped_payload_bytes = header.len as usize;
        let available_payload_bytes = frame_bytes_from_wire.len() - FRAME_HEADER_SIZE;
        if stamped_payload_bytes > available_payload_bytes {
            return Err(Error::FrameHeaderPayloadLengthExceedsFrameBytes {
                port: port.to_string(),
                stamped_payload_bytes,
                available_payload_bytes,
            });
        }
        frame_bytes_from_wire.copy_within(
            FRAME_HEADER_SIZE..FRAME_HEADER_SIZE + stamped_payload_bytes,
            0,
        );
        frame_bytes_from_wire.truncate(stamped_payload_bytes);
        Ok(Some(BagBodyForTheReader {
            body: frame_bytes_from_wire,
            first_sample_or_publish_timestamp_ns: header.timestamp_ns,
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
        loop {
            if let Some(window) = stage.lock().next_ready_window()? {
                return Ok(Some(BagBodyForTheReader {
                    inbound_link_name: self.the_single_inbound_link_name_of(port),
                    ..window
                }));
            }
            let Some(bag) = self.pop_one_bag_off_the_mailbox(port)? else {
                return Ok(None);
            };
            stage.lock().accept(&bag.body)?;
        }
    }

    /// Read the next frame for `port` with no buffer bound — the host-internal
    /// convenience over [`Self::read_raw_bounded`]. Returns
    /// `Ok(Some((data, timestamp_ns)))` if data is available, `Ok(None)` if the
    /// mailbox is empty.
    pub fn read_raw(&self, port: &str) -> Result<Option<(Vec<u8>, i64)>> {
        Ok(self
            .read_one_frame_unbounded(port)?
            .map(|(data, timestamp_ns, _)| (data, timestamp_ns)))
    }

    /// The next frame for `port` with no buffer bound, and the link it arrived
    /// on — the one read both public unbounded reads sit on.
    ///
    /// A `usize::MAX` cap always fits, so the grow-and-retry outcome cannot
    /// arise here and is reported as the internal inconsistency it would be.
    fn read_one_frame_unbounded(
        &self,
        port: &str,
    ) -> Result<Option<(Vec<u8>, i64, Option<InboundLinkName>)>> {
        match self.read_raw_bounded(port, usize::MAX)? {
            BoundedReadOutcome::Empty => Ok(None),
            BoundedReadOutcome::Frame {
                data,
                timestamp_ns,
                inbound_link_name,
            } => Ok(Some((data, timestamp_ns, inbound_link_name))),
            BoundedReadOutcome::NeedsLargerBuffer { required_bytes } => Err(Error::Link(format!(
                "read of input port '{port}': frame of {required_bytes} bytes did not fit an \
                 unbounded buffer"
            ))),
        }
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
    ) -> Result<Option<(Vec<u8>, i64, InboundLinkName)>> {
        let Some((data, timestamp_ns, inbound_link_name)) = self.read_one_frame_unbounded(port)?
        else {
            return Ok(None);
        };
        let Some(inbound_link_name) = inbound_link_name else {
            return Err(Error::Link(format!(
                "a bag on input port '{port}' arrived with no inbound link behind it, so \
                 there is no link to name and the bag is consumed. Read such a port with \
                 `read_raw`, which names no link"
            )));
        };
        Ok(Some((data, timestamp_ns, inbound_link_name)))
    }

    /// The next frame for `port` deserialized into `T`, with the inbound link
    /// it arrived on, or `None` when the mailbox is empty.
    ///
    /// `None` rather than an error on an empty mailbox, unlike
    /// [`InputMailboxes::read`]: a destination fanning in N links reads until
    /// its port runs dry, and an empty port is that loop ending.
    pub fn read_from_inbound_link<T: DeserializeOwned>(
        &self,
        port: &str,
    ) -> Result<Option<(T, InboundLinkName)>> {
        let Some((body, _timestamp_ns, inbound_link_name)) =
            self.read_raw_from_inbound_link(port)?
        else {
            return Ok(None);
        };
        Ok(Some((deserialize_bag_body(&body)?, inbound_link_name)))
    }

    /// Check if a port has any payloads available. This first
    /// receives any pending data from the iceoryx2 Subscriber.
    ///
    /// On a port declaring a window contract this means a full window, not an
    /// arrived bag: the contract promises `process()` exact-size blocks, so a
    /// reactive processor that woke and found nothing would contradict it.
    pub fn has_data(&self, port: &str) -> bool {
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

    /// Drain all raw frame slices from the given port's mailbox.
    pub fn drain(&self, port: &str) -> Vec<Vec<u8>> {
        let ports = self.ports.lock();
        ports
            .get(port)
            .into_iter()
            .flat_map(|p| p.mailbox.drain())
            .collect()
    }

    /// Route a raw frame slice into the mailbox named by the frame's stamped
    /// source-port key. This is the manual-injection path — used only by
    /// callers that synthesize a frame directly (SDK e2e harness + unit
    /// tests), NOT the live receive path (which is [`receive_pending`],
    /// routing by subscriber-to-local-port binding). The two differ: the live
    /// path is binding-keyed so two destinations subscribing one channel each
    /// land in their own local port, whereas this routes by the header's
    /// stamped source port.
    ///
    /// Returns true if the payload was routed, false if no matching mailbox
    /// exists. Thread-safe: can be called from any thread.
    ///
    /// [`receive_pending`]: Self::receive_pending
    pub fn route(&self, raw: Vec<u8>) -> bool {
        if raw.len() < FRAME_HEADER_SIZE {
            return false;
        }
        let port = FrameHeader::read_port_from_slice(&raw);
        let ports = self.ports.lock();
        if let Some(port_config) = ports.get(port) {
            port_config
                .mailbox
                .push_frame_without_inbound_link_attribution(raw);
            true
        } else {
            false
        }
    }

    /// Get the list of configured port names.
    pub fn port_names(&self) -> Vec<String> {
        self.ports.lock().keys().cloned().collect()
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
pub struct InputMailboxes {
    /// Opaque handle: `Arc::into_raw(Arc<InputMailboxesInner>)`. Null
    /// on a freshly-constructed processor before
    /// `set_iceoryx2_resources` fires.
    pub(crate) handle: *const c_void,
}

// SAFETY: `handle` points at an `Arc<InputMailboxesInner>` whose
// interior is Send+Sync (the inner uses parking_lot::Mutex for
// `ports` and the SendableSubscriber/SendableListener wrappers
// declare Send+Sync above).
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

    /// Read and deserialize a frame from the given port.
    ///
    /// Uses the port's read mode to determine consumption strategy:
    /// - `SkipToLatest`: Drains buffer, returns only the newest frame (video)
    /// - `ReadNextInOrder`: Returns oldest frame in FIFO order (audio)
    ///
    /// Source-compatible with the pre-#894 `InputMailboxes::read`.
    pub fn read<T: DeserializeOwned>(&self, port: &str) -> Result<T> {
        let raw = self
            .read_raw(port)?
            .ok_or_else(|| Error::Link(format!("No data available on port: {}", port)))?;
        deserialize_bag_body(&raw.0)
    }

    /// Read raw bytes and timestamp from the given port without
    /// deserialization. Returns `Ok(Some((data, timestamp_ns)))` on
    /// success, `Ok(None)` when the mailbox is empty.
    ///
    /// Sizes the receive buffer to
    /// [`streamlib_ipc_types::DEFAULT_EXPECTED_PAYLOAD_BYTES`] and grows on
    /// demand: a publisher under PowerOfTwo growth can deliver a frame larger
    /// than any fixed buffer, so when the host reports the next frame is bigger
    /// than `out_cap` (`out_len > buf.len()`, `has_data == true`) this resizes to
    /// exactly that length and reads again. The host stashes the oversized frame
    /// across the two calls (grow-and-retry), so nothing is dropped — retiring
    /// the pre-#1421 `max_payload_for_port` up-front sizing that dropped every
    /// frame past the authored budget.
    pub fn read_raw(&self, port: &str) -> Result<Option<(Vec<u8>, i64)>> {
        let Some(inner) = self.host_inner() else {
            return Ok(None);
        };
        inner.read_raw(port)
    }

    /// The next bag on `port` with the inbound link it arrived on.
    ///
    /// What a destination taking many links on one port reads with: each
    /// inbound link is one producer, named by the source channel name it
    /// subscribed to, so a sink can tell N streams apart without the producers
    /// having to identify themselves in their bags.
    ///
    /// Bags from one link keep that link's order; no interleaving is promised
    /// between two links. A bag no link delivered is refused by name, and the
    /// refusal consumes it.
    pub fn read_raw_from_inbound_link(
        &self,
        port: &str,
    ) -> Result<Option<(Vec<u8>, i64, InboundLinkName)>> {
        let Some(inner) = self.host_inner() else {
            return Ok(None);
        };
        inner.read_raw_from_inbound_link(port)
    }

    /// The next bag on `port` deserialized into `T`, with the inbound link it
    /// arrived on, or `None` when the mailbox is empty.
    pub fn read_from_inbound_link<T: DeserializeOwned>(
        &self,
        port: &str,
    ) -> Result<Option<(T, InboundLinkName)>> {
        let Some(inner) = self.host_inner() else {
            return Ok(None);
        };
        inner.read_from_inbound_link(port)
    }

    /// Every inbound link feeding `port`, in wiring order.
    ///
    /// Readable in `setup()` — WIRE runs before it — which is how a sink learns
    /// how many producers it owes before the first bag arrives. A port with no
    /// links lists none.
    pub fn inbound_link_names(&self, port: &str) -> Vec<InboundLinkName> {
        match self.host_inner() {
            Some(inner) => inner.inbound_link_names(port),
            None => Vec::new(),
        }
    }

    /// Whether `port` has been configured — a port has a mailbox only once a
    /// link is wired into it.
    pub fn has_port(&self, port: &str) -> bool {
        match self.host_inner() {
            Some(inner) => inner.has_port(port),
            None => false,
        }
    }

    /// Check if a port has any payloads available.
    pub fn has_data(&self, port: &str) -> bool {
        match self.host_inner() {
            Some(inner) => inner.has_data(port),
            None => false,
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
        inner.settle_a_ports_device_matched_audio_window_contract(port, matching)
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
    use crate::iceoryx2::PortKey;

    fn unique_suffix(tag: &str) -> String {
        format!(
            "test/input/{tag}/{}",
            mint_machine_global_unique_name_suffix()
        )
    }

    /// Build a wire frame for `port`: a header stamping `stamped_payload_bytes`
    /// over the body `carried_body`. The stamped and carried lengths are
    /// separate arguments because several tests exercise their divergence.
    fn wire_frame_stamping(
        port: &str,
        timestamp_ns: i64,
        stamped_payload_bytes: u32,
        carried_body: &[u8],
    ) -> Vec<u8> {
        let mut frame = vec![0u8; FRAME_HEADER_SIZE + carried_body.len()];
        FrameHeader::new(port, timestamp_ns, stamped_payload_bytes)
            .expect("port fits PortKey")
            .write_to_slice(&mut frame[..FRAME_HEADER_SIZE]);
        frame[FRAME_HEADER_SIZE..].copy_from_slice(carried_body);
        frame
    }

    /// Open a channel sized for `buffered_frames` in flight and hand back the
    /// publisher (kept alive so sent samples stay resident) plus a bound
    /// subscriber, so a test can publish one frame at a time.
    fn open_channel_for_one_link(
        node: &iceoryx2::node::Node<ipc::Service>,
        tag: &str,
        buffered_frames: usize,
    ) -> (
        iceoryx2::port::publisher::Publisher<ipc::Service, [u8], ()>,
        Subscriber<ipc::Service, [u8], ()>,
    ) {
        open_channel_for_one_link_loaning(node, tag, buffered_frames, 4096)
    }

    /// The same, with the publisher's loan sized explicitly — an audio block of
    /// a thousand `f32` samples outgrows the 4 KiB the frame tests want.
    fn open_channel_for_one_link_loaning(
        node: &iceoryx2::node::Node<ipc::Service>,
        tag: &str,
        buffered_frames: usize,
        loan_bytes: usize,
    ) -> (
        iceoryx2::port::publisher::Publisher<ipc::Service, [u8], ()>,
        Subscriber<ipc::Service, [u8], ()>,
    ) {
        let pubsub = node
            .service_builder(&ServiceName::new(&unique_suffix(tag)).unwrap())
            .publish_subscribe::<[u8]>()
            .max_publishers(2)
            .subscriber_max_buffer_size(buffered_frames)
            .enable_safe_overflow(true)
            .open_or_create()
            .unwrap();
        let publisher = pubsub
            .publisher_builder()
            .initial_max_slice_len(loan_bytes)
            .create()
            .unwrap();
        let subscriber = pubsub.subscriber_builder().create().unwrap();
        (publisher, subscriber)
    }

    /// Publish one frame stamped with `source_port` onto an open channel.
    fn publish_one_frame(
        publisher: &iceoryx2::port::publisher::Publisher<ipc::Service, [u8], ()>,
        source_port: &str,
        body: &[u8],
    ) {
        let frame = wire_frame_stamping(source_port, 0, body.len() as u32, body);
        let sample = publisher.loan_slice_uninit(frame.len()).unwrap();
        sample.write_from_slice(&frame).send().unwrap();
    }

    /// Driving the iceoryx2 Event service end-to-end: notify must transition
    /// the Listener fd to readable within a short bounded window so an epoll
    /// or select wait wakes promptly.
    #[test]
    fn listener_fd_is_valid_and_readable_after_notify() {
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let name = unique_suffix("notify");

        let svc = node
            .service_builder(&ServiceName::new(&name).unwrap())
            .event()
            .max_notifiers(2)
            .max_listeners(1)
            .open_or_create()
            .unwrap();
        let notifier = svc.notifier_builder().create().unwrap();
        let listener = svc.listener_builder().create().unwrap();

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

        // Build a minimal valid frame for `port_a` and route it directly
        // — bypasses the iceoryx2 subscriber, exercising only the
        // mailbox-depth accounting.
        let make_frame = |port: &str| wire_frame_stamping(port, 0, 4, &[1, 2, 3, 4]);

        // Burst: three frames on port_a, two on port_b.
        for _ in 0..3 {
            assert!(mailboxes.route(make_frame("port_a")));
        }
        for _ in 0..2 {
            assert!(mailboxes.route(make_frame("port_b")));
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

    /// N→1 fan-in DELIVERY lock (#1419): a destination consuming TWO inbound
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
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher_a, sub_a) = open_channel_for_one_link(&node, "fanin/a", 1);
        let (publisher_b, sub_b) = open_channel_for_one_link(&node, "fanin/b", 1);
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
        while let Some((data, _ts)) = mailboxes.read_raw("in").unwrap() {
            payloads.push(data);
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
    /// receive at all, overflows the iceoryx2 subscriber ring instead, and
    /// that loss is counted nowhere.
    #[test]
    fn each_inbound_link_reports_its_own_losses_at_a_stalled_ordered_port() {
        const MAILBOX_DEPTH: usize = 2;
        const FRAMES_PER_LINK: usize = 5;

        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher_a, subscriber_a) =
            open_channel_for_one_link(&node, "drop-count/a", FRAMES_PER_LINK);
        let (publisher_b, subscriber_b) =
            open_channel_for_one_link(&node, "drop-count/b", FRAMES_PER_LINK);

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
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher_a, sub_a) = open_channel_for_one_link(&node, "naming/a", 4);
        let (publisher_b, sub_b) = open_channel_for_one_link(&node, "naming/b", 4);

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
        while let Some((body, _stamp, inbound_link_name)) = mailboxes
            .read_raw_from_inbound_link("in")
            .expect("a delivered bag names its link")
        {
            named.push((
                String::from_utf8(body).unwrap(),
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

        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher_a, subscriber_a) =
            open_channel_for_one_link(&node, "naming-counts/a", FRAMES_PER_LINK);
        let (publisher_b, subscriber_b) =
            open_channel_for_one_link(&node, "naming-counts/b", FRAMES_PER_LINK);

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
        while let Some((_body, _stamp, inbound_link_name)) = mailboxes
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
    /// [`InputMailboxes::read`]: a drained port is `Ok(None)` here where `read`
    /// is an error. A destination fanning in N links reads until its port runs
    /// dry, and an empty port is that loop ending rather than a failure.
    #[test]
    fn the_typed_read_deserializes_the_bag_and_a_drained_port_is_not_an_error() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct OneTrackBag {
            track: String,
        }

        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher, subscriber) = open_channel_for_one_link(&node, "naming/typed", 4);

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
            .read_from_inbound_link::<OneTrackBag>("in")
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
                .read_from_inbound_link::<OneTrackBag>("in")
                .expect("a drained port is not a failure")
                .is_none(),
            "a drained port must end a sink's read loop rather than raise",
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
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (_publisher_a, sub_a) = open_channel_for_one_link(&node, "listing/a", 1);
        let (_publisher_b, sub_b) = open_channel_for_one_link(&node, "listing/b", 1);
        let (_publisher_c, sub_c) = open_channel_for_one_link(&node, "listing/c", 1);

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
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher, subscriber) = open_channel_for_one_link(&node, "naming/windowed", 8);

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
            publish_one_frame(
                &publisher,
                "mic_out",
                &mono_audio_block_body(160, 16_000, (block * 160 * 1_000_000_000 / 16_000) as i64),
            );
        }

        let (_window, _stamp, inbound_link_name) = mailboxes
            .read_raw_from_inbound_link("in")
            .expect("640 samples completes a 512 window")
            .expect("a full window reads out");
        assert_eq!(
            inbound_link_name.as_str(),
            "pmicrophone/audio_out",
            "a window names the one link its bags arrived on",
        );
    }

    /// A bag nothing delivered has no link to name, and the read says so
    /// instead of borrowing one.
    ///
    /// The refusal consumes the bag it refused — the mailbox admits no peek, so
    /// the frame leaves the queue before its link is looked at — and `read_raw`,
    /// which names no link, is the read for such a port.
    #[test]
    fn an_injected_bag_with_no_inbound_link_is_refused_by_name_rather_than_borrowing_one() {
        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", 4, ReadMode::ReadNextInOrder);
        assert!(
            mailboxes.route(wire_frame_stamping("in", 0, 5, b"hello")),
            "the frame must route to port 'in'"
        );

        let refusal = mailboxes
            .read_raw_from_inbound_link("in")
            .expect_err("an injected bag has no link to name")
            .to_string();
        assert!(
            refusal.contains("'in'") && refusal.contains("read_raw"),
            "the refusal must name the port and the read that does work; got {refusal}"
        );
        assert!(
            mailboxes
                .read_raw("in")
                .expect("the plain read is untouched by the refusal")
                .is_none(),
            "the refusal consumes the bag it refused, so nothing is left behind it"
        );

        assert!(
            mailboxes.route(wire_frame_stamping("in", 0, 5, b"world")),
            "the frame must route to port 'in'"
        );
        let (body, _stamp) = mailboxes
            .read_raw("in")
            .expect("the plain read is untouched")
            .expect("a read that names no link reads an injected bag whole");
        assert_eq!(body, b"world");
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
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher, subscriber) =
            open_channel_for_one_link_loaning(&node, "window/resampled", 32, 16_384);

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
            publish_one_frame(
                &publisher,
                "mic_out",
                &mono_audio_block_body(
                    SOURCE_FRAMES_PER_BLOCK as usize,
                    48_000,
                    (block * SOURCE_FRAMES_PER_BLOCK * 1_000_000_000 / 48_000) as i64,
                ),
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

    /// One audio-block bag body carrying `frames` per-channel mono samples at
    /// `sample_rate`, the shape a microphone publishes.
    fn mono_audio_block_body(
        frames: usize,
        sample_rate: u32,
        first_sample_timestamp_ns: i64,
    ) -> Vec<u8> {
        #[derive(serde::Serialize)]
        struct AudioBlockBag<'a> {
            #[serde(rename = "samples", with = "serde_bytes")]
            interleaved_sample_bytes: &'a [u8],
            sample_rate: u32,
            channels: u32,
            sample_count: u32,
            dtype: &'a str,
            first_sample_timestamp_ns: i64,
        }
        let payload: Vec<u8> = (0..frames)
            .flat_map(|index| (index as f32 / frames as f32).to_le_bytes())
            .collect();
        rmp_serde::to_vec_named(&AudioBlockBag {
            interleaved_sample_bytes: &payload,
            sample_rate,
            channels: 1,
            sample_count: frames as u32,
            dtype: "f32",
            first_sample_timestamp_ns,
        })
        .expect("an audio block bag encodes")
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

    /// The scalars one emitted window carries, read back out of the bag body
    /// the stage wrote.
    fn scalars_of_one_window(body: &[u8]) -> (Vec<f32>, u32, u32) {
        #[derive(serde::Deserialize)]
        struct AudioBlockBag {
            #[serde(rename = "samples", with = "serde_bytes")]
            interleaved_sample_bytes: Vec<u8>,
            sample_rate: u32,
            channels: u32,
        }
        let bag: AudioBlockBag =
            rmp_serde::from_slice(body).expect("the stage writes an audio block bag");
        let scalars = bag
            .interleaved_sample_bytes
            .chunks_exact(4)
            .map(|scalar| f32::from_le_bytes(scalar.try_into().expect("four bytes")))
            .collect();
        (scalars, bag.sample_rate, bag.channels)
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
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher, subscriber) =
            open_channel_for_one_link_loaning(&node, "window/awaiting", 8, 16_384);

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
            publish_one_frame(
                &publisher,
                "mic_out",
                &mono_audio_block_body(480, 16_000, (block * 30_000_000) as i64),
            );
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

        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher, subscriber) = open_channel_for_one_link(&node, "window/awaiting-drop", 4);

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

        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher, subscriber) = open_channel_for_one_link(&node, "window/settled-drop", 4);

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
            let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
            let (publisher, subscriber) = open_channel_for_one_link_loaning(&node, tag, 16, 16_384);
            mailboxes.add_channel_subscriber(
                "in",
                "L-only",
                &InboundLinkName::from("ponly/out"),
                subscriber,
            );
            for block in 0..4u64 {
                publish_one_frame(
                    &publisher,
                    "mic_out",
                    &mono_audio_block_body(480, 16_000, (block * 30_000_000) as i64),
                );
            }
            mailboxes.receive_pending();
        }

        /// Every window port `"in"` hands back, each held to the device format
        /// its contract settled to.
        fn windows_read_out_of(mailboxes: &InputMailboxesInner) -> Vec<Vec<f32>> {
            let mut windows = Vec::new();
            while let Some((body, _)) = mailboxes.read_raw("in").expect("the port reads") {
                let (scalars, sample_rate, channels) = scalars_of_one_window(&body);
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
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher, subscriber) = open_channel_for_one_link(&node, "window/readiness", 8);

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
        publish_one_frame(
            &publisher,
            "mic_out",
            &mono_audio_block_body(160, 16_000, 0),
        );
        assert!(
            !mailboxes.has_data("in"),
            "160 of 512 samples is not a window, and a wake here would dispatch a \
             process() with nothing to read"
        );
        assert!(!mailboxes.any_port_has_data());

        // Enough to complete one.
        for block in 1..4u64 {
            publish_one_frame(
                &publisher,
                "mic_out",
                &mono_audio_block_body(160, 16_000, (block * 160 * 1_000_000_000 / 16_000) as i64),
            );
        }
        assert!(
            mailboxes.has_data("in"),
            "640 samples completes a 512 window"
        );
        assert!(mailboxes.any_port_has_data());

        let (window, _) = mailboxes
            .read_raw("in")
            .expect("a window reads")
            .expect("the gate said a window was ready");
        assert!(!window.is_empty());
    }

    /// One 1024-sample quantum against a 512/512 contract satisfies exactly two
    /// windows — the count the reactive drain loop dispatches `process()`.
    #[test]
    fn one_1024_sample_quantum_reads_out_of_a_512_512_port_exactly_twice() {
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher, subscriber) =
            open_channel_for_one_link_loaning(&node, "window/quantum", 4, 16_384);

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
        publish_one_frame(
            &publisher,
            "mic_out",
            &mono_audio_block_body(1024, 16_000, 0),
        );

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
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher, subscriber) =
            open_channel_for_one_link_loaning(&node, "window/untouched", 4, 16_384);

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", 8, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber(
            "in",
            "L-only",
            &InboundLinkName::from("ponly/out"),
            subscriber,
        );

        let published = mono_audio_block_body(1024, 16_000, 77);
        publish_one_frame(&publisher, "mic_out", &published);

        assert!(
            mailboxes.has_data("in"),
            "an arrived bag is data on an unwindowed port"
        );
        let (read_back, _) = mailboxes.read_raw("in").expect("reads").expect("a bag");
        assert_eq!(read_back, published);
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
            let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
            let (publisher, subscriber) = open_channel_for_one_link(&node, tag, 4);
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
                publish_one_frame(
                    &publisher,
                    "mic_out",
                    &mono_audio_block_body(160, 16_000, (block as i64) * 10_000_000),
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

        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher, subscriber) = open_channel_for_one_link(&node, "window/depth", 4);
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
            publish_one_frame(
                &publisher,
                "mic_out",
                &mono_audio_block_body(160, 16_000, (block as i64) * 10_000_000),
            );
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
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher, subscriber) = open_channel_for_one_link(&node, "window/refusal", 4);

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
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher_a, subscriber_a) = open_channel_for_one_link(&node, "no-drop/a", 4);
        let (publisher_b, subscriber_b) = open_channel_for_one_link(&node, "no-drop/b", 4);

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
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
        let (publisher, subscriber) = open_channel_for_one_link(&node, "reclaim-count", 4);

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

    /// Per-link destination reclaim (#1549): a destination fanning two inbound
    /// links into ONE local port holds two tagged subscribers plus one shared
    /// listener. Disconnecting one link drops only its subscriber (the port
    /// mailbox and listener survive so the other link keeps delivering);
    /// disconnecting the last link removes the port mailbox AND drops the shared
    /// listener — releasing the notify service so a reconnect recreates it fresh.
    ///
    /// Fail-without-fix: revert `remove_channel_link` to a no-op (the pre-#1549
    /// `close_iceoryx2_service` behaviour) and the final disconnect leaves the
    /// port and listener live, so `has_port` / `has_listener` stay true and the
    /// release assertions fail.
    #[test]
    fn remove_channel_link_reclaims_per_link_then_drops_port_and_listener() {
        let node = NodeBuilder::new().create::<ipc::Service>().unwrap();

        let open_subscriber = |tag: &str| {
            node.service_builder(&ServiceName::new(&unique_suffix(tag)).unwrap())
                .publish_subscribe::<[u8]>()
                .max_publishers(2)
                .open_or_create()
                .unwrap()
                .subscriber_builder()
                .create()
                .unwrap()
        };
        let listener = node
            .service_builder(&ServiceName::new(&unique_suffix("reclaim/notify")).unwrap())
            .event()
            .max_notifiers(2)
            .max_listeners(1)
            .open_or_create()
            .unwrap()
            .listener_builder()
            .create()
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

    /// Grow-and-retry staging (#1421): a frame larger than the caller's buffer
    /// is NOT dropped — [`InputMailboxesInner::read_raw_bounded`] reports its
    /// required length and stashes it, then re-delivers it intact on the retry
    /// with a large-enough buffer.
    ///
    /// Fail-without-fix: revert `read_raw_bounded` to consume-then-error on a
    /// too-small buffer and the second read returns `Empty` (the frame was
    /// dropped) — the byte-for-byte re-delivery assertion fails.
    #[test]
    fn read_raw_bounded_stages_oversized_frame_and_redelivers() {
        let inner = InputMailboxesInner::new();
        inner.add_port("in", 8, ReadMode::ReadNextInOrder);

        let body: Vec<u8> = (0..300u32).map(|i| (i % 251) as u8).collect();
        let frame = wire_frame_stamping("in", 42, body.len() as u32, &body);
        assert!(inner.route(frame), "frame must route to port 'in'");

        // Buffer too small: the frame is reported (not consumed).
        match inner.read_raw_bounded("in", 100).expect("bounded read") {
            BoundedReadOutcome::NeedsLargerBuffer { required_bytes } => {
                assert_eq!(required_bytes, body.len());
            }
            BoundedReadOutcome::Empty => panic!("expected NeedsLargerBuffer, got Empty"),
            BoundedReadOutcome::Frame { .. } => {
                panic!("expected NeedsLargerBuffer, but the too-small buffer delivered a Frame")
            }
        }

        // A staged frame is data waiting. Before the readiness gate consulted
        // it, a grow-and-retry frame with an empty mailbox behind it reported
        // no data — so the reactive drain loop stopped and the frame sat until
        // some later bag happened to wake the port.
        assert!(
            inner.has_data("in"),
            "a frame staged by a bounded read is still waiting for its retry"
        );
        assert!(inner.any_port_has_data());

        // Retry with a large-enough buffer: the SAME frame is re-delivered.
        match inner
            .read_raw_bounded("in", body.len())
            .expect("bounded read retry")
        {
            BoundedReadOutcome::Frame {
                data, timestamp_ns, ..
            } => {
                assert_eq!(data, body, "staged frame must re-deliver byte-for-byte");
                assert_eq!(timestamp_ns, 42);
            }
            _ => panic!("expected the staged frame to be re-delivered"),
        }

        // The staged frame was consumed exactly once — the mailbox is now empty.
        assert!(!inner.has_data("in"));
        assert!(matches!(
            inner
                .read_raw_bounded("in", body.len())
                .expect("bounded read"),
            BoundedReadOutcome::Empty
        ));
    }

    /// The payload length is the last wire-derived number the read path trusts,
    /// and a frame claiming more than it carries has no safe default — it is
    /// unusable, so it must surface as a typed error naming the port and both
    /// numbers rather than slicing past the frame.
    ///
    /// Fail-without-fix: drop the bound and `read_raw_bounded` slices
    /// `[76..76 + 4096]` out of an 84-byte frame — "range end index 4172 out of
    /// range for slice of length 84".
    #[test]
    fn read_raw_bounded_rejects_a_frame_stamping_more_payload_than_it_carries() {
        const STAMPED: u32 = 4096;
        const CARRIED: usize = 8;

        let inner = InputMailboxesInner::new();
        inner.add_port("in", 64, ReadMode::ReadNextInOrder);

        let malformed = wire_frame_stamping("in", 42, STAMPED, &[0u8; CARRIED]);
        assert!(inner.route(malformed), "frame must route to port 'in'");

        let err = match inner.read_raw_bounded("in", usize::MAX) {
            Err(e) => e,
            Ok(_) => panic!("a frame stamping more than it carries must not read"),
        };
        assert!(
            matches!(
                &err,
                Error::FrameHeaderPayloadLengthExceedsFrameBytes {
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
        for expected in ["'in'", "4096", "8"] {
            assert!(
                rendered.contains(expected),
                "message must name {expected}: {rendered}"
            );
        }

        // The malformed frame is dropped, not staged — the port keeps serving.
        let body = [1u8, 2, 3, 4];
        let well_formed = wire_frame_stamping("in", 43, body.len() as u32, &body);
        assert!(
            inner.route(well_formed),
            "well-formed frame must route to port 'in'"
        );

        match inner
            .read_raw_bounded("in", usize::MAX)
            .expect("bounded read")
        {
            BoundedReadOutcome::Frame {
                data, timestamp_ns, ..
            } => {
                assert_eq!(data, body, "a well-formed frame still delivers intact");
                assert_eq!(timestamp_ns, 43);
            }
            _ => panic!("expected the well-formed frame to deliver"),
        }
    }

    /// A frame may carry more bytes than its header stamps — the wire
    /// contract trusts the stamped length, so the read delivers exactly that
    /// many payload bytes and the slack past them never reaches the caller.
    ///
    /// Fail-without-fix: hand the caller the frame's whole tail instead of
    /// slicing (or truncating) to the stamped length and the delivered
    /// payload grows by the slack bytes — the exact-length assertion fails.
    #[test]
    fn read_raw_bounded_delivers_exactly_the_stamped_payload_from_an_over_carrying_frame() {
        const STAMPED: usize = 200;
        const CARRIED: usize = 300;

        let inner = InputMailboxesInner::new();
        inner.add_port("in", 8, ReadMode::ReadNextInOrder);

        let body: Vec<u8> = (0..CARRIED as u32).map(|i| (i % 251) as u8).collect();
        let frame = wire_frame_stamping("in", 77, STAMPED as u32, &body);
        assert!(inner.route(frame), "frame must route to port 'in'");

        match inner
            .read_raw_bounded("in", usize::MAX)
            .expect("bounded read")
        {
            BoundedReadOutcome::Frame {
                data, timestamp_ns, ..
            } => {
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
            }
            _ => panic!("expected the over-carrying frame to deliver its stamped payload"),
        }

        // The slack was dropped with the frame, not staged — the port is empty.
        assert!(matches!(
            inner
                .read_raw_bounded("in", usize::MAX)
                .expect("bounded read"),
            BoundedReadOutcome::Empty
        ));
    }

    /// A malformed length prefix must not deliver a frame to a real mailbox.
    ///
    /// The saturating bound is only safe because a clamped name fails to match
    /// a port — which stops being true at exactly one width: a port named with
    /// the full 63 bytes is what the clamp produces, so an over-capacity prefix
    /// on a frame for that port reads back as the port's own name.
    #[test]
    fn route_refuses_a_frame_whose_port_key_prefix_is_over_capacity() {
        let longest_port = "z".repeat(PortKey::MAX_NAME_BYTES);

        let inner = InputMailboxesInner::new();
        inner.add_port(&longest_port, 64, ReadMode::ReadNextInOrder);

        let mut frame = wire_frame_stamping(&longest_port, 42, 4, &[0u8; 4]);
        frame[0] = 0xFF;

        assert!(
            !inner.route(frame),
            "a frame with an over-capacity prefix must not reach a real mailbox"
        );
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
}
