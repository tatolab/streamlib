// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `ProcessorVTable` — extern "C" dispatch table for processor instances.

use core::ffi::c_void;

use crate::{InputMailboxesVTable, OutputWriterVTable};

/// Layout version of the [`crate::ProcessorVTable`] struct. Read by the
/// host's `processor_register` impl before dereferencing any vtable
/// entry; mismatching versions abort the registration cleanly.
///
/// - v1: 17 fn pointer slots including `get_iceoryx2_output_writer_arc`
///   and `get_iceoryx2_input_mailboxes_mut` returning shared Rust types
///   (the host coupled to the cdylib's `streamlib-engine` source
///   version through `Arc<OutputWriter>` / `&mut InputMailboxes`
///   layout).
/// - v2: issue #894 retires both shared-type crossings. The two
///   `get_iceoryx2_*` slots are removed and a single
///   `set_iceoryx2_resources` slot is added. The host now allocates
///   `OutputWriterInner` and `InputMailboxesInner` and hands the
///   cdylib `(handle, vtable)` PluginAbiObjects via the new slot; the
///   per-frame `write_raw` / `read_raw` calls dispatch through
///   [`crate::OutputWriterVTable`] / [`crate::InputMailboxesVTable`]. **ABI-
///   breaking** — plugins built against v1 are not load-compatible
///   with a v2 host (the slot count and offsets differ).
pub const PROCESSOR_VTABLE_LAYOUT_VERSION: u32 = 2;

/// `extern "C" fn` dispatch table the host uses to call methods on a
/// dlopen'd processor instance. Replaces the `Box<dyn
/// DynGeneratedProcessor>` dyn-trait crossing the host used to
/// dispatch through.
///
/// The vtable covers the full host-called surface — constructor +
/// lifecycle (setup / teardown / on_pause / on_resume / process /
/// start / stop / destroy) plus the static-info, iceoryx2-wiring,
/// and config-IO methods compiler ops invoke on every processor.
/// Methods bodies receive `&RuntimeContext*Access` shims whose
/// public method surface is implemented entirely in terms of the
/// callback tables on [`crate::HostServices`] — no Rust trait-object or
/// shared-struct-layout crossing at the host/cdylib boundary.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0 forever. The host's
/// `processor_register` impl reads it before dereferencing any other
/// field; older vtables loaded into newer hosts are rejected
/// cleanly. New fields go at the **end** and bump
/// [`PROCESSOR_VTABLE_LAYOUT_VERSION`].
///
/// # Error convention
///
/// Sync lifecycle methods (`process`, `start`, `stop`) and async
/// lifecycle methods (`setup`, `teardown`, `on_pause`, `on_resume`)
/// share the error convention: return `0` on success, non-zero on
/// failure. `err_buf` / `err_buf_cap` is a caller-provided UTF-8
/// scratch buffer the callee writes a message into; `*err_len`
/// receives the actual byte count written. Truncation is benign
/// (caller's buffer was too small).
///
/// `construct` follows the same convention but returns a `*mut
/// c_void` instance handle (null on failure).
///
/// `to_runtime_json`, `config_json`, `execution_config` return a
/// byte count: 0 = "no payload"; a value larger than `out_cap` = the
/// required buffer size (caller should resize and retry). On
/// success, `*out_len` receives the bytes written.
#[repr(C)]
pub struct ProcessorVTable {
    /// Vtable layout version. Must equal
    /// [`PROCESSOR_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    // -------------------------------------------------------------------------
    // Constructor + lifetime
    // -------------------------------------------------------------------------

    /// Build a processor instance from msgpack-encoded `Config`
    /// bytes. Returns a thin opaque pointer the cdylib's wrappers
    /// cast back to `*mut P::Processor`. Null = failure (message in
    /// `err_buf`).
    pub construct: unsafe extern "C" fn(
        config_msgpack_ptr: *const u8,
        config_msgpack_len: usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> *mut c_void,

    /// Free the heap allocation `construct` returned. Equivalent to
    /// `Box::from_raw(instance as *mut P::Processor)` + drop on the
    /// cdylib side.
    pub destroy: unsafe extern "C" fn(instance: *mut c_void),

    // -------------------------------------------------------------------------
    // Async lifecycle (block_on'd inside cdylib using host's tokio handle)
    // -------------------------------------------------------------------------

    pub setup: unsafe extern "C" fn(
        instance: *mut c_void,
        ctx_full: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    pub teardown: unsafe extern "C" fn(
        instance: *mut c_void,
        ctx_full: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    pub on_pause: unsafe extern "C" fn(
        instance: *mut c_void,
        ctx_limited: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    pub on_resume: unsafe extern "C" fn(
        instance: *mut c_void,
        ctx_limited: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // Sync lifecycle
    // -------------------------------------------------------------------------

    pub process: unsafe extern "C" fn(
        instance: *mut c_void,
        ctx_limited: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Manual-mode start. Returns non-zero with an error message for
    /// non-Manual processors.
    pub start: unsafe extern "C" fn(
        instance: *mut c_void,
        ctx_full: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Manual-mode stop. Returns non-zero with an error message for
    /// non-Manual processors.
    pub stop: unsafe extern "C" fn(
        instance: *mut c_void,
        ctx_full: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // Static info
    // -------------------------------------------------------------------------

    /// Serialize the processor's [`ExecutionConfig`] to msgpack bytes.
    /// Return value follows the byte-count convention documented on
    /// the struct.
    pub execution_config_msgpack: unsafe extern "C" fn(
        instance: *const c_void,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
    ) -> usize,

    // -------------------------------------------------------------------------
    // Iceoryx2 wiring (host-allocates ownership flip — issue #894)
    //
    // The shared-Rust-type crossings (`Arc<OutputWriter>` /
    // `&mut InputMailboxes`) are retired. The host allocates the
    // `OutputWriterInner` and `InputMailboxesInner` and hands the
    // cdylib opaque `(handle, vtable)` PluginAbiObjects via
    // `set_iceoryx2_resources`. Per-frame `write_raw` / `read_raw`
    // dispatch through the new
    // [`crate::OutputWriterVTable`] / [`crate::InputMailboxesVTable`] slots.
    // -------------------------------------------------------------------------

    pub has_iceoryx2_outputs: unsafe extern "C" fn(instance: *const c_void) -> bool,
    pub has_iceoryx2_inputs: unsafe extern "C" fn(instance: *const c_void) -> bool,

    /// Install host-allocated `OutputWriter` and `InputMailboxes`
    /// PluginAbiObject handles into the cdylib's processor instance.
    ///
    /// Called by the host once per processor instance after
    /// `construct` returns and before any port connections are
    /// wired. The cdylib's `from_config` initializes its `outputs`
    /// / `inputs` PluginAbiObject fields with null `handle` + null `vtable`;
    /// this callback patches in the host-allocated handles so the
    /// per-frame `write_raw` / `read_raw` calls in `process()` see
    /// non-null handles.
    ///
    /// `output_writer_handle` is an `Arc::into_raw(Arc<OutputWriterInner>)`
    /// opaque pointer; the cdylib owns one strong reference and
    /// balances it via `OutputWriterVTable::drop_arc` on Drop. Null
    /// when the processor has no outputs (the cdylib then keeps
    /// the field's null PluginAbiObject and never dispatches through it).
    ///
    /// `input_mailboxes_handle` is an `Arc::into_raw(Arc<InputMailboxesInner>)`
    /// opaque pointer with the same lifetime contract. Null when
    /// the processor has no inputs.
    ///
    /// `output_writer_vtable` / `input_mailboxes_vtable` are
    /// `&'static` pointers to the host's vtables (sourced from
    /// [`crate::HostServices::output_writer_vtable`] /
    /// [`crate::HostServices::input_mailboxes_vtable`]). Layout-versions on
    /// both vtables are validated at install time so the cdylib can
    /// dereference without re-checking.
    ///
    /// Returns `0` on success, non-zero on failure (e.g., processor
    /// has no `outputs` / `inputs` field for a non-null handle —
    /// shape mismatch between host and cdylib).
    pub set_iceoryx2_resources: unsafe extern "C" fn(
        instance: *mut c_void,
        output_writer_handle: *const c_void,
        output_writer_vtable: *const OutputWriterVTable,
        input_mailboxes_handle: *const c_void,
        input_mailboxes_vtable: *const InputMailboxesVTable,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // Config / state IO (msgpack bytes on the wire)
    // -------------------------------------------------------------------------

    /// Apply a runtime-reconfigure update. The bytes are
    /// msgpack-encoded `P::Config` (matches `construct`'s payload
    /// shape).
    pub apply_config_msgpack: unsafe extern "C" fn(
        instance: *mut c_void,
        config_msgpack_ptr: *const u8,
        config_msgpack_len: usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Serialize the processor's runtime state to msgpack. Return
    /// value follows the byte-count convention; 0 = no state.
    pub to_runtime_msgpack: unsafe extern "C" fn(
        instance: *const c_void,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
    ) -> usize,

    /// Serialize the processor's current config to msgpack. Return
    /// value follows the byte-count convention; 0 = no config.
    pub config_msgpack: unsafe extern "C" fn(
        instance: *const c_void,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
    ) -> usize,
}

// Safety: every field is a primitive or a fn pointer. The vtable's
// `&'static` storage on the cdylib side outlives the cdylib's
// process lifetime via `LOADED_PLUGIN_LIBRARIES` pinning.
unsafe impl Send for ProcessorVTable {}
unsafe impl Sync for ProcessorVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn processor_vtable_layout() {
        // v2 (issue #894): the two shared-Rust-type slots
        // `get_iceoryx2_output_writer_arc` and
        // `get_iceoryx2_input_mailboxes_mut` are replaced by a single
        // `set_iceoryx2_resources` slot. 17 - 2 + 1 = 16 fn pointers.
        // header (u32 + u32) + 16 fn pointers @ 8 bytes each =
        // 4 + 4 + 16 * 8 = 136 bytes.
        assert_eq!(size_of::<ProcessorVTable>(), 136);
        assert_eq!(align_of::<ProcessorVTable>(), 8);
        assert_eq!(offset_of!(ProcessorVTable, layout_version), 0);
        assert_eq!(offset_of!(ProcessorVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(ProcessorVTable, construct), 8);
        assert_eq!(offset_of!(ProcessorVTable, destroy), 16);
        assert_eq!(offset_of!(ProcessorVTable, setup), 24);
        assert_eq!(offset_of!(ProcessorVTable, teardown), 32);
        assert_eq!(offset_of!(ProcessorVTable, on_pause), 40);
        assert_eq!(offset_of!(ProcessorVTable, on_resume), 48);
        assert_eq!(offset_of!(ProcessorVTable, process), 56);
        assert_eq!(offset_of!(ProcessorVTable, start), 64);
        assert_eq!(offset_of!(ProcessorVTable, stop), 72);
        assert_eq!(offset_of!(ProcessorVTable, execution_config_msgpack), 80);
        assert_eq!(offset_of!(ProcessorVTable, has_iceoryx2_outputs), 88);
        assert_eq!(offset_of!(ProcessorVTable, has_iceoryx2_inputs), 96);
        assert_eq!(offset_of!(ProcessorVTable, set_iceoryx2_resources), 104);
        assert_eq!(offset_of!(ProcessorVTable, apply_config_msgpack), 112);
        assert_eq!(offset_of!(ProcessorVTable, to_runtime_msgpack), 120);
        assert_eq!(offset_of!(ProcessorVTable, config_msgpack), 128);
    }
}
