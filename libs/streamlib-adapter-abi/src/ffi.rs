// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! FFI-boundary helpers shared by every host-side surface-adapter
//! vtable and by the engine's `host_*` extern "C" trampolines.
//!
//! The canonical [`run_host_extern_c`] panic-safety net lives here so
//! all six consumers (engine + five surface adapters) share a single
//! implementation. The body's tracing target is caller-supplied so each
//! consumer's logs stay in its own namespace (`streamlib::ffi`,
//! `streamlib_adapter_vulkan::ffi`, etc.) without forcing the helper
//! to inspect the call stack.

/// Catch panics at an `extern "C"` boundary so a host-side panic
/// doesn't unwind into cdylib code. On panic, returns
/// `default_on_panic`; the panic message is logged via `tracing` under
/// the `streamlib::ffi` target.
///
/// Wraps every `host_*` extern "C" callback in the engine and in each
/// surface-adapter crate. The cross-crate call sites lock the helper's
/// contract in [`run_host_extern_c_panic_safety_net_tests`].
///
/// Per-adapter log namespacing was previously encoded in the tracing
/// `target` (e.g. `streamlib_adapter_vulkan::ffi`) — this is now
/// folded into the structured `callback` field, which already names
/// the adapter via its prefix (`host_vulkan_*`, `host_opengl_*`,
/// etc.). All FFI panics route under the single
/// `streamlib_adapter_abi::ffi` target; filters that want adapter-
/// specific routing match on `callback`. (The earlier `streamlib::ffi`
/// target string violated the xtask check-boundaries top-level-shortcut
/// rule, so the canonical helper lives under the crate's own path.)
#[inline]
pub fn run_host_extern_c<F, T>(
    callback_name: &'static str,
    body: F,
    default_on_panic: T,
) -> T
where
    F: FnOnce() -> T,
{
    use std::panic::{catch_unwind, AssertUnwindSafe};
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "<non-string panic payload>".to_string()
            };
            tracing::error!(
                target: "streamlib_adapter_abi::ffi",
                callback = callback_name,
                panic = %msg,
                "host extern \"C\" callback panicked; FFI boundary converted panic to default return",
            );
            default_on_panic
        }
    }
}

#[cfg(test)]
mod run_host_extern_c_panic_safety_net_tests {
    //! Lock the helper's panic-catching contract. Mirrors the test
    //! coverage that lived in `streamlib-engine` before the helper
    //! was consolidated here.

    use super::*;

    #[test]
    fn panic_with_static_str_returns_default_i32() {
        let rc = run_host_extern_c("cb_static_str", || -> i32 {
            panic!("static-str panic");
        }, 7);
        assert_eq!(rc, 7);
    }

    #[test]
    fn panic_with_string_returns_default_i32() {
        let rc = run_host_extern_c("cb_string", || -> i32 {
            panic!("{}", String::from("dynamic-string panic"));
        }, 9);
        assert_eq!(rc, 9);
    }

    #[test]
    fn panic_with_non_string_payload_returns_default_i32() {
        let rc = run_host_extern_c("cb_non_string", || -> i32 {
            std::panic::panic_any(0xDEADu16);
        }, 11);
        assert_eq!(rc, 11);
    }

    #[test]
    fn non_panicking_body_returns_its_value() {
        let rc = run_host_extern_c("cb_ok", || -> i32 { 42 }, -1);
        assert_eq!(rc, 42);
    }

    #[test]
    fn panic_with_unit_default_returns_unit() {
        let _: () = run_host_extern_c("cb_unit", || -> () {
            panic!("unit-default panic");
        }, ());
    }

    #[test]
    fn panic_with_null_ptr_default_returns_null() {
        let ptr = run_host_extern_c("cb_null_ptr", || -> *const u8 {
            panic!("null-ptr default panic");
        }, std::ptr::null());
        assert!(ptr.is_null());
    }
}
