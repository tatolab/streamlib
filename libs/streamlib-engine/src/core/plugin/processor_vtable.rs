// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-processor [`ProcessorVTable`] construction.
//!
//! [`vtable_for::<P>()`] returns a `&'static ProcessorVTable` whose
//! extern "C" fn pointers all dispatch to `P`'s implementations of
//! [`GeneratedProcessor`]'s methods. Each P gets its own monomorphized
//! wrapper set and its own leaked vtable (looked up by `TypeId`); two
//! calls with the same P return the same `&'static` pointer.
//!
//! The wrappers cast the opaque `*mut c_void` instance handle back
//! to `*mut P` and call through `P`'s methods. Lifecycle errors are
//! written into a caller-provided UTF-8 scratch buffer; async
//! lifecycle wrappers `block_on` using the tokio handle they pull
//! from the `RuntimeContext*Access` they receive as a parameter.
//!
//! # Where this fits in the engine model
//!
//! This module is the shared monomorphization point for the new
//! plugin ABI's processor dispatch. Both the cdylib path
//! (`RegisterHelper::register::<P>()` in `host_services`) and the
//! host-static path (`ProcessorInstanceFactory::register::<P>()`)
//! call [`vtable_for::<P>()`] — the resulting vtable lives in the
//! DSO of the caller, but the shape is identical.

use std::any::TypeId;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use streamlib_plugin_abi::{ProcessorVTable, PROCESSOR_VTABLE_LAYOUT_VERSION};

use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::processors::{Config, GeneratedProcessor};

/// Map from `TypeId::of::<P>()` to the leaked `&'static
/// ProcessorVTable` for that P. One vtable per processor type in
/// the DSO's process address space; rebuilt only on the first
/// `vtable_for::<P>()` call (subsequent calls hit the cache).
static VTABLES: OnceLock<Mutex<HashMap<TypeId, &'static ProcessorVTable>>> = OnceLock::new();

/// Returns a `&'static ProcessorVTable` whose entries dispatch
/// every host-called method to `P`'s implementation. Looked up by
/// `TypeId` and cached per process.
pub fn vtable_for<P>() -> &'static ProcessorVTable
where
    P: GeneratedProcessor + 'static,
    P::Config: Config,
{
    let cache = VTABLES.get_or_init(|| Mutex::new(HashMap::new()));
    let type_id = TypeId::of::<P>();

    if let Some(vtable) = cache.lock().unwrap().get(&type_id) {
        return *vtable;
    }

    let new_vtable: &'static ProcessorVTable = Box::leak(Box::new(build_vtable::<P>()));
    let mut guard = cache.lock().unwrap();
    *guard.entry(type_id).or_insert(new_vtable)
}

fn build_vtable<P>() -> ProcessorVTable
where
    P: GeneratedProcessor + 'static,
    P::Config: Config,
{
    ProcessorVTable {
        layout_version: PROCESSOR_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        construct: ProcessorWrappers::<P>::construct,
        destroy: ProcessorWrappers::<P>::destroy,
        setup: ProcessorWrappers::<P>::setup,
        teardown: ProcessorWrappers::<P>::teardown,
        on_pause: ProcessorWrappers::<P>::on_pause,
        on_resume: ProcessorWrappers::<P>::on_resume,
        process: ProcessorWrappers::<P>::process,
        start: ProcessorWrappers::<P>::start,
        stop: ProcessorWrappers::<P>::stop,
        execution_config_msgpack: ProcessorWrappers::<P>::execution_config_msgpack,
        has_iceoryx2_outputs: ProcessorWrappers::<P>::has_iceoryx2_outputs,
        has_iceoryx2_inputs: ProcessorWrappers::<P>::has_iceoryx2_inputs,
        get_iceoryx2_output_writer_arc:
            ProcessorWrappers::<P>::get_iceoryx2_output_writer_arc,
        get_iceoryx2_input_mailboxes_mut:
            ProcessorWrappers::<P>::get_iceoryx2_input_mailboxes_mut,
        apply_config_msgpack: ProcessorWrappers::<P>::apply_config_msgpack,
        to_runtime_msgpack: ProcessorWrappers::<P>::to_runtime_msgpack,
        config_msgpack: ProcessorWrappers::<P>::config_msgpack,
    }
}

// =============================================================================
// ProcessorWrappers<P> — per-P monomorphized extern "C" wrappers
// =============================================================================

struct ProcessorWrappers<P>(std::marker::PhantomData<P>);

impl<P> ProcessorWrappers<P>
where
    P: GeneratedProcessor + 'static,
    P::Config: Config,
{
    unsafe extern "C" fn construct(
        config_msgpack_ptr: *const u8,
        config_msgpack_len: usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> *mut c_void {
        let config: P::Config = if config_msgpack_len == 0 || config_msgpack_ptr.is_null() {
            P::Config::default()
        } else {
            let bytes = unsafe {
                std::slice::from_raw_parts(config_msgpack_ptr, config_msgpack_len)
            };
            match rmp_serde::from_slice(bytes) {
                Ok(c) => c,
                Err(e) => {
                    write_err(err_buf, err_buf_cap, err_len, &format!("config deser: {e}"));
                    return std::ptr::null_mut();
                }
            }
        };

        match P::from_config(config) {
            Ok(processor) => Box::into_raw(Box::new(processor)) as *mut c_void,
            Err(e) => {
                write_err(err_buf, err_buf_cap, err_len, &e.to_string());
                std::ptr::null_mut()
            }
        }
    }

    unsafe extern "C" fn destroy(instance: *mut c_void) {
        if !instance.is_null() {
            // SAFETY: instance was produced by Box::into_raw above on this
            // DSO's heap. Box::from_raw + drop releases on the same heap.
            unsafe {
                drop(Box::from_raw(instance as *mut P));
            }
        }
    }

    unsafe extern "C" fn setup(
        instance: *mut c_void,
        ctx_full: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32 {
        let processor = unsafe { &mut *(instance as *mut P) };
        let ctx = unsafe { &*(ctx_full as *const RuntimeContextFullAccess<'_>) };
        let result = ctx
            .tokio_handle()
            .block_on(<P as GeneratedProcessor>::__generated_setup(processor, ctx));
        match result {
            Ok(()) => 0,
            Err(e) => {
                write_err(err_buf, err_buf_cap, err_len, &e.to_string());
                -1
            }
        }
    }

    unsafe extern "C" fn teardown(
        instance: *mut c_void,
        ctx_full: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32 {
        let processor = unsafe { &mut *(instance as *mut P) };
        let ctx = unsafe { &*(ctx_full as *const RuntimeContextFullAccess<'_>) };
        let result = ctx
            .tokio_handle()
            .block_on(<P as GeneratedProcessor>::__generated_teardown(
                processor, ctx,
            ));
        match result {
            Ok(()) => 0,
            Err(e) => {
                write_err(err_buf, err_buf_cap, err_len, &e.to_string());
                -1
            }
        }
    }

    unsafe extern "C" fn on_pause(
        instance: *mut c_void,
        ctx_limited: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32 {
        let processor = unsafe { &mut *(instance as *mut P) };
        let ctx = unsafe { &*(ctx_limited as *const RuntimeContextLimitedAccess<'_>) };
        let result = ctx
            .tokio_handle()
            .block_on(<P as GeneratedProcessor>::__generated_on_pause(
                processor, ctx,
            ));
        match result {
            Ok(()) => 0,
            Err(e) => {
                write_err(err_buf, err_buf_cap, err_len, &e.to_string());
                -1
            }
        }
    }

    unsafe extern "C" fn on_resume(
        instance: *mut c_void,
        ctx_limited: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32 {
        let processor = unsafe { &mut *(instance as *mut P) };
        let ctx = unsafe { &*(ctx_limited as *const RuntimeContextLimitedAccess<'_>) };
        let result = ctx
            .tokio_handle()
            .block_on(<P as GeneratedProcessor>::__generated_on_resume(
                processor, ctx,
            ));
        match result {
            Ok(()) => 0,
            Err(e) => {
                write_err(err_buf, err_buf_cap, err_len, &e.to_string());
                -1
            }
        }
    }

    unsafe extern "C" fn process(
        instance: *mut c_void,
        ctx_limited: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32 {
        let processor = unsafe { &mut *(instance as *mut P) };
        let ctx = unsafe { &*(ctx_limited as *const RuntimeContextLimitedAccess<'_>) };
        match <P as GeneratedProcessor>::process(processor, ctx) {
            Ok(()) => 0,
            Err(e) => {
                write_err(err_buf, err_buf_cap, err_len, &e.to_string());
                -1
            }
        }
    }

    unsafe extern "C" fn start(
        instance: *mut c_void,
        ctx_full: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32 {
        let processor = unsafe { &mut *(instance as *mut P) };
        let ctx = unsafe { &*(ctx_full as *const RuntimeContextFullAccess<'_>) };
        match <P as GeneratedProcessor>::start(processor, ctx) {
            Ok(()) => 0,
            Err(e) => {
                write_err(err_buf, err_buf_cap, err_len, &e.to_string());
                -1
            }
        }
    }

    unsafe extern "C" fn stop(
        instance: *mut c_void,
        ctx_full: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32 {
        let processor = unsafe { &mut *(instance as *mut P) };
        let ctx = unsafe { &*(ctx_full as *const RuntimeContextFullAccess<'_>) };
        match <P as GeneratedProcessor>::stop(processor, ctx) {
            Ok(()) => 0,
            Err(e) => {
                write_err(err_buf, err_buf_cap, err_len, &e.to_string());
                -1
            }
        }
    }

    unsafe extern "C" fn execution_config_msgpack(
        instance: *const c_void,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
    ) -> usize {
        let processor = unsafe { &*(instance as *const P) };
        let cfg = <P as GeneratedProcessor>::execution_config(processor);
        let bytes = match rmp_serde::to_vec_named(&cfg) {
            Ok(b) => b,
            Err(_) => return 0,
        };
        write_out_bytes(&bytes, out_buf, out_cap, out_len)
    }

    unsafe extern "C" fn has_iceoryx2_outputs(instance: *const c_void) -> bool {
        let processor = unsafe { &*(instance as *const P) };
        <P as GeneratedProcessor>::has_iceoryx2_outputs(processor)
    }

    unsafe extern "C" fn has_iceoryx2_inputs(instance: *const c_void) -> bool {
        let processor = unsafe { &*(instance as *const P) };
        <P as GeneratedProcessor>::has_iceoryx2_inputs(processor)
    }

    unsafe extern "C" fn get_iceoryx2_output_writer_arc(
        instance: *const c_void,
    ) -> *const c_void {
        let processor = unsafe { &*(instance as *const P) };
        match <P as GeneratedProcessor>::get_iceoryx2_output_writer(processor) {
            // SAFETY: into_raw transfers one strong reference to the
            // caller. Caller must `Arc::from_raw` exactly once.
            Some(arc) => std::sync::Arc::into_raw(arc) as *const c_void,
            None => std::ptr::null(),
        }
    }

    unsafe extern "C" fn get_iceoryx2_input_mailboxes_mut(
        instance: *mut c_void,
    ) -> *mut c_void {
        let processor = unsafe { &mut *(instance as *mut P) };
        match <P as GeneratedProcessor>::get_iceoryx2_input_mailboxes(processor) {
            Some(mailboxes) => mailboxes as *mut _ as *mut c_void,
            None => std::ptr::null_mut(),
        }
    }

    unsafe extern "C" fn apply_config_msgpack(
        instance: *mut c_void,
        config_msgpack_ptr: *const u8,
        config_msgpack_len: usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32 {
        let processor = unsafe { &mut *(instance as *mut P) };
        let bytes = if config_msgpack_len == 0 || config_msgpack_ptr.is_null() {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(config_msgpack_ptr, config_msgpack_len) }
        };
        let config: P::Config = match rmp_serde::from_slice(bytes) {
            Ok(c) => c,
            Err(e) => {
                write_err(
                    err_buf,
                    err_buf_cap,
                    err_len,
                    &format!("apply_config_msgpack deser: {e}"),
                );
                return -1;
            }
        };
        match <P as GeneratedProcessor>::update_config(processor, config) {
            Ok(()) => 0,
            Err(e) => {
                write_err(err_buf, err_buf_cap, err_len, &e.to_string());
                -2
            }
        }
    }

    unsafe extern "C" fn to_runtime_msgpack(
        instance: *const c_void,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
    ) -> usize {
        let processor = unsafe { &*(instance as *const P) };
        let value = <P as GeneratedProcessor>::to_runtime_json(processor);
        let bytes = match rmp_serde::to_vec_named(&value) {
            Ok(b) => b,
            Err(_) => return 0,
        };
        write_out_bytes(&bytes, out_buf, out_cap, out_len)
    }

    unsafe extern "C" fn config_msgpack(
        instance: *const c_void,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
    ) -> usize {
        let processor = unsafe { &*(instance as *const P) };
        let value = <P as GeneratedProcessor>::config_json(processor);
        let bytes = match rmp_serde::to_vec_named(&value) {
            Ok(b) => b,
            Err(_) => return 0,
        };
        write_out_bytes(&bytes, out_buf, out_cap, out_len)
    }
}

// =============================================================================
// Scratch-buffer helpers
// =============================================================================

fn write_err(buf: *mut u8, cap: usize, out_len: *mut usize, msg: &str) {
    if buf.is_null() || out_len.is_null() {
        return;
    }
    let bytes = msg.as_bytes();
    let n = bytes.len().min(cap);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, n);
        *out_len = n;
    }
}

/// Writes `bytes` to `out_buf` when it fits within `out_cap`. Always
/// returns the **required** buffer size (`bytes.len()`); the caller
/// inspects that value vs. `out_cap` to detect truncation.
fn write_out_bytes(
    bytes: &[u8],
    out_buf: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
) -> usize {
    if bytes.len() <= out_cap && !out_buf.is_null() && !out_len.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, bytes.len());
            *out_len = bytes.len();
        }
    } else if !out_len.is_null() {
        unsafe {
            *out_len = 0;
        }
    }
    bytes.len()
}
