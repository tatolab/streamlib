// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Input mailboxes for receiving frames from upstream processors.
//!
//! # Two-type split: PluginAbiObject vs. inner
//!
//! Issue #894 retires the last shared-Rust-type plugin ABI crossing
//! by splitting this module's public surface into two types:
//!
//! - [`InputMailboxesInner`] holds the actual state — the
//!   `HashMap<port, PortConfig>` of per-port mailboxes plus the
//!   thread-local `Subscriber` and `Listener` wrappers. All
//!   per-frame `receive_pending` + mailbox push/pop work runs
//!   here; only the host references this type directly.
//! - [`InputMailboxes`] is the public `#[repr(C)] { handle, vtable }`
//!   PluginAbiObject that processor structs hold via the macro-emitted
//!   `inputs: InputMailboxes` field. From inside `process()` the
//!   cdylib reaches input data exclusively through `read` /
//!   `read_raw` / `has_data` on this PluginAbiObject; the vtable dispatches
//!   to the host-allocated inner.
//!
//! Host-side wiring code that needs to mutate the inner
//! (`add_port`, `add_channel_subscriber`, `set_listener`, `listener_fd`,
//! `drain_listener`, etc.) operates on `Arc<InputMailboxesInner>`
//! directly via the methods declared on the inner type — no
//! PluginAbiObject, no plugin ABI hop.

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iceoryx2::port::listener::Listener;
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::*;
use serde::de::DeserializeOwned;
use streamlib_plugin_abi::InputMailboxesVTable;

use super::mailbox::PortMailbox;
use super::read_mode::ReadMode;
use super::{FRAME_HEADER_SIZE, FrameHeader, SchemaIdentWire};
use crate::core::error::{Error, Result};
use crate::core::schema_agreement::{SchemaAgreement, classify_wire_schema_agreement};

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

    fn push(&self, local_port: String, subscriber: Subscriber<ipc::Service, [u8], ()>) {
        // SAFETY: Only called from the processor's execution thread during wiring.
        unsafe {
            (*self.0.get()).push(PortBoundSubscriber {
                local_port,
                subscriber,
            });
        }
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
    /// Schema-declared upper bound on serialized frame size for this
    /// port — mirrors the value used by the publisher to size the
    /// iceoryx2 service's slot. The cdylib's `read_raw` queries this
    /// via the v2 `max_payload_for_port` vtable slot to allocate the
    /// receive buffer exactly, eliminating the v1 4 KiB-then-retry
    /// dance. Defaults to [`crate::iceoryx2::MAX_PAYLOAD_SIZE`] when
    /// the port is added without an explicit max (test paths;
    /// in-tree wire path overrides via
    /// [`InputMailboxesInner::set_port_max_payload_bytes`] from the
    /// compiler op).
    max_payload_bytes: usize,
    /// Schema-ident tag this consumer port expects every inbound frame to
    /// carry — the wire form of the port's declared input schema, set by the
    /// compiler op at wire time via
    /// [`InputMailboxesInner::set_port_expected_schema_ident`]. Default
    /// [`SchemaIdentWire::default`] (unset) means "no expectation" (an `any`
    /// port), which never triggers a mismatch. `read_raw` compares each
    /// frame's stamped tag against this and warns on a concrete mismatch.
    expected_schema_ident: SchemaIdentWire,
    /// Latched once `read_raw` observes an inbound tag that disagrees with
    /// [`Self::expected_schema_ident`]. Doubles as the warn-once guard (the
    /// realtime read path must not re-log every frame) and the test / graph
    /// observation surface via
    /// [`InputMailboxesInner::schema_mismatch_observed`].
    schema_mismatch_observed: AtomicBool,
}

/// Host-side inner state for input mailboxes. Owns the per-port
/// mailbox map plus the per-thread subscriber + listener. All
/// per-frame `receive_pending` + queue-pop work runs here.
///
/// Never crosses the plugin ABI. Held by the host via
/// `Arc<InputMailboxesInner>`; the cdylib's [`InputMailboxes`]
/// PluginAbiObject stores a separate `Arc::into_raw`-encoded strong
/// reference to the same inner.
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
    ///
    /// The port's `max_payload_bytes` is set to
    /// [`crate::iceoryx2::MAX_PAYLOAD_SIZE`] by default — the
    /// compiler op overrides it via
    /// [`set_port_max_payload_bytes`] at wire time based on the
    /// link's schema (`metadata.max_payload_bytes`). The cdylib's
    /// v2 `max_payload_for_port` vtable slot reads this value.
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
                max_payload_bytes: crate::iceoryx2::MAX_PAYLOAD_SIZE,
                expected_schema_ident: SchemaIdentWire::default(),
                schema_mismatch_observed: AtomicBool::new(false),
            },
        );
    }

    /// Override the schema-declared `metadata.max_payload_bytes`
    /// for a port that has already been added via [`add_port`].
    /// Called by the compiler op at wire time after computing the
    /// link's max via
    /// `embedded_schemas::max_payload_bytes_for_port_spec`. The
    /// cdylib's v2 `max_payload_for_port` vtable slot returns this
    /// value so its `read_raw` allocates exactly.
    ///
    /// No-op for unknown ports (the compiler op calls this only
    /// after [`add_port`] is known to have succeeded).
    pub fn set_port_max_payload_bytes(&self, port: &str, max_payload_bytes: usize) {
        if let Some(cfg) = self.ports.lock().get_mut(port) {
            cfg.max_payload_bytes = max_payload_bytes;
        }
    }

    /// Read the per-port `max_payload_bytes` set by the wire path.
    /// Returns `None` for unknown ports (the v2 vtable callback
    /// surfaces this as `0`, the caller's wiring-error sentinel).
    pub fn max_payload_for_port(&self, port: &str) -> Option<usize> {
        self.ports.lock().get(port).map(|cfg| cfg.max_payload_bytes)
    }

    /// Record the schema-ident tag this port expects inbound frames to carry.
    /// Called by the compiler op at wire time from the consumer's declared
    /// input schema; [`read_raw`] compares each frame's stamped tag against it
    /// and warns on a concrete mismatch. No-op for unknown ports.
    ///
    /// [`read_raw`]: Self::read_raw
    pub fn set_port_expected_schema_ident(&self, port: &str, expected: SchemaIdentWire) {
        if let Some(cfg) = self.ports.lock().get_mut(port) {
            cfg.expected_schema_ident = expected;
            cfg.schema_mismatch_observed.store(false, Ordering::Relaxed);
        }
    }

    /// Whether [`read_raw`] has observed at least one inbound frame whose
    /// stamped schema tag disagreed with the port's expected tag. Latches on
    /// the first mismatch (the read path warns once, not per frame). `false`
    /// for unknown ports.
    ///
    /// [`read_raw`]: Self::read_raw
    pub fn schema_mismatch_observed(&self, port: &str) -> bool {
        self.ports
            .lock()
            .get(port)
            .map(|cfg| cfg.schema_mismatch_observed.load(Ordering::Relaxed))
            .unwrap_or(false)
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
        subscriber: Subscriber<ipc::Service, [u8], ()>,
    ) {
        self.subscribers.push(local_port.to_string(), subscriber);
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

    /// Read raw bytes and timestamp from the given port without
    /// deserialization. Uses the port's read mode. Returns
    /// `Ok(Some((data, timestamp_ns)))` if data is available, `Ok(None)`
    /// if the mailbox is empty.
    pub fn read_raw(&self, port: &str) -> Result<Option<(Vec<u8>, i64)>> {
        self.receive_pending();

        let ports = self.ports.lock();
        let port_config = ports
            .get(port)
            .ok_or_else(|| Error::Link(format!("Unknown input port: {}", port)))?;

        let raw = match port_config.read_mode {
            ReadMode::SkipToLatest => port_config.mailbox.pop_latest(),
            ReadMode::ReadNextInOrder => port_config.mailbox.pop(),
        };

        match raw {
            Some(r) => {
                let header = FrameHeader::read_from_slice(&r);
                if classify_wire_schema_agreement(header.schema(), &port_config.expected_schema_ident)
                    == SchemaAgreement::Mismatch
                    && !port_config
                        .schema_mismatch_observed
                        .swap(true, Ordering::Relaxed)
                {
                    tracing::warn!(
                        port = port,
                        stamped_schema = %header.schema().render_joined(),
                        expected_schema = %port_config.expected_schema_ident.render_joined(),
                        "read_raw: inbound frame carries a schema tag that does not \
                         match this port's expected input schema (loose validation; \
                         warned once per port). A producer was re-typed, or the \
                         wrong producer is wired to this port."
                    );
                }
                let data = r[FRAME_HEADER_SIZE..FRAME_HEADER_SIZE + header.len as usize].to_vec();
                Ok(Some((data, header.timestamp_ns)))
            }
            None => Ok(None),
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
// InputMailboxes PluginAbiObject
// =============================================================================

/// Public input mailboxes PluginAbiObject. The macro emits
/// `pub inputs: InputMailboxes` on every processor struct that
/// declares input ports.
///
/// Layout-stable: every field is either a primitive or an opaque
/// pointer, so the cdylib's view of this type does not couple to
/// the host's [`InputMailboxesInner`] source layout.
///
/// `Clone` bumps the host-side `Arc<InputMailboxesInner>` strong
/// count via [`InputMailboxesVTable::clone_arc`]; `Drop` decrements
/// via [`InputMailboxesVTable::drop_arc`]. Both run in host-
/// compiled code regardless of which artifact holds this PluginAbiObject.
#[repr(C)]
pub struct InputMailboxes {
    /// Opaque handle. In host mode: `Arc::into_raw(Arc<InputMailboxesInner>)`.
    /// In cdylib mode: whatever the host hands via
    /// `ProcessorVTable::set_iceoryx2_resources`. Null on a
    /// freshly-constructed processor before
    /// `set_iceoryx2_resources` fires.
    pub(crate) handle: *const c_void,
    /// Static dispatch table. Host mode points at
    /// `&HOST_INPUT_MAILBOXES_VTABLE`; cdylib mode points at the
    /// host-installed pointer from
    /// `HostServices::input_mailboxes_vtable`. Null on
    /// freshly-constructed pre-wiring instances; methods short-
    /// circuit cleanly when the vtable is null.
    pub(crate) vtable: *const InputMailboxesVTable,
}

// SAFETY: `handle` points at an `Arc<InputMailboxesInner>` whose
// interior is Send+Sync (the inner uses parking_lot::Mutex for
// `ports` and the SendableSubscriber/SendableListener wrappers
// declare Send+Sync above). Refcount management crosses the plugin
// ABI through the host-installed refcount fn pointers; the
// underlying Arc bookkeeping runs in host-compiled code.
unsafe impl Send for InputMailboxes {}
unsafe impl Sync for InputMailboxes {}

impl InputMailboxes {
    /// Build a host-mode PluginAbiObject from an `Arc<InputMailboxesInner>`.
    /// The strong reference is consumed; the PluginAbiObject owns it for
    /// its lifetime and releases on Drop.
    pub fn from_inner_arc(inner: Arc<InputMailboxesInner>) -> Self {
        let handle = Arc::into_raw(inner) as *const c_void;
        let vtable = crate::core::plugin::host_services::host_input_mailboxes_vtable();
        Self { handle, vtable }
    }

    /// Build an empty pre-wiring PluginAbiObject with null handle and
    /// null vtable. The host patches in real values via
    /// `ProcessorVTable::set_iceoryx2_resources`.
    pub fn empty() -> Self {
        Self {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
        }
    }

    /// Raw-pointer construction used by
    /// `ProcessorVTable::set_iceoryx2_resources` host wiring.
    pub(crate) fn from_raw_parts(
        handle: *const c_void,
        vtable: *const InputMailboxesVTable,
    ) -> Self {
        Self { handle, vtable }
    }

    /// Returns true iff this PluginAbiObject has been wired to a real
    /// host-allocated inner.
    pub fn is_configured(&self) -> bool {
        !self.handle.is_null() && !self.vtable.is_null()
    }

    /// Borrow the host-side `Arc<InputMailboxesInner>` this
    /// PluginAbiObject points at. Returns `None` for unwired PluginAbiObjects.
    /// Bumps the strong count via the vtable's `clone_arc`; the
    /// returned Arc balances with one Drop on the inner.
    pub fn inner_arc(&self) -> Option<Arc<InputMailboxesInner>> {
        if !self.is_configured() {
            return None;
        }
        // SAFETY: handle came from Arc::into_raw; bumping the
        // strong count via the vtable's clone_arc gives us a fresh
        // owning reference we can reconstruct as Arc::from_raw.
        unsafe {
            let cloned_handle = ((*self.vtable).clone_arc)(self.handle);
            if cloned_handle.is_null() {
                return None;
            }
            Some(Arc::from_raw(cloned_handle as *const InputMailboxesInner))
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
    /// Allocates `out_buf` to the schema-declared
    /// `metadata.max_payload_bytes` for the port (queried via the
    /// v2 `max_payload_for_port` vtable slot). The iceoryx2 service
    /// is sized by the same bound on the publisher side, so no
    /// inbound frame can exceed `out_cap` — truncation is
    /// structurally impossible and surfaces as a hard error
    /// (protocol violation).
    pub fn read_raw(&self, port: &str) -> Result<Option<(Vec<u8>, i64)>> {
        if !self.is_configured() {
            return Ok(None);
        }

        // SAFETY: vtable + handle are non-null per is_configured().
        let max_payload = unsafe {
            ((*self.vtable).max_payload_for_port)(self.handle, port.as_ptr(), port.len())
        };
        if max_payload == 0 {
            return Err(Error::Link(format!(
                "InputMailboxes::read_raw(port='{}'): max_payload_for_port \
                 returned 0 — port is not registered (wiring error: was \
                 add_port called on this side of the link?)",
                port
            )));
        }

        let mut buf = vec![0u8; max_payload];
        let mut out_len = 0usize;
        let mut out_timestamp = 0i64;
        let mut has_data = false;
        let mut err_buf = [0u8; 256];
        let mut err_len = 0usize;
        // SAFETY: vtable + handle are non-null per is_configured().
        let rc = unsafe {
            ((*self.vtable).read_raw)(
                self.handle,
                port.as_ptr(),
                port.len(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut out_len as *mut usize,
                &mut out_timestamp as *mut i64,
                &mut has_data as *mut bool,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if rc != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::Link(format!(
                "InputMailboxes::read_raw(port='{}') failed: {}",
                port, msg
            )));
        }
        if !has_data {
            return Ok(None);
        }
        // Truncation is impossible by service-creation invariant
        // (publisher can't loan slot > max_payload, and we sized
        // our buffer to max_payload). Defensive check so a future
        // protocol mismatch surfaces loudly rather than silently
        // corrupting downstream deserialization.
        if out_len > buf.len() {
            return Err(Error::Link(format!(
                "InputMailboxes::read_raw(port='{}'): host returned frame \
                 of {} bytes exceeding the queried max_payload {} — \
                 protocol violation",
                port,
                out_len,
                buf.len()
            )));
        }
        buf.truncate(out_len);
        Ok(Some((buf, out_timestamp)))
    }

    /// Check if a port has any payloads available.
    pub fn has_data(&self, port: &str) -> bool {
        if !self.is_configured() {
            return false;
        }
        // SAFETY: vtable + handle are non-null per is_configured().
        unsafe { ((*self.vtable).has_data)(self.handle, port.as_ptr(), port.len()) }
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
        // SAFETY: vtable + handle are non-null per is_configured().
        let cloned_handle = unsafe { ((*self.vtable).clone_arc)(self.handle) };
        Self {
            handle: cloned_handle,
            vtable: self.vtable,
        }
    }
}

impl Drop for InputMailboxes {
    fn drop(&mut self) {
        if !self.is_configured() {
            return;
        }
        // SAFETY: vtable + handle are non-null per is_configured().
        unsafe {
            ((*self.vtable).drop_arc)(self.handle);
        }
        self.handle = std::ptr::null();
        self.vtable = std::ptr::null();
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
        let schema_ident = streamlib_ipc_types::SchemaIdentWire::from_segments(
            "tatolab",
            "test",
            "AnyPortHasData",
            1,
            0,
            0,
        )
        .expect("schema ident");
        let make_frame = |port: &str| -> Vec<u8> {
            let mut buf = vec![0u8; FRAME_HEADER_SIZE + 4];
            let header = FrameHeader::new(port, schema_ident, 0, 4).expect("port fits PortKey");
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

    fn frame_with_schema(port: &str, schema: SchemaIdentWire) -> Vec<u8> {
        let mut buf = vec![0u8; FRAME_HEADER_SIZE + 4];
        let header = FrameHeader::new(port, schema, 0, 4).expect("port fits PortKey");
        header.write_to_slice(&mut buf);
        buf[FRAME_HEADER_SIZE..].copy_from_slice(&[9, 8, 7, 6]);
        buf
    }

    /// Runtime read-side schema check (#1430): a frame whose stamped schema
    /// tag disagrees with the port's expected input schema is observed as a
    /// mismatch, but the read still succeeds (loose-but-observed posture).
    ///
    /// Revert lock: mentally revert `read_raw`'s tag comparison (stop reading
    /// `header.schema()`) and `schema_mismatch_observed` stays `false` — this
    /// asserts `true`, so the test fails, catching a regression back to the
    /// pre-#1430 "stamped-but-unread" state.
    #[test]
    fn read_raw_observes_schema_tag_mismatch_but_still_delivers() {
        let mailboxes = InputMailboxesInner::new();
        mailboxes.add_port("in", 64, ReadMode::ReadNextInOrder);

        let expected =
            SchemaIdentWire::from_segments("tatolab", "core", "VideoFrame", 1, 0, 0).unwrap();
        mailboxes.set_port_expected_schema_ident("in", expected);

        let stamped_wrong =
            SchemaIdentWire::from_segments("tatolab", "core", "AudioFrame", 1, 0, 0).unwrap();
        assert!(mailboxes.route(frame_with_schema("in", stamped_wrong)));

        let read = mailboxes
            .read_raw("in")
            .expect("read_raw must succeed under loose validation")
            .expect("a frame is queued");
        assert_eq!(read.0, vec![9, 8, 7, 6], "payload delivered despite mismatch");
        assert!(
            mailboxes.schema_mismatch_observed("in"),
            "the disagreeing tag must be observed as a mismatch",
        );
    }

    /// A frame whose stamped tag matches the port's expected schema is NOT
    /// flagged; the wildcard cases (unset expected, or unset stamp) are
    /// likewise silent.
    #[test]
    fn read_raw_is_silent_on_matching_or_wildcard_schema() {
        let matching = SchemaIdentWire::from_segments("tatolab", "core", "VideoFrame", 1, 0, 0)
            .unwrap();

        // Exact match → no mismatch.
        let mb_match = InputMailboxesInner::new();
        mb_match.add_port("in", 64, ReadMode::ReadNextInOrder);
        mb_match.set_port_expected_schema_ident("in", matching);
        assert!(mb_match.route(frame_with_schema("in", matching)));
        mb_match.read_raw("in").unwrap().unwrap();
        assert!(!mb_match.schema_mismatch_observed("in"));

        // Unset expected (an `any` port) accepts any stamped tag.
        let mb_any = InputMailboxesInner::new();
        mb_any.add_port("in", 64, ReadMode::ReadNextInOrder);
        assert!(mb_any.route(frame_with_schema("in", matching)));
        mb_any.read_raw("in").unwrap().unwrap();
        assert!(!mb_any.schema_mismatch_observed("in"));
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
        let schema =
            SchemaIdentWire::from_segments("tatolab", "core", "VideoFrame", 1, 0, 0).unwrap();

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
            FrameHeader::new(source_port, schema, 0, data.len() as u32)
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
        mailboxes.add_channel_subscriber("in", sub_a);
        mailboxes.add_channel_subscriber("in", sub_b);

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

    /// Empty (unwired) PluginAbiObject should return Ok(None) from read_raw
    /// rather than crash. Mentally revert the is_configured guard
    /// and the test panics dereferencing a null vtable.
    #[test]
    fn empty_mailboxes_returns_none_cleanly() {
        let mb = InputMailboxes::empty();
        assert!(!mb.is_configured());
        assert!(mb.read_raw("any").unwrap().is_none());
        assert!(!mb.has_data("any"));
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
