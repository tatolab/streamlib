// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `OutputWriterVTable` — extern "C" dispatch for the cdylib's `OutputWriter` PluginAbiObject.

use core::ffi::c_void;

/// Layout version of [`crate::OutputWriterVTable`].
///
/// - v1: ships the four slots a cdylib processor's `OutputWriter`
///   PluginAbiObject needs to dispatch every public-API call through the
///   host: `write_raw` (the per-frame hot-path emit), `has_port`
///   (configuration query), `clone_arc` / `drop_arc`
///   (refcount-managed handle lifetime so the cdylib-side PluginAbiObject
///   can implement `Clone` + `Drop` without crossing the inner
///   `Arc<OutputWriterInner>` source layout).
pub const OUTPUT_WRITER_VTABLE_LAYOUT_VERSION: u32 = 1;

/// `extern "C" fn` dispatch table for the cdylib's `OutputWriter`
/// PluginAbiObject. Replaces the shared-Rust-type `Arc<OutputWriter>`
/// crossing the cdylib used to expose to the host via
/// `ProcessorVTable::get_iceoryx2_output_writer_arc`.
///
/// Today the host allocates an `Arc<OutputWriterInner>` and hands
/// the cdylib a `(handle, vtable)` PluginAbiObject that delegates every
/// public-API call through this vtable. Hot-path emits cross extern
/// "C" once per `write` call; the bytes carry msgpack-encoded
/// frames the cdylib serialized in its own plugin.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0. Older vtables loaded
/// into newer hosts are rejected cleanly. New fields append after
/// `drop_arc` and bump [`OUTPUT_WRITER_VTABLE_LAYOUT_VERSION`].
///
/// # Error convention
///
/// `write_raw` returns `0` on success, non-zero on failure. The
/// caller-provided `err_buf` / `err_buf_cap` is a UTF-8 scratch
/// buffer the callee writes a message into; `*err_len` receives
/// the actual byte count written. Truncation is benign.
///
/// `clone_arc` / `drop_arc` are infallible — they bump / decrement
/// the host-side `Arc<OutputWriterInner>` strong count. `clone_arc`
/// returns the same opaque handle (the underlying inner is the same
/// object); the cdylib pairs each `clone_arc` with exactly one
/// `drop_arc` to keep refcount accounting balanced.
#[repr(C)]
pub struct OutputWriterVTable {
    /// Vtable layout version. Must equal
    /// [`OUTPUT_WRITER_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned; zero today, never read).
    pub _reserved_padding: u32,

    /// Write a raw msgpack-encoded frame to the named output port at
    /// the given timestamp. The cdylib serializes `T` to msgpack in
    /// its own plugin and passes the bytes through; the host then runs
    /// the underlying iceoryx2 publish + notify. Returns `0` on
    /// success, non-zero on failure.
    pub write_raw: unsafe extern "C" fn(
        handle: *const c_void,
        port_ptr: *const u8,
        port_len: usize,
        data_ptr: *const u8,
        data_len: usize,
        timestamp_ns: i64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Check whether a port has been configured. Returns `true` if
    /// the host's `OutputWriterInner` has a channel publisher
    /// installed for the named port.
    pub has_port:
        unsafe extern "C" fn(handle: *const c_void, port_ptr: *const u8, port_len: usize) -> bool,

    /// Bump the host-side `Arc<OutputWriterInner>` strong count.
    /// Returns the same opaque handle (the cdylib uses the same
    /// handle in subsequent calls). Pairs 1:1 with `drop_arc`.
    pub clone_arc: unsafe extern "C" fn(handle: *const c_void) -> *const c_void,

    /// Decrement the host-side `Arc<OutputWriterInner>` strong
    /// count. Releases the inner when the count reaches zero.
    pub drop_arc: unsafe extern "C" fn(handle: *const c_void),
}

// Safety: every field is a primitive or an `extern "C" fn` pointer.
// The vtable's `&'static` storage outlives the cdylib's process
// lifetime via the `LOADED_PLUGIN_LIBRARIES` pinning shape.
unsafe impl Send for OutputWriterVTable {}
unsafe impl Sync for OutputWriterVTable {}

/// Emit the channel-egress admission tracing shared by the host writer and the
/// subprocess SDK natives' output-write path, so the host, Python, and Deno
/// stay lock-step on the refusal / segment-growth / quarter-of-ceiling
/// diagnostics off the same [`decide_channel_egress_admission`] decision.
/// `trust_tier` labels each line; `log_prefix` is `None` for the host and
/// `Some((runtime_tag, processor_id))` for a native (`"slpn"` / `"sldn"` plus
/// its processor id) to scope the message with a `[tag:id] ` prefix. The caller
/// still maps [`ChannelEgressAdmission::RefusedOverCeiling`] to its own refuse
/// return code or typed error.
///
/// [`decide_channel_egress_admission`]: streamlib_ipc_types::decide_channel_egress_admission
pub fn emit_channel_egress_admission_tracing(
    log_prefix: Option<(&str, &str)>,
    trust_tier: streamlib_ipc_types::ChannelTrustTier,
    channel_service_name: &str,
    channel_ceiling_bytes: usize,
    payload_total_bytes: usize,
    admission: &streamlib_ipc_types::ChannelEgressAdmission,
) {
    use streamlib_ipc_types::ChannelEgressAdmission;

    let prefix = match log_prefix {
        Some((runtime_tag, processor_id)) => format!("[{}:{}] ", runtime_tag, processor_id),
        None => String::new(),
    };

    match admission {
        ChannelEgressAdmission::RefusedOverCeiling { refused_count } => {
            tracing::warn!(
                channel = channel_service_name,
                payload_bytes = payload_total_bytes,
                ceiling_bytes = channel_ceiling_bytes,
                tier = trust_tier.as_str(),
                refused_count = *refused_count,
                "{}output channel refused a payload above its per-channel ceiling",
                prefix,
            );
        }
        ChannelEgressAdmission::Admitted { grew_to } => {
            if let Some(growth) = grew_to {
                tracing::info!(
                    channel = channel_service_name,
                    old_segment_bytes = growth.old_segment_bytes,
                    new_segment_bytes = growth.new_segment_bytes,
                    tier = trust_tier.as_str(),
                    "{}iceoryx2 publisher data segment grew (PowerOfTwo)",
                    prefix,
                );
                if growth.crossed_quarter_ceiling {
                    tracing::warn!(
                        channel = channel_service_name,
                        segment_bytes = growth.new_segment_bytes,
                        ceiling_bytes = channel_ceiling_bytes,
                        tier = trust_tier.as_str(),
                        "{}iceoryx2 publisher segment crossed a quarter of the channel ceiling",
                        prefix,
                    );
                }
            }
        }
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn output_writer_vtable_layout() {
        // header (u32 + u32) + 4 fn pointers @ 8 bytes each =
        // 4 + 4 + 4 * 8 = 40 bytes.
        assert_eq!(size_of::<OutputWriterVTable>(), 40);
        assert_eq!(align_of::<OutputWriterVTable>(), 8);
        assert_eq!(offset_of!(OutputWriterVTable, layout_version), 0);
        assert_eq!(offset_of!(OutputWriterVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(OutputWriterVTable, write_raw), 8);
        assert_eq!(offset_of!(OutputWriterVTable, has_port), 16);
        assert_eq!(offset_of!(OutputWriterVTable, clone_arc), 24);
        assert_eq!(offset_of!(OutputWriterVTable, drop_arc), 32);
    }
}
