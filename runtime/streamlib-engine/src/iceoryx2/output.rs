// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Output writer for sending frames to downstream processors.
//!
//! # Two-type split: PluginAbiObject vs. inner
//!
//! Issue #894 retires the last shared-Rust-type plugin ABI crossing
//! by splitting this module's public surface into two types:
//!
//! - [`OutputWriterInner`] holds the actual state — the
//!   `Mutex<HashMap<port, Vec<DownstreamConnection>>>` and the
//!   iceoryx2 publish + notify logic. It runs entirely in the
//!   host; cdylib code never references this type directly.
//! - [`OutputWriter`] is the public `#[repr(C)] { handle, vtable }`
//!   PluginAbiObject that processor structs hold via the macro-emitted
//!   `outputs: OutputWriter` field. In host mode the vtable
//!   resolves to the host's static
//!   `HOST_OUTPUT_WRITER_VTABLE`; in cdylib mode it points at the
//!   host-installed pointer from
//!   `HostServices::output_writer_vtable`. Either way the methods
//!   on the PluginAbiObject (`write`, `write_raw`, `has_port`, `clone`,
//!   `drop`) dispatch through the vtable to the host-allocated
//!   inner.
//!
//! Host-side code that needs to mutate the inner (e.g. compiler ops
//! adding downstream connections at wiring time) operates on
//! `Arc<OutputWriterInner>` directly via
//! [`OutputWriterInner::add_connection`] — no PluginAbiObject, no plugin ABI hop.
//! The cdylib's per-frame `write` calls cross extern "C" exactly
//! once per emit (sub-microsecond on amd64; see the PR microbench
//! for issue #894).

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;

use iceoryx2::port::notifier::Notifier;
use iceoryx2::port::publisher::Publisher;
use iceoryx2::prelude::*;
use parking_lot::Mutex;
use serde::Serialize;
use streamlib_plugin_abi::OutputWriterVTable;

use super::{FRAME_HEADER_SIZE, FrameHeader, SchemaIdentWire};
use crate::core::error::{Error, Result};
use crate::core::media_clock::MediaClock;

/// One source output port's channel egress: the single channel publisher
/// (a channel carries exactly one publisher — see
/// [`streamlib_ipc_types::MAX_PUBLISHERS_PER_CHANNEL`]), the structured schema
/// tag it stamps into every [`FrameHeader`], and one notifier per destination.
///
/// The transport inversion (#1419): one source output port maps to one channel,
/// so a single zero-copy loan reaches every subscriber. The per-destination
/// notifiers stay separate because each destination keeps its own listener-fd
/// (the notify service is destination-keyed for fd-multiplexed wakeups); the
/// data itself is published ONCE.
struct ChannelEgress {
    schema_ident: SchemaIdentWire,
    publisher: Publisher<ipc::Service, [u8], ()>,
    notifiers: Vec<Notifier<ipc::Service>>,
}

/// Host-side inner state for an output writer. Owns the per-output-port
/// channel publisher and its destination notifiers; all per-frame publish +
/// notify work runs here.
///
/// Never crosses the plugin ABI. Held by the host via
/// `Arc<OutputWriterInner>`; the cdylib's [`OutputWriter`] PluginAbiObject
/// stores a separate `Arc::into_raw`-encoded strong reference to
/// the same inner.
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
        self.channels.lock().contains_key(output_port)
    }

    /// Install the single channel publisher for an output port.
    ///
    /// `schema_ident` is the structured wire identifier stamped into every
    /// [`FrameHeader`] this port publishes. Callers build it once at wiring time
    /// from the port's structured `PortSchemaSpec` via
    /// [`SchemaIdentWire::from_segments`] — no parser runs on the per-frame hot
    /// path. Called once per output port (the first link out of it); a second
    /// call replaces the publisher, which the wiring op avoids via
    /// [`Self::has_channel_publisher`].
    pub fn set_channel_publisher(
        &self,
        output_port: &str,
        schema_ident: SchemaIdentWire,
        publisher: Publisher<ipc::Service, [u8], ()>,
    ) {
        self.channels.lock().insert(
            output_port.to_string(),
            ChannelEgress {
                schema_ident,
                publisher,
                notifiers: Vec::new(),
            },
        );
    }

    /// Append a destination notifier to an output port's channel.
    ///
    /// One notifier per `connect()` link out of this port — each wakes a distinct
    /// destination's listener fd. No-op (the notifier is dropped) if the channel
    /// publisher has not been installed yet, which the wiring op never does.
    pub fn add_channel_notifier(&self, output_port: &str, notifier: Notifier<ipc::Service>) {
        if let Some(egress) = self.channels.lock().get_mut(output_port) {
            egress.notifiers.push(notifier);
        }
    }

    /// Write raw bytes to the specified output port without serialization.
    ///
    /// The data is assumed to be pre-serialized (e.g., msgpack from a
    /// subprocess bridge OR the PluginAbiObject's serialize-then-plugin-ABI path).
    /// One zero-copy loan reaches every channel subscriber; the frame is built
    /// and sent ONCE, then every destination notifier is signalled.
    pub fn write_raw(&self, port: &str, data: &[u8], timestamp_ns: i64) -> Result<()> {
        let channels = self.channels.lock();
        let egress = channels
            .get(port)
            .ok_or_else(|| Error::Link(format!("Unknown output port: {}", port)))?;

        let total_len = FRAME_HEADER_SIZE + data.len();
        let mut frame = vec![0u8; total_len];
        FrameHeader::new(port, egress.schema_ident, timestamp_ns, data.len() as u32)
            .map_err(|e| Error::Link(format!("output port '{}': {}", port, e)))?
            .write_to_slice(&mut frame[..FRAME_HEADER_SIZE]);
        frame[FRAME_HEADER_SIZE..].copy_from_slice(data);

        let sample = egress
            .publisher
            .loan_slice_uninit(total_len)
            .map_err(|e| Error::Link(format!("Failed to loan slice: {:?}", e)))?;
        let sample = sample.write_from_slice(&frame);
        sample
            .send()
            .map_err(|e| Error::Link(format!("Failed to send sample: {:?}", e)))?;

        // Wake every downstream listener fd. notify() may transiently fail
        // (e.g. a listener not yet created) — log and continue rather than
        // failing the publish; the data is already in shared memory and the
        // next send() will wake the listener anyway.
        for notifier in &egress.notifiers {
            if let Err(e) = notifier.notify() {
                tracing::trace!(
                    "OutputWriter: notify() failed for port '{}': {:?}",
                    port,
                    e
                );
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
// OutputWriter PluginAbiObject
// =============================================================================

/// Public output writer PluginAbiObject. The macro emits
/// `pub outputs: OutputWriter` on every processor struct that
/// declares output ports.
///
/// Layout-stable: every field is either a primitive or an opaque
/// pointer, so the cdylib's view of this type does not couple to
/// the host's [`OutputWriterInner`] source layout.
///
/// `Clone` bumps the host-side `Arc<OutputWriterInner>` strong
/// count via [`OutputWriterVTable::clone_arc`]; `Drop` decrements
/// via [`OutputWriterVTable::drop_arc`]. Both run in host-compiled
/// code regardless of which artifact holds this PluginAbiObject.
#[repr(C)]
pub struct OutputWriter {
    /// Opaque handle. In host mode: `Arc::into_raw(Arc<OutputWriterInner>)`.
    /// In cdylib mode: whatever the host hands via
    /// `ProcessorVTable::set_iceoryx2_resources` (which is also
    /// `Arc::into_raw`-shaped, so the wire contract is the same).
    /// Null on a freshly-constructed processor before
    /// `set_iceoryx2_resources` fires.
    pub(crate) handle: *const c_void,
    /// Static dispatch table. Host mode points at
    /// `&HOST_OUTPUT_WRITER_VTABLE`; cdylib mode points at the
    /// host-installed pointer from
    /// `HostServices::output_writer_vtable`. Null on
    /// freshly-constructed pre-wiring instances; methods short-
    /// circuit to errors when the vtable is null.
    pub(crate) vtable: *const OutputWriterVTable,
}

// SAFETY: `handle` points at an `Arc<OutputWriterInner>` whose
// interior is Send+Sync (OutputWriterInner declares both above).
// Refcount management crosses the plugin ABI through the
// vtable but the underlying Arc bookkeeping runs in host-compiled
// code regardless.
unsafe impl Send for OutputWriter {}
unsafe impl Sync for OutputWriter {}

impl OutputWriter {
    /// Build a host-mode PluginAbiObject from an `Arc<OutputWriterInner>`.
    /// The strong reference is consumed; the PluginAbiObject owns it for
    /// its lifetime and releases on Drop.
    ///
    /// Engine-only — used by the host's processor wiring path
    /// (`ProcessorInstanceFactory::install_iceoryx2_resources`) and
    /// by the macro-emitted `from_config` initializer when no
    /// outputs are declared (an empty inner is used).
    pub fn from_inner_arc(inner: Arc<OutputWriterInner>) -> Self {
        let handle = Arc::into_raw(inner) as *const c_void;
        let vtable = crate::core::plugin::host_services::host_output_writer_vtable();
        Self { handle, vtable }
    }

    /// Build an empty pre-wiring PluginAbiObject with null handle and
    /// null vtable. The host patches in real values via
    /// `ProcessorVTable::set_iceoryx2_resources` before any
    /// downstream connection wiring runs. Method calls on the
    /// empty PluginAbiObject return cleanly with no-op semantics
    /// (matches today's pre-wiring behaviour of an empty
    /// `OutputWriter::new()`).
    pub fn empty() -> Self {
        Self {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
        }
    }

    /// Raw-pointer construction used by
    /// `ProcessorVTable::set_iceoryx2_resources` host wiring to
    /// patch an existing PluginAbiObject's fields without owning a typed
    /// `Arc<OutputWriterInner>`.
    pub(crate) fn from_raw_parts(handle: *const c_void, vtable: *const OutputWriterVTable) -> Self {
        Self { handle, vtable }
    }

    /// Returns true iff this PluginAbiObject has been wired to a real
    /// host-allocated inner.
    pub fn is_configured(&self) -> bool {
        !self.handle.is_null() && !self.vtable.is_null()
    }

    /// Borrow the host-side `Arc<OutputWriterInner>` this PluginAbiObject
    /// points at. Returns `None` for unwired PluginAbiObjects. Bumps the
    /// strong count via the vtable's `clone_arc`; the returned
    /// Arc balances with one Drop on the inner.
    ///
    /// Engine-only (used by the macro-emitted
    /// `iceoryx2_output_writer_inner` trait method to expose the
    /// host's wiring path to compiler ops). Cdylib code can call
    /// this too but the host's inner reach is always preferred for
    /// per-frame mutation since it skips the vtable hop.
    pub fn inner_arc(&self) -> Option<Arc<OutputWriterInner>> {
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
            Some(Arc::from_raw(cloned_handle as *const OutputWriterInner))
        }
    }

    /// Write a frame to the specified output port.
    ///
    /// Source-compatible with the pre-#894 `OutputWriter::write` —
    /// the cdylib serializes `T` to msgpack in its own plugin then
    /// crosses extern "C" once with the bytes. Thread-safe.
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
        if !self.is_configured() {
            return Err(Error::Link(format!(
                "OutputWriter not wired (port='{}'): host has not yet \
                 installed iceoryx2 resources on this processor instance",
                port
            )));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len = 0usize;
        // SAFETY: vtable + handle are non-null per is_configured().
        // The fn pointer's lifetime is tied to the host's process
        // (the vtable lives in static memory). The err_buf is a
        // local stack allocation we own.
        let rc = unsafe {
            ((*self.vtable).write_raw)(
                self.handle,
                port.as_ptr(),
                port.len(),
                data.as_ptr(),
                data.len(),
                timestamp_ns,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::Link(format!(
                "OutputWriter::write_raw(port='{}') failed: {}",
                port, msg
            )))
        }
    }

    /// Check if a port is configured.
    pub fn has_port(&self, port: &str) -> bool {
        if !self.is_configured() {
            return false;
        }
        // SAFETY: vtable + handle are non-null per is_configured().
        unsafe { ((*self.vtable).has_port)(self.handle, port.as_ptr(), port.len()) }
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
        // SAFETY: vtable + handle are non-null per is_configured().
        let cloned_handle = unsafe { ((*self.vtable).clone_arc)(self.handle) };
        Self {
            handle: cloned_handle,
            vtable: self.vtable,
        }
    }
}

impl Drop for OutputWriter {
    fn drop(&mut self) {
        if !self.is_configured() {
            return;
        }
        // SAFETY: vtable + handle are non-null per is_configured().
        // After dispatch, null out the local fields so a double-
        // drop becomes a no-op (mirrors the Texture / PixelBuffer
        // PluginAbiObject pattern).
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

    /// Each test gets a unique service-name prefix so parallel invocations
    /// don't collide on iceoryx2's machine-global `/dev/shm` namespace.
    fn unique_suffix(tag: &str) -> String {
        format!(
            "test/output/{}/{}/{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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
        let schema_ident =
            SchemaIdentWire::from_segments("tatolab", "core", "VideoFrame", 1, 0, 0).unwrap();
        inner.set_channel_publisher("out", schema_ident, publisher);
        inner.add_channel_notifier("out", notifier);

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

    /// Empty (unwired) writers should fail cleanly rather than crash.
    /// Mentally revert the `is_configured()` guard in `write_raw` and
    /// the test segfaults dereferencing the null vtable.
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

    /// Clone bumps the strong count via the vtable; both clones drop
    /// independently. Mentally revert the `clone_arc` call in
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
}
