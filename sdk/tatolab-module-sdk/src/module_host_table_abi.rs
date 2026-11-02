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
//! which attaches to the interpreter for its round trip to the parent.

use std::ffi::{CStr, c_void};

/// The table major this SDK reads; a table of any other major is refused.
pub const MODULE_HOST_TABLE_ABI_MAJOR: u32 = 1;

/// The table minor this SDK was built against; a runtime lending an older
/// minor is refused, a newer one accepted.
pub const MODULE_HOST_TABLE_ABI_MINOR: u32 = 0;

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
/// The verb was refused; the error message says why.
pub const MODULE_HOST_TABLE_STATUS_REFUSED: ModuleHostTableStatus = -1;

/// The timestamp a write passes to be stamped with the host's monotonic now.
pub const MODULE_HOST_TABLE_TIMESTAMP_NOW: i64 = i64::MIN;

/// [`ModuleHostTable::log`] levels, most severe first.
pub const MODULE_HOST_TABLE_LOG_LEVEL_ERROR: u32 = 1;
pub const MODULE_HOST_TABLE_LOG_LEVEL_WARN: u32 = 2;
pub const MODULE_HOST_TABLE_LOG_LEVEL_INFO: u32 = 3;
pub const MODULE_HOST_TABLE_LOG_LEVEL_DEBUG: u32 = 4;
pub const MODULE_HOST_TABLE_LOG_LEVEL_TRACE: u32 = 5;

/// How many bytes of a refusal's message the caller's buffer holds; a longer
/// message is cut at a UTF-8 boundary.
pub const MODULE_HOST_TABLE_ERROR_MESSAGE_CAPACITY_BYTES: usize = 1024;

/// UTF-8 text the caller owns for the length of the call: not NUL-terminated.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTableUtf8 {
    pub bytes: *const u8,
    pub length: usize,
}

/// A message payload the caller owns for the length of the call.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTableBytes {
    pub bytes: *const u8,
    pub length: usize,
}

/// A port lent to native code, valid for as long as the node host is retained;
/// `0` names no port.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleHostTablePortHandle {
    pub value: u64,
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

/// The verbs a node's native code reaches its node through, and the node host
/// they act on. Every verb's first argument is [`ModuleHostTable::node_host`].
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostTable {
    pub abi_major: u32,
    pub abi_minor: u32,
    /// The size of the table the runtime built, which is at least the size of
    /// the fields its minor states.
    pub table_size_bytes: u64,
    /// Opaque; kept alive by [`ModuleHostTable::node_host_retain`] and
    /// [`ModuleHostTable::node_host_release`], counted from one the capsule holds.
    pub node_host: *mut c_void,
    pub node_host_retain: unsafe extern "C" fn(node_host: *mut c_void),
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
        assert_eq!(size_of::<ModuleHostTable>(), 16 + 14 * POINTER);
        assert_eq!(align_of::<ModuleHostTable>(), 8);
    }
}
