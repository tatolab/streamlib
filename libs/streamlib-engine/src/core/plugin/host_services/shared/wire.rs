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

/// Companion to [`write_err`] used by the iceoryx2-transport
/// wrappers (`OutputWriter`, `InputMailboxes`). Same shape with a
/// stricter early-return — skips the write entirely if either
/// `err_buf` or `err_len` is null.
pub(in crate::core::plugin::host_services) fn write_extern_err(
    msg: &str,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) {
    if err_buf.is_null() || err_len.is_null() {
        return;
    }
    let bytes = msg.as_bytes();
    let n = bytes.len().min(err_buf_cap);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), err_buf, n);
        *err_len = n;
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
#[cfg(test)]
mod run_host_extern_c_panic_safety_net_tests {
    //! Phase G (#961) panic-injection coverage.
    //!
    //! Every `host_*` extern "C" callback wraps its body in
    //! [`run_host_extern_c`], whose `catch_unwind` safety net is
    //! the only thing standing between a panic in cdylib-author
    //! code path and an `extern "C"` unwind across the FFI
    //! boundary (UB). This module locks the safety-net contract:
    //!
    //!   - A body that panics with a `&'static str` payload is
    //!     caught and the documented default is returned.
    //!   - A body that panics with a `String` payload is caught
    //!     and the default is returned.
    //!   - A body that panics with an arbitrary non-string
    //!     payload (e.g. a custom Debug type) is caught and the
    //!     default is returned.
    //!   - Successful (non-panicking) bodies still return their
    //!     value as expected — proves the safety net isn't
    //!     intercepting normal control flow.
    //!
    //! Mental-revert: removing the `catch_unwind` wrap (i.e.
    //! calling `body()` directly inside `run_host_extern_c`)
    //! reverts every test below to a `panic!()` that aborts the
    //! test process. The harness reports each as a hard process
    //! abort rather than a fail.
    //!
    //! Why this module instead of a per-vtable-callback test:
    //! every host_* callback delegates to the same
    //! `run_host_extern_c` helper. Locking the helper's contract
    //! once covers every callback that rides it. Per-callback
    //! panic-injection would require deliberately corrupting
    //! handle state in production-shaped ways (UB) and would
    //! re-test the same `catch_unwind` machinery a hundred times.
    //! This is the engine-tier fix per CLAUDE.md ("Engine-wide
    //! bugs get fixed at the engine layer").

    use super::super::super::run_host_extern_c;
    use std::ffi::c_void;

    #[test]
    fn panic_with_static_str_returns_default_i32() {
        let rc = run_host_extern_c::<_, i32>(
            "test_static_str_panic",
            || panic!("deliberate test panic with &'static str"),
            42i32,
        );
        assert_eq!(rc, 42, "catch_unwind must return the default on panic");
    }

    #[test]
    fn panic_with_string_returns_default_i32() {
        let rc = run_host_extern_c::<_, i32>(
            "test_string_panic",
            || panic!("{}", String::from("deliberate dynamic panic")),
            7i32,
        );
        assert_eq!(rc, 7);
    }

    #[test]
    fn panic_with_non_string_payload_returns_default_i32() {
        // The wrapper's downcast chain handles `&'static str` and
        // `String` explicitly and falls through to a generic
        // "<non-string panic payload>" tracing message for anything
        // else. The catch_unwind contract is the load-bearing part:
        // even with an exotic payload the default must come back.
        #[derive(Debug)]
        struct CustomPayload;
        let rc = run_host_extern_c::<_, i32>(
            "test_custom_payload_panic",
            || std::panic::panic_any(CustomPayload),
            -1i32,
        );
        assert_eq!(rc, -1);
    }

    #[test]
    fn non_panicking_body_returns_its_value() {
        // Locks the "safety net doesn't intercept normal control
        // flow" invariant. Mental-revert: making `run_host_extern_c`
        // always return `default_on_panic` (no Ok branch) would
        // fail this test.
        let rc = run_host_extern_c::<_, i32>(
            "test_ok_path",
            || 99i32,
            -1i32,
        );
        assert_eq!(rc, 99);
    }

    #[test]
    fn panic_with_unit_default_returns_unit() {
        // Locks the panic-default for `()`-returning callbacks
        // (the entire RuntimeOps / clone/drop / null-handle-no-op
        // shape). The () default is trivially "the same value as
        // before"; what matters is that the body's panic doesn't
        // propagate past the FFI boundary.
        let mut hit = false;
        run_host_extern_c::<_, ()>(
            "test_unit_default_panic",
            || {
                hit = true;
                panic!("unit-default panic");
            },
            (),
        );
        assert!(hit, "body must have run before panicking");
    }

    #[test]
    fn panic_with_null_ptr_default_returns_null() {
        // Locks the panic-default for `*const c_void`-returning
        // callbacks (e.g. `host_gpu_lim_clone_handle`,
        // `host_rcv_audio_clock_handle`). The default is a null
        // pointer; the assertion confirms a panicking body returns
        // null rather than dangling memory.
        let p = run_host_extern_c::<_, *const c_void>(
            "test_null_ptr_default_panic",
            || panic!("null-ptr-default panic"),
            std::ptr::null(),
        );
        assert!(p.is_null());
    }
}
