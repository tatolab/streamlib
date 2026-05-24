// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Input mailboxes for receiving frames from upstream processors.
//!
//! # Two-type split: β-shape vs. inner
//!
//! Issue #894 retires the last shared-Rust-type plugin-ABI crossing
//! by splitting this module's public surface into two types:
//!
//! - [`InputMailboxesInner`] holds the actual state — the
//!   `HashMap<port, PortConfig>` of per-port mailboxes plus the
//!   thread-local `Subscriber` and `Listener` wrappers. All
//!   per-frame `receive_pending` + mailbox push/pop work runs
//!   here; only the host DSO references this type directly.
//! - [`InputMailboxes`] is the public `#[repr(C)] { handle, vtable }`
//!   β-shape that processor structs hold via the macro-emitted
//!   `inputs: InputMailboxes` field. From inside `process()` the
//!   cdylib reaches input data exclusively through `read` /
//!   `read_raw` / `has_data` on this β-shape; the vtable dispatches
//!   to the host-allocated inner.
//!
//! Host-side wiring code that needs to mutate the inner
//! (`add_port`, `set_subscriber`, `set_listener`, `listener_fd`,
//! `drain_listener`, etc.) operates on `Arc<InputMailboxesInner>`
//! directly via the methods declared on the inner type — no
//! β-shape, no FFI hop.

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;

use iceoryx2::port::listener::Listener;
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::*;
use serde::de::DeserializeOwned;
use streamlib_plugin_abi::InputMailboxesVTable;

use super::mailbox::PortMailbox;
use super::read_mode::ReadMode;
use super::{FrameHeader, FRAME_HEADER_SIZE};
use crate::core::error::{Error, Result};

/// Thread-local subscriber wrapper.
///
/// # Safety
/// This wrapper is safe to send between threads because:
/// 1. The Subscriber is only ever set AFTER the processor is spawned on its execution thread
/// 2. Once set, the Subscriber is only accessed from that same thread
/// 3. The wrapper starts with `None` and is populated during wiring on the target thread
struct SendableSubscriber(UnsafeCell<Option<Subscriber<ipc::Service, [u8], ()>>>);

// SAFETY: The Subscriber is only accessed from a single thread after being set.
// The processor lifecycle ensures that:
// 1. InputMailboxesInner is created with subscriber = None (safe to send)
// 2. After spawn, the processor is on its execution thread
// 3. set_subscriber() is called from that execution thread during wiring
// 4. All subsequent access is from the same thread
unsafe impl Send for SendableSubscriber {}
unsafe impl Sync for SendableSubscriber {}

impl SendableSubscriber {
    fn new() -> Self {
        Self(UnsafeCell::new(None))
    }

    fn set(&self, subscriber: Subscriber<ipc::Service, [u8], ()>) {
        // SAFETY: Only called from the processor's execution thread after spawn
        unsafe {
            *self.0.get() = Some(subscriber);
        }
    }

    fn get(&self) -> Option<&Subscriber<ipc::Service, [u8], ()>> {
        // SAFETY: Only called from the processor's execution thread
        unsafe { (*self.0.get()).as_ref() }
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
}

/// Host-side inner state for input mailboxes. Owns the per-port
/// mailbox map plus the per-thread subscriber + listener. All
/// per-frame `receive_pending` + queue-pop work runs here.
///
/// Never crosses the cdylib boundary. Held by the host via
/// `Arc<InputMailboxesInner>`; the cdylib's [`InputMailboxes`]
/// β-shape stores a separate `Arc::into_raw`-encoded strong
/// reference to the same inner.
pub struct InputMailboxesInner {
    ports: parking_lot::Mutex<HashMap<String, PortConfig>>,
    subscriber: SendableSubscriber,
    listener: SendableListener,
}

impl InputMailboxesInner {
    /// Create a new empty inner.
    pub fn new() -> Self {
        Self {
            ports: parking_lot::Mutex::new(HashMap::new()),
            subscriber: SendableSubscriber::new(),
            listener: SendableListener::new(),
        }
    }

    /// Check if a port has already been configured.
    pub fn has_port(&self, port: &str) -> bool {
        self.ports.lock().contains_key(port)
    }

    /// Add a mailbox for the given port with the specified buffer size and read mode.
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
            },
        );
    }

    /// Check if a subscriber has already been configured.
    pub fn has_subscriber(&self) -> bool {
        self.subscriber.get().is_some()
    }

    /// Set the iceoryx2 Subscriber for receiving payloads.
    ///
    /// Note: This should only be called from the processor's execution thread.
    pub fn set_subscriber(&self, subscriber: Subscriber<ipc::Service, [u8], ()>) {
        self.subscriber.set(subscriber);
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
                tracing::trace!("InputMailboxes: drain_listener try_wait_all failed: {:?}", e);
            }
        }
    }

    /// Receive all pending payloads from the iceoryx2 Subscriber and route them to mailboxes.
    ///
    /// This is called automatically by `read()` and `has_data()`, but can be called
    /// explicitly if needed.
    ///
    /// Note: This should only be called from the thread that owns the subscriber.
    pub fn receive_pending(&self) {
        let Some(subscriber) = self.subscriber.get() else {
            return;
        };

        // Receive [u8] slices and route to mailboxes
        loop {
            match subscriber.receive() {
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
                    let port_name = FrameHeader::read_port_from_slice(slice);
                    let ports = self.ports.lock();
                    if let Some(port_config) = ports.get(port_name) {
                        port_config.mailbox.push(slice.to_vec());
                    } else {
                        tracing::warn!("InputMailboxes: received sample but no matching port");
                    }
                }
                Ok(None) => break, // no more samples
                Err(e) => {
                    tracing::error!("InputMailboxes: subscriber.receive() FAILED: {:?}", e);
                    break;
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

    /// Route a raw frame slice to the appropriate mailbox based on port_key in the header.
    ///
    /// Returns true if the payload was routed, false if no matching mailbox exists.
    /// Thread-safe: can be called from any thread.
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
// InputMailboxes β-shape
// =============================================================================

/// Public input mailboxes β-shape. The macro emits
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
/// compiled code regardless of which DSO holds this β-shape.
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
// declare Send+Sync above). Refcount management crosses the cdylib
// boundary through the host-installed refcount fn pointers; the
// underlying Arc bookkeeping runs in host-compiled code.
unsafe impl Send for InputMailboxes {}
unsafe impl Sync for InputMailboxes {}

impl InputMailboxes {
    /// Build a host-mode β-shape from an `Arc<InputMailboxesInner>`.
    /// The strong reference is consumed; the β-shape owns it for
    /// its lifetime and releases on Drop.
    pub fn from_inner_arc(inner: Arc<InputMailboxesInner>) -> Self {
        let handle = Arc::into_raw(inner) as *const c_void;
        let vtable = crate::core::plugin::host_services::host_input_mailboxes_vtable();
        Self { handle, vtable }
    }

    /// Build an empty pre-wiring β-shape with null handle and
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

    /// Returns true iff this β-shape has been wired to a real
    /// host-allocated inner.
    pub fn is_configured(&self) -> bool {
        !self.handle.is_null() && !self.vtable.is_null()
    }

    /// Borrow the host-side `Arc<InputMailboxesInner>` this
    /// β-shape points at. Returns `None` for unwired β-shapes.
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
        let raw = self.read_raw(port)?.ok_or_else(|| {
            Error::Link(format!("No data available on port: {}", port))
        })?;
        rmp_serde::from_slice(&raw.0)
            .map_err(|e| Error::Link(format!("Failed to deserialize frame: {}", e)))
    }

    /// Read raw bytes and timestamp from the given port without
    /// deserialization. Returns `Ok(Some((data, timestamp_ns)))` on
    /// success, `Ok(None)` when the mailbox is empty.
    pub fn read_raw(&self, port: &str) -> Result<Option<(Vec<u8>, i64)>> {
        if !self.is_configured() {
            return Ok(None);
        }

        // Start with a buffer sized for typical frames (4 KiB —
        // common audio/control payload size). On truncation we
        // resize to the required size returned via `*out_len` and
        // retry; iceoryx2's `MAX_PAYLOAD_SIZE` bounds the worst
        // case.
        let mut buf = vec![0u8; 4 * 1024];
        loop {
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
                let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                    .into_owned();
                return Err(Error::Link(format!(
                    "InputMailboxes::read_raw(port='{}') failed: {}",
                    port, msg
                )));
            }
            if !has_data {
                return Ok(None);
            }
            if out_len > buf.len() {
                // Caller's buffer too small; resize and retry. The
                // host's `read_raw` host wrapper guarantees the
                // frame is NOT consumed on truncation, so the retry
                // sees the same frame.
                buf = vec![0u8; out_len];
                continue;
            }
            buf.truncate(out_len);
            return Ok(Some((buf, out_timestamp)));
        }
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

        assert!(!mailboxes.any_port_has_data(), "empty mailboxes report no data");

        // Build a minimal valid frame for `port_a` and route it directly
        // — bypasses the iceoryx2 subscriber, exercising only the
        // mailbox-depth accounting.
        let schema_ident = streamlib_ipc_types::SchemaIdentWire::from_segments(
            "tatolab", "test", "AnyPortHasData", 1, 0, 0,
        )
        .expect("schema ident");
        let make_frame = |port: &str| -> Vec<u8> {
            let mut buf = vec![0u8; FRAME_HEADER_SIZE + 4];
            let header = FrameHeader::new(port, schema_ident, 0, 4);
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
        assert!(mailboxes.any_port_has_data(), "five queued frames must report has_data");

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

    /// Empty (unwired) β-shape should return Ok(None) from read_raw
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
