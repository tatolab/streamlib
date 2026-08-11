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

use super::mailbox::PortMailbox;
use super::read_mode::ReadMode;
use super::{FRAME_HEADER_SIZE, FrameHeader};
use crate::core::error::{Error, Result};

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
    subscriber: Subscriber<ipc::Service, [u8], ()>,
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
        subscriber: Subscriber<ipc::Service, [u8], ()>,
    ) {
        // SAFETY: Only called from the processor's execution thread during wiring.
        unsafe {
            (*self.0.get()).push(PortBoundSubscriber {
                link_id,
                local_port,
                subscriber,
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

    fn iter(&self) -> &[PortBoundSubscriber] {
        // SAFETY: Only called from the processor's execution thread.
        unsafe { &*self.0.get() }
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
    },
    /// The next frame is `required_bytes` long — larger than the caller's
    /// buffer. The caller must resize to at least this many bytes and read
    /// again; the frame is held for that retry.
    NeedsLargerBuffer {
        /// Byte length the caller's next buffer must reach.
        required_bytes: usize,
    },
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
    staged_oversized: Option<(Vec<u8>, i64)>,
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
}

impl InputMailboxesInner {
    /// Create a new empty inner.
    pub fn new() -> Self {
        Self {
            ports: parking_lot::Mutex::new(HashMap::new()),
            subscribers: SendableChannelSubscribers::new(),
            listener: SendableListener::new(),
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
            },
        );
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
        subscriber: Subscriber<ipc::Service, [u8], ()>,
    ) {
        self.subscribers
            .push(link_id.to_string(), local_port.to_string(), subscriber);
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
        for bound in self.subscribers.iter() {
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
                            port_config.mailbox.push(slice.to_vec());
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

        let mut ports = self.ports.lock();
        let port_config = ports
            .get_mut(port)
            .ok_or_else(|| Error::Link(format!("Unknown input port: {}", port)))?;

        let candidate: (Vec<u8>, i64) = if let Some(staged) = port_config.staged_oversized.take() {
            staged
        } else {
            let raw = match port_config.read_mode {
                ReadMode::SkipToLatest => port_config.mailbox.pop_latest(),
                ReadMode::ReadNextInOrder => port_config.mailbox.pop(),
            };
            match raw {
                None => return Ok(BoundedReadOutcome::Empty),
                Some(r) => {
                    let header = FrameHeader::read_from_slice(&r);
                    // The stamped length comes off the wire; the frame is the
                    // capacity it indexes. Unlike the port key it has no safe
                    // default — a frame claiming more than it carries is
                    // unusable — so it fails typed rather than saturating.
                    let stamped_payload_bytes = header.len as usize;
                    let available_payload_bytes = r.len().saturating_sub(FRAME_HEADER_SIZE);
                    if stamped_payload_bytes > available_payload_bytes {
                        return Err(Error::FrameHeaderPayloadLengthExceedsFrameBytes {
                            port: port.to_string(),
                            stamped_payload_bytes,
                            available_payload_bytes,
                        });
                    }
                    let data =
                        r[FRAME_HEADER_SIZE..FRAME_HEADER_SIZE + stamped_payload_bytes].to_vec();
                    (data, header.timestamp_ns)
                }
            }
        };

        if candidate.0.len() <= out_cap {
            Ok(BoundedReadOutcome::Frame {
                data: candidate.0,
                timestamp_ns: candidate.1,
            })
        } else {
            let required_bytes = candidate.0.len();
            port_config.staged_oversized = Some(candidate);
            Ok(BoundedReadOutcome::NeedsLargerBuffer { required_bytes })
        }
    }

    /// Read the next frame for `port` with no buffer bound — the host-internal
    /// convenience over [`Self::read_raw_bounded`]. Returns
    /// `Ok(Some((data, timestamp_ns)))` if data is available, `Ok(None)` if the
    /// mailbox is empty.
    pub fn read_raw(&self, port: &str) -> Result<Option<(Vec<u8>, i64)>> {
        match self.read_raw_bounded(port, usize::MAX)? {
            BoundedReadOutcome::Empty => Ok(None),
            BoundedReadOutcome::Frame { data, timestamp_ns } => Ok(Some((data, timestamp_ns))),
            // Unreachable: usize::MAX cap always fits.
            BoundedReadOutcome::NeedsLargerBuffer { required_bytes } => Err(Error::Link(format!(
                "read_raw: frame of {required_bytes} bytes did not fit an unbounded buffer"
            ))),
        }
    }

    /// Check if a port has any payloads available. This first
    /// receives any pending data from the iceoryx2 Subscriber.
    pub fn has_data(&self, port: &str) -> bool {
        self.receive_pending();
        self.ports
            .lock()
            .get(port)
            .map(|p| !p.mailbox.is_empty())
            .unwrap_or(false)
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
        self.ports.lock().values().any(|p| !p.mailbox.is_empty())
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
            port_config.mailbox.push(raw);
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
        rmp_serde::from_slice(&raw.0)
            .map_err(|e| Error::Link(format!("Failed to deserialize frame: {}", e)))
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

    /// Check if a port has any payloads available.
    pub fn has_data(&self, port: &str) -> bool {
        match self.host_inner() {
            Some(inner) => inner.has_data(port),
            None => false,
        }
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

    fn unique_suffix(tag: &str) -> String {
        format!(
            "test/input/{}/{}/{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
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
        let make_frame = |port: &str| -> Vec<u8> {
            let mut buf = vec![0u8; FRAME_HEADER_SIZE + 4];
            let header = FrameHeader::new(port, 0, 4).expect("port fits PortKey");
            header.write_to_slice(&mut buf);
            buf[FRAME_HEADER_SIZE..].copy_from_slice(&[1, 2, 3, 4]);
            buf
        };

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

        // Open a fresh channel, publish one frame stamped with its own source
        // port, and hand back the publisher (kept alive so the sent sample stays
        // resident) plus the bound subscriber.
        let open_channel_and_publish = |tag: &str, source_port: &str, data: &[u8]| {
            let pubsub = node
                .service_builder(&ServiceName::new(&unique_suffix(tag)).unwrap())
                .publish_subscribe::<[u8]>()
                .max_publishers(2)
                .open_or_create()
                .unwrap();
            let publisher = pubsub
                .publisher_builder()
                .initial_max_slice_len(4096)
                .create()
                .unwrap();
            let subscriber = pubsub.subscriber_builder().create().unwrap();

            let total = FRAME_HEADER_SIZE + data.len();
            let mut frame = vec![0u8; total];
            FrameHeader::new(source_port, 0, data.len() as u32)
                .expect("source port fits PortKey")
                .write_to_slice(&mut frame[..FRAME_HEADER_SIZE]);
            frame[FRAME_HEADER_SIZE..].copy_from_slice(data);
            let sample = publisher.loan_slice_uninit(total).unwrap();
            sample.write_from_slice(&frame).send().unwrap();

            (publisher, subscriber)
        };

        let (_pub_a, sub_a) = open_channel_and_publish("fanin/a", "src_a_out", b"frame-from-a");
        let (_pub_b, sub_b) = open_channel_and_publish("fanin/b", "src_b_out", b"frame-from-b");

        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", 64, ReadMode::ReadNextInOrder);
        mailboxes.add_channel_subscriber("in", "L-fanin-a", sub_a);
        mailboxes.add_channel_subscriber("in", "L-fanin-b", sub_b);

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
        inner.add_channel_subscriber("in", "L-link-a", open_subscriber("reclaim/a"));
        inner.add_channel_subscriber("in", "L-link-b", open_subscriber("reclaim/b"));
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
        let mut frame = vec![0u8; FRAME_HEADER_SIZE + body.len()];
        FrameHeader::new("in", 42, body.len() as u32)
            .expect("port fits PortKey")
            .write_to_slice(&mut frame[..FRAME_HEADER_SIZE]);
        frame[FRAME_HEADER_SIZE..].copy_from_slice(&body);
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

        // Retry with a large-enough buffer: the SAME frame is re-delivered.
        match inner
            .read_raw_bounded("in", body.len())
            .expect("bounded read retry")
        {
            BoundedReadOutcome::Frame { data, timestamp_ns } => {
                assert_eq!(data, body, "staged frame must re-deliver byte-for-byte");
                assert_eq!(timestamp_ns, 42);
            }
            _ => panic!("expected the staged frame to be re-delivered"),
        }

        // The staged frame was consumed exactly once — the mailbox is now empty.
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

        let mut frame = vec![0u8; FRAME_HEADER_SIZE + CARRIED];
        FrameHeader::new("in", 42, STAMPED)
            .expect("port fits PortKey")
            .write_to_slice(&mut frame[..FRAME_HEADER_SIZE]);
        assert!(inner.route(frame), "frame must route to port 'in'");

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
        let mut good = vec![0u8; FRAME_HEADER_SIZE + body.len()];
        FrameHeader::new("in", 43, body.len() as u32)
            .expect("port fits PortKey")
            .write_to_slice(&mut good[..FRAME_HEADER_SIZE]);
        good[FRAME_HEADER_SIZE..].copy_from_slice(&body);
        assert!(
            inner.route(good),
            "well-formed frame must route to port 'in'"
        );

        match inner
            .read_raw_bounded("in", usize::MAX)
            .expect("bounded read")
        {
            BoundedReadOutcome::Frame { data, timestamp_ns } => {
                assert_eq!(data, body, "a well-formed frame still delivers intact");
                assert_eq!(timestamp_ns, 43);
            }
            _ => panic!("expected the well-formed frame to deliver"),
        }
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
