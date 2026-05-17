// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Input mailboxes for receiving frames from upstream processors.

use std::cell::UnsafeCell;
use std::collections::HashMap;

use iceoryx2::port::listener::Listener;
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::*;
use serde::de::DeserializeOwned;

use super::mailbox::PortMailbox;
use super::read_mode::ReadMode;
use super::{FrameHeader, FRAME_HEADER_SIZE};
use crate::core::error::{Result, Error};

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
// 1. InputMailboxes is created with subscriber = None (safe to send)
// 2. After spawn, the processor is on its execution thread
// 3. set_subscriber() is called from that execution thread during wiring
// 4. All subsequent access is from the same thread
unsafe impl Send for SendableSubscriber {}

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
struct PortConfig {
    mailbox: PortMailbox,
    read_mode: ReadMode,
}

/// Collection of input mailboxes, one per input port.
///
/// The mailsorter task routes incoming payloads to the appropriate mailbox
/// based on the port_key in the payload.
///
/// Thread-safe: All read operations can be called from any thread.
/// The subscriber is still single-threaded (set once, used from one thread).
pub struct InputMailboxes {
    ports: HashMap<String, PortConfig>,
    subscriber: SendableSubscriber,
    listener: SendableListener,
}

impl InputMailboxes {
    /// Create a new empty collection of input mailboxes.
    pub fn new() -> Self {
        Self {
            ports: HashMap::new(),
            subscriber: SendableSubscriber::new(),
            listener: SendableListener::new(),
        }
    }

    /// Check if a port has already been configured.
    pub fn has_port(&self, port: &str) -> bool {
        self.ports.contains_key(port)
    }

    /// Add a mailbox for the given port with the specified buffer size and read mode.
    pub fn add_port(&mut self, port: &str, buffer_size: usize, read_mode: ReadMode) {
        tracing::debug!(
            port = port,
            buffer_size = buffer_size,
            read_mode = ?read_mode,
            "InputMailboxes: add_port"
        );
        self.ports.insert(
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
    /// MUST stop using it before [`InputMailboxes`] is dropped. Suitable for
    /// registering with `epoll_ctl(EPOLL_CTL_ADD)` or `select` from the
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
                    if let Some(port_config) = self.ports.get(port_name) {
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

    /// Read and deserialize a frame from the given port.
    ///
    /// Uses the port's read mode to determine consumption strategy:
    /// - `SkipToLatest`: Drains buffer, returns only the newest frame (video)
    /// - `ReadNextInOrder`: Returns oldest frame in FIFO order (audio)
    ///
    /// This first receives any pending data from the iceoryx2 Subscriber,
    /// routes it to the appropriate mailboxes, then reads from the requested port.
    ///
    /// Thread-safe for the pop operation, but receive_pending should only be
    /// called from the subscriber's thread.
    pub fn read<T: DeserializeOwned>(&self, port: &str) -> Result<T> {
        self.receive_pending();

        let port_config = self
            .ports
            .get(port)
            .ok_or_else(|| Error::Link(format!("Unknown input port: {}", port)))?;

        let raw = match port_config.read_mode {
            ReadMode::SkipToLatest => port_config.mailbox.pop_latest(),
            ReadMode::ReadNextInOrder => port_config.mailbox.pop(),
        }
        .ok_or_else(|| Error::Link(format!("No data available on port: {}", port)))?;

        let header = FrameHeader::read_from_slice(&raw);
        let data = &raw[FRAME_HEADER_SIZE..FRAME_HEADER_SIZE + header.len as usize];
        rmp_serde::from_slice(data)
            .map_err(|e| Error::Link(format!("Failed to deserialize frame: {}", e)))
    }

    /// Read raw bytes and timestamp from the given port without deserialization.
    ///
    /// Uses the port's read mode (same as [`read`]). Returns `Ok(Some((data, timestamp_ns)))`
    /// if data is available, `Ok(None)` if the mailbox is empty.
    pub fn read_raw(&self, port: &str) -> Result<Option<(Vec<u8>, i64)>> {
        self.receive_pending();

        let port_config = self
            .ports
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

    /// Check if a port has any payloads available.
    ///
    /// This first receives any pending data from the iceoryx2 Subscriber.
    pub fn has_data(&self, port: &str) -> bool {
        self.receive_pending();
        self.ports
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
        self.ports.values().any(|p| !p.mailbox.is_empty())
    }

    /// Drain all raw frame slices from the given port's mailbox.
    pub fn drain(&self, port: &str) -> impl Iterator<Item = Vec<u8>> + '_ {
        self.ports
            .get(port)
            .into_iter()
            .flat_map(|p| p.mailbox.drain())
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
        if let Some(port_config) = self.ports.get(port) {
            port_config.mailbox.push(raw);
            true
        } else {
            false
        }
    }

    /// Get the list of configured port names.
    pub fn port_names(&self) -> impl Iterator<Item = &str> {
        self.ports.keys().map(|s| s.as_str())
    }
}

impl Default for InputMailboxes {
    fn default() -> Self {
        Self::new()
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

        let mailboxes = InputMailboxes::new();
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
    /// configured ports, draining iceoryx2 samples into the per-port
    /// mailboxes first. The reactive runner relies on this method to
    /// know whether more `process()` calls are needed after a single
    /// epoll wake — iceoryx2's Event service coalesces multiple
    /// notify()s on the same EventId into one fd-readable transition,
    /// so the runner cannot trust "one wake = one event". Mentally
    /// reverting `any_port_has_data` to always return `false` makes
    /// this test fail at every depth check below.
    #[test]
    fn any_port_has_data_reflects_total_queued_depth() {
        let mut mailboxes = InputMailboxes::new();
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
}
