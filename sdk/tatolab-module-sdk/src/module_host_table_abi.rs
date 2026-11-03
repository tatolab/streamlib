// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The module host table's binary layout, as the lent `tatolab.runtime` builds
//! it and a module's native half reads it.
//!
//! Append-only: a minor release adds fields at the end and raises
//! [`ModuleHostTable::abi_minor`] and [`ModuleHostTable::table_size_bytes`];
//! no field is ever moved, retyped or removed within a major. Only plain data
//! and opaque handles cross — never an engine or RHI object; `cargo xtask
//! check-module-host-table-surface` holds every field here to that.
//!
//! Every verb is callable from any thread the module owns and never takes
//! Python's GIL, except [`ModuleHostTable::output_port_set_catalog_entry`],
//! which attaches to the interpreter for its round trip to the parent. The
//! surface verbs minor 2 adds reach the parent over the helper's native escalate
//! channel, with no interpreter on either end, as do the IOSurface import
//! minor 4 adds and the lend by surface id minor 5 adds.

use std::ffi::{CStr, c_void};

/// The table major this SDK reads; a table of any other major is refused.
pub const MODULE_HOST_TABLE_ABI_MAJOR: u32 = 1;

/// The table minor this SDK was built against; a runtime lending an older
/// minor is refused, a newer one accepted.
pub const MODULE_HOST_TABLE_ABI_MINOR: u32 = 5;

/// The name of the capsule a node's context hands its Python half the table in.
pub const MODULE_HOST_TABLE_PYTHON_CAPSULE_NAME: &CStr = c"tatolab.runtime.module_host_table";

/// A verb's outcome; negative is a refusal whose message the verb wrote.
pub type ModuleHostTableStatus = i32;

/// The verb did what it was asked.
pub const MODULE_HOST_TABLE_STATUS_OK: ModuleHostTableStatus = 0;
/// A read found the port's mailbox empty.
pub const MODULE_HOST_TABLE_STATUS_EMPTY: ModuleHostTableStatus = 1;
/// A wait elapsed with no input port holding a message.
pub const MODULE_HOST_TABLE_STATUS_TIMED_OUT: ModuleHostTableStatus = 2;
/// The stream the verb asked about has ended; the error message says why.
pub const MODULE_HOST_TABLE_STATUS_ENDED: ModuleHostTableStatus = 3;
/// The verb was refused; the error message says why.
pub const MODULE_HOST_TABLE_STATUS_REFUSED: ModuleHostTableStatus = -1;

/// The timestamp a write passes to be stamped with the host's monotonic now.
pub const MODULE_HOST_TABLE_TIMESTAMP_NOW: i64 = i64::MIN;

/// The [`ModuleHostTable::log`] level of a failure.
pub const MODULE_HOST_TABLE_LOG_LEVEL_ERROR: u32 = 1;
/// The [`ModuleHostTable::log`] level of a condition the node recovers from.
pub const MODULE_HOST_TABLE_LOG_LEVEL_WARN: u32 = 2;
/// The [`ModuleHostTable::log`] level of normal progress.
pub const MODULE_HOST_TABLE_LOG_LEVEL_INFO: u32 = 3;
/// The [`ModuleHostTable::log`] level of a diagnostic.
pub const MODULE_HOST_TABLE_LOG_LEVEL_DEBUG: u32 = 4;
/// The [`ModuleHostTable::log`] level of a per-step trace.
pub const MODULE_HOST_TABLE_LOG_LEVEL_TRACE: u32 = 5;

/// A requested [`ModuleHostTableAudioStreamFormat::sample_format`] that takes
/// whatever the device opened in; never the format of an opened stream.
pub const MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_ANY: u32 = 0;
/// 32-bit little-endian float scalars.
pub const MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_F32: u32 = 1;
/// 16-bit little-endian signed integer scalars.
pub const MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_I16: u32 = 2;

/// How many bytes of a refusal's message the caller's buffer holds; a longer
/// message is cut at a UTF-8 boundary.
pub const MODULE_HOST_TABLE_ERROR_MESSAGE_CAPACITY_BYTES: usize = 1024;

/// UTF-8 text the caller owns for the length of the call: not NUL-terminated.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTableUtf8 {
    /// The first byte of the text.
    pub bytes: *const u8,
    /// The text's length in bytes.
    pub length: usize,
}

/// A message payload the caller owns for the length of the call.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTableBytes {
    /// The first byte of the payload.
    pub bytes: *const u8,
    /// The payload's length in bytes.
    pub length: usize,
}

/// A port lent to native code, valid for as long as the node host is retained;
/// `0` names no port.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleHostTablePortHandle {
    pub value: u64,
}

/// A surface lent to native code, valid until
/// [`ModuleHostTable::surface_release`]; `0` names no surface.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleHostTableSurfaceHandle {
    pub value: u64,
}

/// The most planes one [`ModuleHostTableDmaBufImport`] describes.
pub const MODULE_HOST_TABLE_DMA_BUF_IMPORT_MAX_PLANES: usize = 4;

/// One foreign DMA-BUF and where the pixels of `pixel_format` sit inside it.
///
/// `fd` stays the caller's: the host dups it. Offsets and strides past
/// `plane_count` are ignored; `drm_format_modifier` other than linear (`0`) is
/// refused.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTableDmaBufImport {
    pub fd: i32,
    pub width: u32,
    pub height: u32,
    pub plane_count: u32,
    /// A pixel format wire name, such as `nv12_video_range` or `yuyv`.
    pub pixel_format: ModuleHostTableUtf8,
    pub allocation_byte_size: u64,
    pub drm_format_modifier: u64,
    pub plane_offsets_bytes: [u64; MODULE_HOST_TABLE_DMA_BUF_IMPORT_MAX_PLANES],
    pub plane_strides_bytes: [u64; MODULE_HOST_TABLE_DMA_BUF_IMPORT_MAX_PLANES],
}

/// One foreign IOSurface and the pixel format its planes hold.
///
/// `iosurface` is an `IOSurfaceRef` that stays the caller's: the host takes
/// its own reference and its own Mach port for as long as the import lives.
/// The planes' offsets and strides are the surface's own.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTableIOSurfaceImport {
    pub iosurface: *const c_void,
    /// A pixel format wire name, such as `nv12_video_range`.
    pub pixel_format: ModuleHostTableUtf8,
    pub width: u32,
    pub height: u32,
}

/// One texture out of a node output pool: a new `<slot>#<generation>` per
/// call, never a slot a consumer still holds.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTableNodeOutputTextureRequest {
    /// The pool's key, unique within the node.
    pub pool_key: ModuleHostTableUtf8,
    /// A texture format wire name, such as `rgba8_unorm`.
    pub texture_format: ModuleHostTableUtf8,
    /// Usage names joined by commas, such as `storage_binding,texture_binding`.
    pub usage: ModuleHostTableUtf8,
    pub width: u32,
    pub height: u32,
    pub rotation_depth: u32,
    /// Always zero; keeps the struct 8-aligned on every target.
    pub reserved: u32,
}

/// A surface's H.273 colour description by axis name, as `VideoFrame`'s
/// `ColorInfo` spells it; an empty name leaves that axis to its default.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTableColorInfo {
    pub primaries: ModuleHostTableUtf8,
    pub transfer: ModuleHostTableUtf8,
    pub matrix: ModuleHostTableUtf8,
    pub range: ModuleHostTableUtf8,
}

/// The caller's buffer a refused verb writes its message into.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTableErrorMessage {
    pub bytes: [u8; MODULE_HOST_TABLE_ERROR_MESSAGE_CAPACITY_BYTES],
    pub length: u32,
}

/// One message read off an input port. The payload is the host's until the
/// caller hands the message back through
/// [`ModuleHostTable::input_message_release`].
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTableReadMessage {
    pub payload: *const u8,
    pub payload_length: usize,
    pub timestamp_ns: i64,
    /// Non-zero when the message opens a group.
    pub starts_group: u32,
    /// Always zero; keeps the next field 8-aligned on every target.
    pub reserved: u32,
    pub lost_since_last_read: u64,
    pub host_owned_message: *mut c_void,
}

/// An audio stream's rate, channel count and scalar encoding. In a request,
/// a zero field takes whatever the device opened in.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleHostTableAudioStreamFormat {
    pub sample_rate: u32,
    pub channels: u32,
    /// One of the `MODULE_HOST_TABLE_AUDIO_SAMPLE_FORMAT_*` values.
    pub sample_format: u32,
}

/// What a module asks the host to open a capture stream on.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTableAudioCaptureStreamRequest {
    /// The backend-named device; empty takes the backend's default.
    pub device_id: ModuleHostTableUtf8,
    /// The format the stream must open in; a device that opens in another is
    /// refused, never converted.
    pub requested_format: ModuleHostTableAudioStreamFormat,
    /// Always zero; keeps the struct's size a multiple of 8 on every target.
    pub reserved: u32,
}

/// An audio stream the host opened for the module, valid until it is closed;
/// `0` names no stream.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleHostTableAudioStreamHandle {
    pub value: u64,
}

/// One block a capture device delivered, borrowed for the length of the
/// callback it is handed to.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTableCapturedAudioBlock {
    /// Interleaved little-endian scalars in the stream's format.
    pub interleaved_sample_bytes: ModuleHostTableBytes,
    /// Per-channel samples: the bytes hold `sample_count × channels` scalars.
    pub sample_count: u32,
    /// Always zero; keeps the next field 8-aligned on every target.
    pub reserved: u32,
    /// The monotonic instant of the block's first sample as the device timed
    /// it, never the instant of delivery.
    pub first_sample_timestamp_ns: i64,
}

/// What a capture stream calls with each block, on the host's device thread.
/// It must not block, and must not call a verb on the stream delivering it.
pub type ModuleHostTableCapturedAudioBlockCallback = unsafe extern "C" fn(
    callback_context: *mut c_void,
    captured_block: *const ModuleHostTableCapturedAudioBlock,
);

/// What a module asks the host to open a playback stream on. Minor 3.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTableAudioPlaybackStreamRequest {
    /// The backend-named device; empty takes the backend's default.
    pub device_id: ModuleHostTableUtf8,
    /// The format every block handed to the stream is in; the host converts it
    /// to the device's, and refuses the open where it cannot.
    pub requested_format: ModuleHostTableAudioStreamFormat,
    /// Always zero; keeps the struct's size a multiple of 8 on every target.
    pub reserved: u32,
}

/// What a playback stream opened in. Minor 3.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleHostTableOpenedAudioPlaybackStream {
    /// The format every block handed to the stream must be in.
    pub stream_format: ModuleHostTableAudioStreamFormat,
    /// Per-channel samples the device asks for in one cycle; `0` where the
    /// backend does not report its device's period.
    pub device_period_in_per_channel_samples: u32,
}

/// One block a playback device asks for, borrowed for the length of the
/// callback it is handed to.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTableAudioBlockRequestedByDevice {
    /// The device's buffer, to be filled whole with interleaved little-endian
    /// scalars in the stream's format; whatever is left unwritten plays as
    /// whatever was there.
    pub interleaved_sample_bytes_to_fill: *mut u8,
    /// The buffer's length in bytes.
    pub interleaved_sample_byte_count: usize,
    /// Per-channel samples the buffer has room for.
    pub sample_count: u32,
    /// Always zero; keeps the struct's size a multiple of 8 on every target.
    pub reserved: u32,
}

/// What a playback stream calls each time its device needs samples, on the
/// host's device thread. It must not block, and must not call a verb on the
/// stream asking.
pub type ModuleHostTableAudioBlockRequestedByDeviceCallback = unsafe extern "C" fn(
    callback_context: *mut c_void,
    requested_block: *const ModuleHostTableAudioBlockRequestedByDevice,
);

/// The verbs a node's native code reaches its node through, and the node host
/// they act on. Every verb's first argument is [`ModuleHostTable::node_host`].
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTable {
    /// The table's ABI major; a module refuses any other than its own.
    pub abi_major: u32,
    /// The table's ABI minor; a module refuses one older than its own.
    pub abi_minor: u32,
    /// The size of the table the runtime built, which is at least the size of
    /// the fields its minor states.
    pub table_size_bytes: u64,
    /// Opaque; kept alive by [`ModuleHostTable::node_host_retain`] and
    /// [`ModuleHostTable::node_host_release`], counted from one the capsule holds.
    pub node_host: *mut c_void,
    /// Add one count to `node_host`.
    pub node_host_retain: unsafe extern "C" fn(node_host: *mut c_void),
    /// Drop one count from `node_host`, freeing it at zero.
    pub node_host_release: unsafe extern "C" fn(node_host: *mut c_void),
    /// Open an output port Python lent to native code, by its declared name.
    pub output_port_open: unsafe extern "C" fn(
        node_host: *mut c_void,
        port_name: ModuleHostTableUtf8,
        opened_port: *mut ModuleHostTablePortHandle,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// Publish one message to every downstream link of the port. A port no
    /// link has reached drops it; an over-ceiling message is counted and
    /// dropped, never refused.
    pub output_port_write: unsafe extern "C" fn(
        node_host: *mut c_void,
        port: ModuleHostTablePortHandle,
        payload: ModuleHostTableBytes,
        timestamp_ns: i64,
        starts_group: u32,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// State the port's catalog entry: `section` is `video` or `audio`, and
    /// `entry_json` is the entry's fields as one JSON object.
    pub output_port_set_catalog_entry: unsafe extern "C" fn(
        node_host: *mut c_void,
        port: ModuleHostTablePortHandle,
        section: ModuleHostTableUtf8,
        entry_json: ModuleHostTableUtf8,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// Open an input port Python lent to native code, by its declared name.
    pub input_port_open: unsafe extern "C" fn(
        node_host: *mut c_void,
        port_name: ModuleHostTableUtf8,
        opened_port: *mut ModuleHostTablePortHandle,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// Take the port's next message, or answer
    /// [`MODULE_HOST_TABLE_STATUS_EMPTY`].
    pub input_port_read: unsafe extern "C" fn(
        node_host: *mut c_void,
        port: ModuleHostTablePortHandle,
        read_message: *mut ModuleHostTableReadMessage,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// Hand a message [`ModuleHostTable::input_port_read`] filled back to the
    /// host, once.
    pub input_message_release:
        unsafe extern "C" fn(node_host: *mut c_void, read_message: *mut ModuleHostTableReadMessage),
    /// Wait up to `timeout_ns` for any input port to hold a message.
    pub input_wait_for_any_port: unsafe extern "C" fn(
        node_host: *mut c_void,
        timeout_ns: u64,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// Non-zero once the node has been asked to stop.
    pub stop_requested: unsafe extern "C" fn(node_host: *mut c_void) -> u32,
    /// The engine's monotonic clock, comparable with every message stamp.
    pub monotonic_now_ns: unsafe extern "C" fn(node_host: *mut c_void) -> i64,
    /// Hand one record to the node's log, as the node's own.
    pub log: unsafe extern "C" fn(
        node_host: *mut c_void,
        level: u32,
        target: ModuleHostTableUtf8,
        message: ModuleHostTableUtf8,
    ),
    /// What the host offers, as one JSON object; valid while the node host is
    /// retained.
    pub host_capabilities_json: unsafe extern "C" fn(node_host: *mut c_void) -> ModuleHostTableUtf8,
    /// Open a capture stream through the engine's audio device backend, under
    /// the stream's capture-prompting policy, and write the format it opened in.
    /// Minor 1.
    pub audio_capture_stream_open: unsafe extern "C" fn(
        node_host: *mut c_void,
        request: *const ModuleHostTableAudioCaptureStreamRequest,
        opened_stream: *mut ModuleHostTableAudioStreamHandle,
        opened_format: *mut ModuleHostTableAudioStreamFormat,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// Begin calling `block_callback` with `callback_context` for every block
    /// the stream captures, replacing any callback an earlier call installed.
    /// The replaced callback is not called once this returns. Minor 1.
    pub audio_capture_stream_start: unsafe extern "C" fn(
        node_host: *mut c_void,
        stream: ModuleHostTableAudioStreamHandle,
        block_callback: ModuleHostTableCapturedAudioBlockCallback,
        callback_context: *mut c_void,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// Stop delivering; the callback is not called again once this returns.
    /// Minor 1.
    pub audio_capture_stream_stop: unsafe extern "C" fn(
        node_host: *mut c_void,
        stream: ModuleHostTableAudioStreamHandle,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// [`MODULE_HOST_TABLE_STATUS_OK`] while the device serves the stream;
    /// [`MODULE_HOST_TABLE_STATUS_ENDED`], with the failure written, once it
    /// stopped. Minor 1.
    pub audio_capture_stream_liveness: unsafe extern "C" fn(
        node_host: *mut c_void,
        stream: ModuleHostTableAudioStreamHandle,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// Stop delivering and close the stream; its handle names nothing after.
    /// Minor 1.
    pub audio_capture_stream_close:
        unsafe extern "C" fn(node_host: *mut c_void, stream: ModuleHostTableAudioStreamHandle),
    /// Minor 2: adopt a foreign DMA-BUF as a surface the graph can resolve,
    /// registered with its plane layout and as written by a foreign device.
    pub surface_import_dma_buf: unsafe extern "C" fn(
        node_host: *mut c_void,
        dma_buf_import: *const ModuleHostTableDmaBufImport,
        imported_surface: *mut ModuleHostTableSurfaceHandle,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// Minor 2: take the next texture out of a node output pool.
    pub output_texture_acquire_from_node_output_pool: unsafe extern "C" fn(
        node_host: *mut c_void,
        request: *const ModuleHostTableNodeOutputTextureRequest,
        acquired_surface: *mut ModuleHostTableSurfaceHandle,
        error_message: *mut ModuleHostTableErrorMessage,
    )
        -> ModuleHostTableStatus,
    /// Minor 2: convert between a YCbCr surface and an RGBA texture of the
    /// same extent with the engine's colour converter, returning once the
    /// destination's next reader sees the result. `ycbcr_side_color`
    /// describes whichever side is YCbCr. Minor 2 converts a YCbCr pixel
    /// buffer into an RGBA texture; from minor 5 a macOS host also converts
    /// an RGBA texture into an adopted NV12 IOSurface.
    pub surface_convert_into: unsafe extern "C" fn(
        node_host: *mut c_void,
        source: ModuleHostTableSurfaceHandle,
        destination: ModuleHostTableSurfaceHandle,
        ycbcr_side_color: *const ModuleHostTableColorInfo,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// Minor 2: the surface id a message names `surface` by; valid until the
    /// surface is released, empty for a handle that names no surface.
    pub surface_id: unsafe extern "C" fn(
        node_host: *mut c_void,
        surface: ModuleHostTableSurfaceHandle,
    ) -> ModuleHostTableUtf8,
    /// Minor 2: give a surface back. An import's registration is removed; a pool
    /// texture's slot stays the pool's.
    pub surface_release:
        unsafe extern "C" fn(node_host: *mut c_void, surface: ModuleHostTableSurfaceHandle),
    /// Open a playback stream through the engine's audio device backend in the
    /// requested format, and write the format and device period it opened
    /// with. Minor 3.
    pub audio_playback_stream_open: unsafe extern "C" fn(
        node_host: *mut c_void,
        request: *const ModuleHostTableAudioPlaybackStreamRequest,
        opened_stream: *mut ModuleHostTableAudioStreamHandle,
        opened_playback_stream: *mut ModuleHostTableOpenedAudioPlaybackStream,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// Begin calling `fill_callback` with `callback_context` each time the
    /// device needs samples, replacing any callback an earlier call installed.
    /// The replaced callback is not called once this returns. Minor 3.
    pub audio_playback_stream_start: unsafe extern "C" fn(
        node_host: *mut c_void,
        stream: ModuleHostTableAudioStreamHandle,
        fill_callback: ModuleHostTableAudioBlockRequestedByDeviceCallback,
        callback_context: *mut c_void,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// Stop asking; the callback is not called again once this returns.
    /// Minor 3.
    pub audio_playback_stream_stop: unsafe extern "C" fn(
        node_host: *mut c_void,
        stream: ModuleHostTableAudioStreamHandle,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// [`MODULE_HOST_TABLE_STATUS_OK`] while the device serves the stream;
    /// [`MODULE_HOST_TABLE_STATUS_ENDED`], with the failure written, once it
    /// stopped. Minor 3.
    pub audio_playback_stream_liveness: unsafe extern "C" fn(
        node_host: *mut c_void,
        stream: ModuleHostTableAudioStreamHandle,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// Stop asking and close the stream; its handle names nothing after.
    /// Minor 3.
    pub audio_playback_stream_close:
        unsafe extern "C" fn(node_host: *mut c_void, stream: ModuleHostTableAudioStreamHandle),
    /// Minor 4: adopt a foreign IOSurface as a surface the graph can resolve,
    /// registered as written by a foreign device. macOS only; refused by name
    /// elsewhere.
    pub surface_import_iosurface: unsafe extern "C" fn(
        node_host: *mut c_void,
        iosurface_import: *const ModuleHostTableIOSurfaceImport,
        imported_surface: *mut ModuleHostTableSurfaceHandle,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
    /// Minor 5: lend native code the surface a message names by
    /// `surface_id`, such as an upstream node's frame, to convert from.
    /// Releasing it gives back only the lend; the surface stays its
    /// publisher's.
    pub surface_lend_by_surface_id: unsafe extern "C" fn(
        node_host: *mut c_void,
        surface_id: ModuleHostTableUtf8,
        lent_surface: *mut ModuleHostTableSurfaceHandle,
        error_message: *mut ModuleHostTableErrorMessage,
    ) -> ModuleHostTableStatus,
}

impl ModuleHostTableUtf8 {
    /// A view of `text` for the length of a call.
    pub fn of(text: &str) -> Self {
        Self {
            bytes: text.as_ptr(),
            length: text.len(),
        }
    }

    /// The text this names.
    ///
    /// # Safety
    ///
    /// `bytes` points at `length` valid UTF-8 bytes that outlive `'text`.
    pub unsafe fn as_str<'text>(self) -> Result<&'text str, std::str::Utf8Error> {
        if self.length == 0 {
            return Ok("");
        }
        // SAFETY: the caller's contract.
        std::str::from_utf8(unsafe { std::slice::from_raw_parts(self.bytes, self.length) })
    }
}

impl ModuleHostTableBytes {
    /// A view of `payload` for the length of a call.
    pub fn of(payload: &[u8]) -> Self {
        Self {
            bytes: payload.as_ptr(),
            length: payload.len(),
        }
    }

    /// The bytes this names.
    ///
    /// # Safety
    ///
    /// `bytes` points at `length` readable bytes that outlive `'payload`.
    pub unsafe fn as_slice<'payload>(self) -> &'payload [u8] {
        if self.length == 0 {
            return &[];
        }
        // SAFETY: the caller's contract.
        unsafe { std::slice::from_raw_parts(self.bytes, self.length) }
    }
}

impl ModuleHostTableErrorMessage {
    /// An empty buffer for one call.
    pub const fn empty() -> Self {
        Self {
            bytes: [0; MODULE_HOST_TABLE_ERROR_MESSAGE_CAPACITY_BYTES],
            length: 0,
        }
    }

    /// Write `message`, cut at the last UTF-8 boundary that fits.
    pub fn write(&mut self, message: &str) {
        let mut length = message
            .len()
            .min(MODULE_HOST_TABLE_ERROR_MESSAGE_CAPACITY_BYTES);
        while !message.is_char_boundary(length) {
            length -= 1;
        }
        self.bytes[..length].copy_from_slice(&message.as_bytes()[..length]);
        self.length = length as u32;
    }

    /// The message written, lossily if a host wrote bytes that are not UTF-8.
    pub fn message(&self) -> String {
        let length = (self.length as usize).min(MODULE_HOST_TABLE_ERROR_MESSAGE_CAPACITY_BYTES);
        String::from_utf8_lossy(&self.bytes[..length]).into_owned()
    }
}

impl ModuleHostTableReadMessage {
    /// A message slot for one read.
    pub const fn empty() -> Self {
        Self {
            payload: std::ptr::null(),
            payload_length: 0,
            timestamp_ns: 0,
            starts_group: 0,
            reserved: 0,
            lost_since_last_read: 0,
            host_owned_message: std::ptr::null_mut(),
        }
    }
}

#[cfg(test)]
mod layout_regression_tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    const POINTER: usize = size_of::<*const c_void>();

    #[test]
    fn utf8_and_bytes_are_a_pointer_and_a_length() {
        for (size, align, bytes_offset, length_offset) in [
            (
                size_of::<ModuleHostTableUtf8>(),
                align_of::<ModuleHostTableUtf8>(),
                offset_of!(ModuleHostTableUtf8, bytes),
                offset_of!(ModuleHostTableUtf8, length),
            ),
            (
                size_of::<ModuleHostTableBytes>(),
                align_of::<ModuleHostTableBytes>(),
                offset_of!(ModuleHostTableBytes, bytes),
                offset_of!(ModuleHostTableBytes, length),
            ),
        ] {
            assert_eq!(size, 2 * POINTER);
            assert_eq!(align, POINTER);
            assert_eq!(bytes_offset, 0);
            assert_eq!(length_offset, POINTER);
        }
    }

    #[test]
    fn a_port_handle_is_one_u64() {
        assert_eq!(size_of::<ModuleHostTablePortHandle>(), 8);
        assert_eq!(align_of::<ModuleHostTablePortHandle>(), 8);
    }

    #[test]
    fn an_error_message_is_its_buffer_then_its_length() {
        assert_eq!(offset_of!(ModuleHostTableErrorMessage, bytes), 0);
        assert_eq!(offset_of!(ModuleHostTableErrorMessage, length), 1024);
        assert_eq!(size_of::<ModuleHostTableErrorMessage>(), 1028);
        assert_eq!(align_of::<ModuleHostTableErrorMessage>(), 4);
    }

    #[test]
    fn a_read_message_lays_out_on_8_byte_fields() {
        assert_eq!(offset_of!(ModuleHostTableReadMessage, payload), 0);
        assert_eq!(
            offset_of!(ModuleHostTableReadMessage, payload_length),
            POINTER
        );
        assert_eq!(
            offset_of!(ModuleHostTableReadMessage, timestamp_ns),
            2 * POINTER
        );
        assert_eq!(
            offset_of!(ModuleHostTableReadMessage, starts_group),
            2 * POINTER + 8
        );
        assert_eq!(
            offset_of!(ModuleHostTableReadMessage, reserved),
            2 * POINTER + 12
        );
        assert_eq!(
            offset_of!(ModuleHostTableReadMessage, lost_since_last_read),
            2 * POINTER + 16
        );
        assert_eq!(
            offset_of!(ModuleHostTableReadMessage, host_owned_message),
            2 * POINTER + 24
        );
        assert_eq!(size_of::<ModuleHostTableReadMessage>(), 3 * POINTER + 24);
    }

    /// Every v1.0 field at the offset a module built against v1.0 reads it
    /// from: the header, then one pointer-sized slot per field in order.
    #[test]
    fn the_v1_0_table_puts_every_field_where_a_v1_0_module_reads_it() {
        assert_eq!(offset_of!(ModuleHostTable, abi_major), 0);
        assert_eq!(offset_of!(ModuleHostTable, abi_minor), 4);
        assert_eq!(offset_of!(ModuleHostTable, table_size_bytes), 8);
        let pointer_slots_in_order = [
            offset_of!(ModuleHostTable, node_host),
            offset_of!(ModuleHostTable, node_host_retain),
            offset_of!(ModuleHostTable, node_host_release),
            offset_of!(ModuleHostTable, output_port_open),
            offset_of!(ModuleHostTable, output_port_write),
            offset_of!(ModuleHostTable, output_port_set_catalog_entry),
            offset_of!(ModuleHostTable, input_port_open),
            offset_of!(ModuleHostTable, input_port_read),
            offset_of!(ModuleHostTable, input_message_release),
            offset_of!(ModuleHostTable, input_wait_for_any_port),
            offset_of!(ModuleHostTable, stop_requested),
            offset_of!(ModuleHostTable, monotonic_now_ns),
            offset_of!(ModuleHostTable, log),
            offset_of!(ModuleHostTable, host_capabilities_json),
        ];
        for (slot, offset) in pointer_slots_in_order.into_iter().enumerate() {
            assert_eq!(offset, 16 + slot * POINTER, "pointer slot {slot}");
        }
    }

    /// Minor 1 appends its audio capture verbs after every v1.0 field.
    #[test]
    fn the_v1_1_table_appends_its_audio_capture_verbs_after_every_v1_0_field() {
        let pointer_slots_appended_at_minor_1 = [
            offset_of!(ModuleHostTable, audio_capture_stream_open),
            offset_of!(ModuleHostTable, audio_capture_stream_start),
            offset_of!(ModuleHostTable, audio_capture_stream_stop),
            offset_of!(ModuleHostTable, audio_capture_stream_liveness),
            offset_of!(ModuleHostTable, audio_capture_stream_close),
        ];
        for (slot, offset) in pointer_slots_appended_at_minor_1.into_iter().enumerate() {
            assert_eq!(offset, 16 + (14 + slot) * POINTER, "minor 1 slot {slot}");
        }
    }

    /// Minor 3 appends its audio playback verbs after every v1.2 field.
    #[test]
    fn the_v1_3_table_appends_its_audio_playback_verbs_after_every_v1_2_field() {
        let pointer_slots_appended_at_minor_3 = [
            offset_of!(ModuleHostTable, audio_playback_stream_open),
            offset_of!(ModuleHostTable, audio_playback_stream_start),
            offset_of!(ModuleHostTable, audio_playback_stream_stop),
            offset_of!(ModuleHostTable, audio_playback_stream_liveness),
            offset_of!(ModuleHostTable, audio_playback_stream_close),
        ];
        for (slot, offset) in pointer_slots_appended_at_minor_3.into_iter().enumerate() {
            assert_eq!(offset, 16 + (24 + slot) * POINTER, "minor 3 slot {slot}");
        }
        assert_eq!(align_of::<ModuleHostTable>(), 8);
    }

    #[test]
    fn a_playback_request_is_the_device_id_then_the_format() {
        assert_eq!(
            offset_of!(ModuleHostTableAudioPlaybackStreamRequest, device_id),
            0
        );
        assert_eq!(
            offset_of!(ModuleHostTableAudioPlaybackStreamRequest, requested_format),
            2 * POINTER
        );
        assert_eq!(
            offset_of!(ModuleHostTableAudioPlaybackStreamRequest, reserved),
            2 * POINTER + 12
        );
        assert_eq!(
            size_of::<ModuleHostTableAudioPlaybackStreamRequest>(),
            2 * POINTER + 16
        );
        assert_eq!(
            align_of::<ModuleHostTableAudioPlaybackStreamRequest>(),
            POINTER
        );
    }

    #[test]
    fn an_opened_playback_stream_is_its_format_then_its_device_period() {
        assert_eq!(
            offset_of!(ModuleHostTableOpenedAudioPlaybackStream, stream_format),
            0
        );
        assert_eq!(
            offset_of!(
                ModuleHostTableOpenedAudioPlaybackStream,
                device_period_in_per_channel_samples
            ),
            12
        );
        assert_eq!(size_of::<ModuleHostTableOpenedAudioPlaybackStream>(), 16);
        assert_eq!(align_of::<ModuleHostTableOpenedAudioPlaybackStream>(), 4);
    }

    #[test]
    fn a_block_requested_by_a_device_lays_out_on_8_byte_fields() {
        assert_eq!(
            offset_of!(
                ModuleHostTableAudioBlockRequestedByDevice,
                interleaved_sample_bytes_to_fill
            ),
            0
        );
        assert_eq!(
            offset_of!(
                ModuleHostTableAudioBlockRequestedByDevice,
                interleaved_sample_byte_count
            ),
            POINTER
        );
        assert_eq!(
            offset_of!(ModuleHostTableAudioBlockRequestedByDevice, sample_count),
            2 * POINTER
        );
        assert_eq!(
            offset_of!(ModuleHostTableAudioBlockRequestedByDevice, reserved),
            2 * POINTER + 4
        );
        assert_eq!(
            size_of::<ModuleHostTableAudioBlockRequestedByDevice>(),
            2 * POINTER + 8
        );
    }

    #[test]
    fn an_audio_stream_format_is_three_u32s() {
        assert_eq!(offset_of!(ModuleHostTableAudioStreamFormat, sample_rate), 0);
        assert_eq!(offset_of!(ModuleHostTableAudioStreamFormat, channels), 4);
        assert_eq!(
            offset_of!(ModuleHostTableAudioStreamFormat, sample_format),
            8
        );
        assert_eq!(size_of::<ModuleHostTableAudioStreamFormat>(), 12);
        assert_eq!(align_of::<ModuleHostTableAudioStreamFormat>(), 4);
    }

    #[test]
    fn a_capture_request_is_the_device_id_then_the_format() {
        assert_eq!(
            offset_of!(ModuleHostTableAudioCaptureStreamRequest, device_id),
            0
        );
        assert_eq!(
            offset_of!(ModuleHostTableAudioCaptureStreamRequest, requested_format),
            2 * POINTER
        );
        assert_eq!(
            offset_of!(ModuleHostTableAudioCaptureStreamRequest, reserved),
            2 * POINTER + 12
        );
        assert_eq!(
            size_of::<ModuleHostTableAudioCaptureStreamRequest>(),
            2 * POINTER + 16
        );
        assert_eq!(
            align_of::<ModuleHostTableAudioCaptureStreamRequest>(),
            POINTER
        );
    }

    /// Minor 2 appends its surface verbs after the minor 1 audio capture verbs.
    #[test]
    fn the_v1_2_table_appends_its_surface_verbs_after_every_v1_1_field() {
        let pointer_slots_in_order = [
            offset_of!(ModuleHostTable, surface_import_dma_buf),
            offset_of!(
                ModuleHostTable,
                output_texture_acquire_from_node_output_pool
            ),
            offset_of!(ModuleHostTable, surface_convert_into),
            offset_of!(ModuleHostTable, surface_id),
            offset_of!(ModuleHostTable, surface_release),
        ];
        for (slot, offset) in pointer_slots_in_order.into_iter().enumerate() {
            assert_eq!(offset, 16 + (19 + slot) * POINTER, "pointer slot {slot}");
        }
    }

    /// Minor 4 appends its IOSurface import after the minor 3 audio playback verbs.
    #[test]
    fn the_v1_4_table_appends_its_iosurface_import_after_every_v1_3_field() {
        assert_eq!(
            offset_of!(ModuleHostTable, surface_import_iosurface),
            16 + 29 * POINTER
        );
    }

    /// Minor 5 appends its lend by surface id after the minor 4 IOSurface import.
    #[test]
    fn the_v1_5_table_appends_its_lend_by_surface_id_after_every_v1_4_field() {
        assert_eq!(
            offset_of!(ModuleHostTable, surface_lend_by_surface_id),
            16 + 30 * POINTER
        );
        assert_eq!(size_of::<ModuleHostTable>(), 16 + 31 * POINTER);
    }

    #[test]
    fn an_iosurface_import_puts_its_surface_then_its_name_then_its_extent() {
        assert_eq!(offset_of!(ModuleHostTableIOSurfaceImport, iosurface), 0);
        assert_eq!(
            offset_of!(ModuleHostTableIOSurfaceImport, pixel_format),
            POINTER
        );
        assert_eq!(
            offset_of!(ModuleHostTableIOSurfaceImport, width),
            3 * POINTER
        );
        assert_eq!(
            offset_of!(ModuleHostTableIOSurfaceImport, height),
            3 * POINTER + 4
        );
        assert_eq!(size_of::<ModuleHostTableIOSurfaceImport>(), 3 * POINTER + 8);
    }

    #[test]
    fn a_surface_handle_is_one_u64() {
        assert_eq!(size_of::<ModuleHostTableSurfaceHandle>(), 8);
        assert_eq!(align_of::<ModuleHostTableSurfaceHandle>(), 8);
    }

    #[test]
    fn a_dma_buf_import_puts_its_geometry_then_its_name_then_its_planes() {
        assert_eq!(offset_of!(ModuleHostTableDmaBufImport, fd), 0);
        assert_eq!(offset_of!(ModuleHostTableDmaBufImport, width), 4);
        assert_eq!(offset_of!(ModuleHostTableDmaBufImport, height), 8);
        assert_eq!(offset_of!(ModuleHostTableDmaBufImport, plane_count), 12);
        assert_eq!(offset_of!(ModuleHostTableDmaBufImport, pixel_format), 16);
        let after_the_name = 16 + 2 * POINTER;
        assert_eq!(
            offset_of!(ModuleHostTableDmaBufImport, allocation_byte_size),
            after_the_name
        );
        assert_eq!(
            offset_of!(ModuleHostTableDmaBufImport, drm_format_modifier),
            after_the_name + 8
        );
        assert_eq!(
            offset_of!(ModuleHostTableDmaBufImport, plane_offsets_bytes),
            after_the_name + 16
        );
        assert_eq!(
            offset_of!(ModuleHostTableDmaBufImport, plane_strides_bytes),
            after_the_name + 48
        );
        assert_eq!(
            size_of::<ModuleHostTableDmaBufImport>(),
            after_the_name + 80
        );
    }

    #[test]
    fn a_node_output_texture_request_puts_its_names_then_its_geometry() {
        assert_eq!(
            offset_of!(ModuleHostTableNodeOutputTextureRequest, pool_key),
            0
        );
        assert_eq!(
            offset_of!(ModuleHostTableNodeOutputTextureRequest, texture_format),
            2 * POINTER
        );
        assert_eq!(
            offset_of!(ModuleHostTableNodeOutputTextureRequest, usage),
            4 * POINTER
        );
        assert_eq!(
            offset_of!(ModuleHostTableNodeOutputTextureRequest, width),
            6 * POINTER
        );
        assert_eq!(
            offset_of!(ModuleHostTableNodeOutputTextureRequest, reserved),
            6 * POINTER + 12
        );
        assert_eq!(
            size_of::<ModuleHostTableNodeOutputTextureRequest>(),
            6 * POINTER + 16
        );
    }

    #[test]
    fn a_color_info_is_four_names() {
        assert_eq!(offset_of!(ModuleHostTableColorInfo, matrix), 4 * POINTER);
        assert_eq!(size_of::<ModuleHostTableColorInfo>(), 8 * POINTER);
    }

    #[test]
    fn an_audio_stream_handle_is_one_u64() {
        assert_eq!(size_of::<ModuleHostTableAudioStreamHandle>(), 8);
        assert_eq!(align_of::<ModuleHostTableAudioStreamHandle>(), 8);
    }

    #[test]
    fn a_captured_block_lays_out_on_8_byte_fields() {
        assert_eq!(
            offset_of!(ModuleHostTableCapturedAudioBlock, interleaved_sample_bytes),
            0
        );
        assert_eq!(
            offset_of!(ModuleHostTableCapturedAudioBlock, sample_count),
            2 * POINTER
        );
        assert_eq!(
            offset_of!(ModuleHostTableCapturedAudioBlock, reserved),
            2 * POINTER + 4
        );
        assert_eq!(
            offset_of!(ModuleHostTableCapturedAudioBlock, first_sample_timestamp_ns),
            2 * POINTER + 8
        );
        assert_eq!(
            size_of::<ModuleHostTableCapturedAudioBlock>(),
            2 * POINTER + 16
        );
    }
}
