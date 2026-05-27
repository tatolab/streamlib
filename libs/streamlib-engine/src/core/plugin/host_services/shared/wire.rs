// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared wire-format helpers for host-side vtable callbacks.
//!
//! Every host-side `extern "C" fn` in the sibling submodules uses one
//! or more of these to (a) read a UTF-8 / byte slice from a
//! `(ptr, len)` pair handed across the FFI, or (b) write an error
//! message / id bytes into a `(out_buf, out_buf_cap, out_len)` triple
//! the cdylib provided. Lifting them out of `mod.rs` keeps the
//! per-vtable submodules from each carrying a private copy and pins
//! the wire-format conventions in one place.

/// Borrow a byte slice from a `(ptr, len)` pair. Returns an empty
/// slice when `ptr` is null or `len` is zero.
///
/// # Safety
///
/// `ptr` must be valid for reads of `len` bytes for the duration of
/// the dispatch. The returned slice's `'static` lifetime is a
/// stand-in for "tied to the caller's `(ptr, len)` argument" — the
/// callback body must not store the slice past return.
pub(in crate::core::plugin::host_services) unsafe fn slice_from_raw(ptr: *const u8, len: usize) -> &'static [u8] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: caller-supplied UTF-8 byte slice; the lifetime is
    // bounded by the dispatch (we never store the slice past return).
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// Write an error message into a `(err_buf, err_buf_cap, err_len)`
/// triple. Truncates the message at `err_buf_cap`; writes the
/// would-have-been length into `*err_len` regardless.
pub(in crate::core::plugin::host_services) fn write_err(
    msg: &str,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) {
    let bytes = msg.as_bytes();
    let written = bytes.len().min(err_buf_cap);
    if written > 0 && !err_buf.is_null() {
        // SAFETY: caller-provided `err_buf` is writable for `err_buf_cap`.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), err_buf, written) };
    }
    if !err_len.is_null() {
        // SAFETY: caller-provided `err_len` is writable.
        unsafe { *err_len = written };
    }
}

/// Write id bytes into a `(out_buf, out_buf_cap, out_len)` triple
/// using the runtime-id / processor-id convention: write the smaller
/// of `bytes.len()` and `out_buf_cap`, set `*out_len` to the number
/// of bytes written, and return the full `bytes.len()` (the caller
/// can detect truncation by comparing `out_len` to the return value).
pub(in crate::core::plugin::host_services) fn write_id_bytes(
    bytes: &[u8],
    out_buf: *mut u8,
    out_buf_cap: usize,
    out_len: *mut usize,
) -> usize {
    let required = bytes.len();
    let written = required.min(out_buf_cap);
    if written > 0 && !out_buf.is_null() {
        // SAFETY: caller guarantees `out_buf` is writable for
        // `out_buf_cap` bytes; we only write `written` bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, written) };
    }
    if !out_len.is_null() {
        // SAFETY: caller guarantees `out_len` is writable.
        unsafe { *out_len = written };
    }
    required
}
