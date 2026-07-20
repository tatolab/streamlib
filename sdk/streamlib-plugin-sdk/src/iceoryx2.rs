// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twins of the engine's iceoryx2 transport views.
//!
//! [`OutputWriter`] and [`InputMailboxes`] are the public
//! `#[repr(C)] { handle, vtable }` PluginAbiObjects a processor struct holds
//! via the macro-emitted `outputs` / `inputs` fields. Every method
//! dispatches through the host-installed vtable to the host-allocated
//! inner; the cdylib never names the host inner's layout.
//!
//! # Opaque inner types
//!
//! The `#[processor]` macro emits trait overrides whose signatures name
//! [`OutputWriterInner`] / [`InputMailboxesInner`] (the
//! `iceoryx2_*_inner()` accessors). The HOST backing of those types stays
//! in the engine; here they are **opaque empty types** that exist only so
//! the generated method signatures compile. The SDK's
//! [`OutputWriter::inner_arc`] / [`InputMailboxes::inner_arc`] return
//! `None` (a cdylib has no host-side inner Arc — resources are wired
//! through the vtable's `set_iceoryx2_resources` host path), so any
//! generated code path that reaches an inner is dead in cdylib mode. The
//! opaque [`InputMailboxesInner`] still exposes `has_port` / `add_port`
//! stubs because the macro's `assign_inputs` body names them inside an
//! `if let Some(input_inner)` block — dead at runtime (inner_arc is
//! `None`), but it must typecheck.

use std::ffi::c_void;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use streamlib_error::{Error, Result};
use streamlib_plugin_abi::{InputMailboxesVTable, OutputWriterVTable};

use serde::{Deserialize, Serialize as SerdeSerialize};

use crate::bag::Bag;
use crate::media_clock::MediaClock;

/// Default receive-buffer size (bytes) the cdylib read path starts with before
/// growing to the frame it actually receives. Mirrors the host's
/// `streamlib_ipc_types::DEFAULT_EXPECTED_PAYLOAD_BYTES`, restated here because
/// the engine-free SDK does not depend on the host IPC crate.
///
/// A frame larger than this is delivered by the host grow-and-retry protocol
/// (#1421), not truncated — so this is a starting hint, never a cap. It retires
/// the phase-1 interim `BAG_MAX_PAYLOAD_BYTES` write-side budget guard, which
/// PowerOfTwo dynamic slot allocation obviated.
pub const BAG_DEFAULT_EXPECTED_PAYLOAD_BYTES: usize = 65536;

/// How frames should be read from an input port's buffer. Engine-free
/// twin of the engine's `iceoryx2::ReadMode`; the macro emits
/// `ReadMode::{SkipToLatest,ReadNextInOrder}` into generated
/// `set_iceoryx2_resources` bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, SerdeSerialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadMode {
    /// Drain buffer and return only the newest frame (optimal for video).
    #[default]
    SkipToLatest,
    /// Read next frame in FIFO order (required for audio).
    ReadNextInOrder,
}

// =============================================================================
// cdylib vtable resolvers
// =============================================================================

/// Cdylib-arm resolver for the host's [`OutputWriterVTable`]. Returns the
/// host-installed pointer cached on [`crate::plugin::HostCallbacks`], or
/// null when no callbacks are installed / the field is null. (No host
/// static exists in the engine-free SDK — the host arm is the engine's.)
///
/// Currently unused: the SDK's views are constructed via [`from_raw_parts`]
/// (the host hands the vtable directly through `set_iceoryx2_resources`).
/// Kept as the cdylib-arm resolver for a future inner-construction path,
/// mirroring the engine's `host_output_writer_vtable`.
#[allow(dead_code)]
fn host_output_writer_vtable() -> *const OutputWriterVTable {
    crate::plugin::host_callbacks()
        .map(|c| c.output_writer_vtable)
        .filter(|p| !p.is_null())
        .unwrap_or(std::ptr::null())
}

/// Cdylib-arm resolver for the host's [`InputMailboxesVTable`]. See
/// [`host_output_writer_vtable`].
#[allow(dead_code)]
fn host_input_mailboxes_vtable() -> *const InputMailboxesVTable {
    crate::plugin::host_callbacks()
        .map(|c| c.input_mailboxes_vtable)
        .filter(|p| !p.is_null())
        .unwrap_or(std::ptr::null())
}

// =============================================================================
// Opaque inner types (host backing stays in the engine)
// =============================================================================

/// Opaque cdylib-side placeholder for the engine's host-side
/// `OutputWriterInner`. Exists only so the macro-emitted
/// `iceoryx2_output_writer_inner()` return signature compiles. The SDK
/// never constructs one (its `inner_arc()` returns `None`).
pub struct OutputWriterInner {
    _opaque: (),
}

/// Opaque cdylib-side placeholder for the engine's host-side
/// `InputMailboxesInner`. Exists so the macro-emitted
/// `iceoryx2_input_mailboxes_inner()` return signature compiles, and so
/// the macro's `assign_inputs` body (`has_port` / `add_port` calls inside
/// a dead `if let Some(inner)` block) typechecks. The SDK never
/// constructs one (its `inner_arc()` returns `None`).
pub struct InputMailboxesInner {
    _opaque: (),
}

impl InputMailboxesInner {
    /// Stub — named by the macro's `assign_inputs` body. Never called in
    /// cdylib mode (the enclosing `inner_arc()` returns `None`).
    pub fn has_port(&self, _port: &str) -> bool {
        false
    }

    /// Stub — named by the macro's `assign_inputs` body. Never called in
    /// cdylib mode.
    pub fn add_port(&self, _port: &str, _buffer_size: usize, _read_mode: ReadMode) {}
}

// =============================================================================
// OutputWriter PluginAbiObject
// =============================================================================

/// Public output writer PluginAbiObject. The macro emits
/// `pub outputs: OutputWriter` on every processor struct that declares
/// output ports.
#[repr(C)]
pub struct OutputWriter {
    pub(crate) handle: *const c_void,
    pub(crate) vtable: *const OutputWriterVTable,
}

// SAFETY: `handle` points at an `Arc<OutputWriterInner>` whose interior
// is Send+Sync; refcount management crosses the plugin ABI through the
// vtable but the Arc bookkeeping runs in host-compiled code.
unsafe impl Send for OutputWriter {}
unsafe impl Sync for OutputWriter {}

impl OutputWriter {
    /// Build an empty pre-wiring PluginAbiObject with null handle and null
    /// vtable. The host patches in real values via
    /// `ProcessorVTable::set_iceoryx2_resources`.
    pub fn empty() -> Self {
        Self {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
        }
    }

    /// Raw-pointer construction used by
    /// `ProcessorVTable::set_iceoryx2_resources` host wiring.
    pub(crate) fn from_raw_parts(handle: *const c_void, vtable: *const OutputWriterVTable) -> Self {
        Self { handle, vtable }
    }

    /// Returns true iff this PluginAbiObject has been wired to a real
    /// host-allocated inner.
    pub fn is_configured(&self) -> bool {
        !self.handle.is_null() && !self.vtable.is_null()
    }

    /// Cdylib-side `inner_arc` returns `None` — a cdylib has no host-side
    /// `Arc<OutputWriterInner>`. Per-frame work goes through the vtable;
    /// the host wires connections via its own inner reach.
    pub fn inner_arc(&self) -> Option<Arc<OutputWriterInner>> {
        None
    }

    /// Write a frame to the specified output port.
    pub fn write<T: Serialize>(&self, port: &str, value: &T) -> Result<()> {
        let timestamp_ns = MediaClock::now().as_nanos() as i64;
        self.write_with_timestamp(port, value, timestamp_ns)
    }

    /// Write a frame to the specified output port with an explicit
    /// timestamp.
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
        // SAFETY: vtable + handle are non-null per is_configured(). The fn
        // pointer's lifetime is tied to the host's process; err_buf is a
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

    /// Write a schema-free [`Bag`] to `port` with an explicit timestamp.
    ///
    /// Encodes the bag as a msgpack named map and writes it over the same wire
    /// as [`Self::write`]. There is no write-side size budget: the host
    /// publisher grows its iceoryx2 segment (PowerOfTwo) to fit an oversized
    /// payload, and the node-level per-channel ceiling is the graceful bound
    /// enforced host-side.
    #[tracing::instrument(level = "trace", skip(self, bag), fields(port = %port))]
    pub fn write_bag(&self, port: &str, bag: &Bag, timestamp_ns: i64) -> Result<()> {
        let data = bag.to_msgpack()?;
        self.write_raw(port, &data, timestamp_ns)
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
        unsafe {
            ((*self.vtable).drop_arc)(self.handle);
        }
        self.handle = std::ptr::null();
        self.vtable = std::ptr::null();
    }
}

// =============================================================================
// InputMailboxes PluginAbiObject
// =============================================================================

/// Public input mailboxes PluginAbiObject. The macro emits
/// `pub inputs: InputMailboxes` on every processor struct that declares
/// input ports.
#[repr(C)]
pub struct InputMailboxes {
    pub(crate) handle: *const c_void,
    pub(crate) vtable: *const InputMailboxesVTable,
}

// SAFETY: `handle` points at an `Arc<InputMailboxesInner>` whose interior
// is Send+Sync; refcount management crosses the plugin ABI through the
// host-installed refcount fn pointers.
unsafe impl Send for InputMailboxes {}
unsafe impl Sync for InputMailboxes {}

impl InputMailboxes {
    /// Build an empty pre-wiring PluginAbiObject with null handle and null
    /// vtable. The host patches in real values via
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

    /// Cdylib-side `inner_arc` returns `None` — a cdylib has no host-side
    /// `Arc<InputMailboxesInner>`. Per-frame work goes through the vtable.
    pub fn inner_arc(&self) -> Option<Arc<InputMailboxesInner>> {
        None
    }

    /// Read and deserialize a frame from the given port.
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
    /// Grow-and-retry (#1421): starts with a [`BAG_DEFAULT_EXPECTED_PAYLOAD_BYTES`]
    /// buffer and, when the host reports the next frame is larger
    /// (`out_len > buf.len()`, `has_data == true`), resizes to that length and
    /// reads again. The host stashes the oversized frame across the two calls, so
    /// a PowerOfTwo-grown payload is delivered rather than dropped.
    pub fn read_raw(&self, port: &str) -> Result<Option<(Vec<u8>, i64)>> {
        if !self.is_configured() {
            return Ok(None);
        }

        // SAFETY: vtable + handle are non-null per is_configured().
        unsafe {
            streamlib_plugin_abi::grow_and_retry_read(
                self.vtable,
                self.handle,
                port,
                BAG_DEFAULT_EXPECTED_PAYLOAD_BYTES,
            )
        }
        .map_err(Error::Link)
    }

    /// Read the latest frame on `port` as a schema-free [`Bag`].
    ///
    /// Returns `Ok(Some((bag, timestamp_ns)))` on a decoded frame and
    /// `Ok(None)` when the mailbox is empty. A frame whose bytes are not a
    /// msgpack named map surfaces as the named [`Error::BagDecodeFailed`],
    /// never a panic.
    #[tracing::instrument(level = "trace", skip(self), fields(port = %port))]
    pub fn read_bag(&self, port: &str) -> Result<Option<(Bag, i64)>> {
        match self.read_raw(port)? {
            None => Ok(None),
            Some((data, timestamp_ns)) => {
                let bag = Bag::from_msgpack(&data)?;
                Ok(Some((bag, timestamp_ns)))
            }
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

    #[test]
    fn bag_default_expected_payload_bytes_matches_host_default() {
        // The engine-free SDK restates the host's
        // `streamlib_ipc_types::DEFAULT_EXPECTED_PAYLOAD_BYTES` (64 KiB) as its
        // read-buffer starting size; a drift here would grow-and-retry from the
        // wrong floor.
        assert_eq!(BAG_DEFAULT_EXPECTED_PAYLOAD_BYTES, 65536);
    }
}
