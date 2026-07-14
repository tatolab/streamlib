// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Hand-written FFI declarations for the subset of `libnvjpeg.so.12` the
//! backend uses. Resolved via [`libloading`] at runtime — the workspace
//! builds cleanly on hosts without nvJPEG installed; failure to `dlopen`
//! surfaces as [`Error::NotSupported`] from
//! [`crate::nvjpeg_backend::NvJpegBackend::new`].
//!
//! Symbols mirror NVIDIA's `nvjpeg.h` from the CUDA Toolkit 12.x line. The
//! C API is stable across the 12.x major: signatures match the header
//! verbatim, and we resolve a small subset (handle init/destroy, state
//! init/destroy, image-info query, decode).
//!
//! [`Error::NotSupported`]: streamlib::sdk::error::Error::NotSupported

#![allow(non_camel_case_types, non_upper_case_globals, dead_code)]

use std::ffi::{c_int, c_uint, c_void};
use std::sync::Arc;

use libloading::{Library, Symbol};
use streamlib::sdk::error::{Error, Result};

/// Opaque handle type — `struct nvjpegHandle*`.
pub type nvjpegHandle_t = *mut c_void;
/// Opaque per-decoder state — `struct nvjpegJpegState*`.
pub type nvjpegJpegState_t = *mut c_void;
/// CUDA stream handle — `cudaStream_t` is `void*` in CUDA runtime.
pub type cudaStream_t = *mut c_void;

/// `nvjpegStatus_t` — full enum from `nvjpeg.h`. We only special-case
/// `NVJPEG_STATUS_SUCCESS`; other variants map to a descriptive error
/// string.
pub const NVJPEG_STATUS_SUCCESS: c_int = 0;

/// `NVJPEG_MAX_COMPONENT` from the header — fixed at 4 for the 12.x
/// API. Matches `nvjpegImage_t::channel[]` / `pitch[]` array sizing.
pub const NVJPEG_MAX_COMPONENT: usize = 4;

/// `nvjpegOutputFormat_t` — we only use `NVJPEG_OUTPUT_RGBI`
/// (interleaved RGB, 3 bytes per pixel in `channel[0]`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum nvjpegOutputFormat_t {
    Unchanged = 0,
    Yuv = 1,
    Y = 2,
    Rgb = 3,
    Bgr = 4,
    Rgbi = 5,
    Bgri = 6,
}

/// `nvjpegChromaSubsampling_t` from the header. Returned by
/// `nvjpegGetImageInfo`; we forward it to the caller for tracing but
/// don't gate on it (nvJPEG accepts any subsampling on the decode path).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum nvjpegChromaSubsampling_t {
    Css444 = 0,
    Css422 = 1,
    Css420 = 2,
    Css440 = 3,
    Css411 = 4,
    Css410 = 5,
    CssGray = 6,
    Css410v = 7,
    CssUnknown = -1,
}

/// `nvjpegImage_t` — caller-supplied output descriptor passed to
/// `nvjpegDecode`. For `NVJPEG_OUTPUT_RGBI` only `channel[0]` /
/// `pitch[0]` are used.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct nvjpegImage_t {
    pub channel: [*mut u8; NVJPEG_MAX_COMPONENT],
    pub pitch: [usize; NVJPEG_MAX_COMPONENT],
}

impl Default for nvjpegImage_t {
    fn default() -> Self {
        Self {
            channel: [std::ptr::null_mut(); NVJPEG_MAX_COMPONENT],
            pitch: [0; NVJPEG_MAX_COMPONENT],
        }
    }
}

// ---- FFI signatures ----
//
// All return `nvjpegStatus_t` (i.e. `c_int`). Status codes are
// translated via `status_to_error` below.

type nvjpegCreateSimpleFn = unsafe extern "C" fn(handle: *mut nvjpegHandle_t) -> c_int;
type nvjpegDestroyFn = unsafe extern "C" fn(handle: nvjpegHandle_t) -> c_int;
type nvjpegJpegStateCreateFn =
    unsafe extern "C" fn(handle: nvjpegHandle_t, state: *mut nvjpegJpegState_t) -> c_int;
type nvjpegJpegStateDestroyFn = unsafe extern "C" fn(state: nvjpegJpegState_t) -> c_int;
type nvjpegGetImageInfoFn = unsafe extern "C" fn(
    handle: nvjpegHandle_t,
    data: *const u8,
    length: usize,
    n_components: *mut c_int,
    subsampling: *mut nvjpegChromaSubsampling_t,
    widths: *mut c_int,
    heights: *mut c_int,
) -> c_int;
type nvjpegDecodeFn = unsafe extern "C" fn(
    handle: nvjpegHandle_t,
    state: nvjpegJpegState_t,
    data: *const u8,
    length: usize,
    output_format: nvjpegOutputFormat_t,
    destination: *mut nvjpegImage_t,
    stream: cudaStream_t,
) -> c_int;

/// Loaded `libnvjpeg.so.12` + every symbol we use, resolved once at
/// construction. Cheap to clone (`Arc<Library>` underneath, raw fn
/// pointers).
#[derive(Clone)]
pub struct NvJpegLib {
    // Keeping `_library` alive in the struct ensures the resolved
    // function pointers stay valid; dropping the lib would `dlclose`
    // and invalidate them.
    _library: Arc<Library>,
    pub create_simple: nvjpegCreateSimpleFn,
    pub destroy: nvjpegDestroyFn,
    pub state_create: nvjpegJpegStateCreateFn,
    pub state_destroy: nvjpegJpegStateDestroyFn,
    pub get_image_info: nvjpegGetImageInfoFn,
    pub decode: nvjpegDecodeFn,
}

impl std::fmt::Debug for NvJpegLib {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NvJpegLib").finish_non_exhaustive()
    }
}

impl NvJpegLib {
    /// Resolve `libnvjpeg.so.12` and every symbol we use. Returns
    /// [`Error::NotSupported`] when the dynamic linker can't find the
    /// library — that's the canonical "no nvJPEG on this host" signal
    /// the auto-selection path checks. Tries the bare SONAME first
    /// (works when `ldconfig` has refreshed since the CUDA Toolkit
    /// install) then falls back to canonical CUDA Toolkit install
    /// paths — see [`probe_nvjpeg_loadable_paths`] for the list.
    pub fn load() -> Result<Self> {
        // SAFETY: `libloading::Library::new` is safe per its contract —
        // failure returns `Err`, not undefined behavior. The library is
        // held in `Arc<Library>` to keep `_library` (and thus the
        // resolved symbol addresses) alive for the struct's lifetime.
        let mut last_err: Option<libloading::Error> = None;
        let mut loaded: Option<Library> = None;
        for candidate in probe_nvjpeg_loadable_paths() {
            match unsafe { Library::new(candidate) } {
                Ok(lib) => {
                    loaded = Some(lib);
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        let library = loaded.ok_or_else(|| {
            let err_msg = last_err.map_or_else(
                || "no candidate paths".to_string(),
                |e| format!("{e}"),
            );
            Error::NotSupported(format!(
                "libnvjpeg.so.12 not loadable: {err_msg} (install libnvjpeg-dev-12-* \
                 from the NVIDIA CUDA apt repository, or `sudo ldconfig` if just installed)"
            ))
        })?;
        let library = Arc::new(library);

        // SAFETY: each symbol lookup either succeeds with a non-null
        // function pointer whose signature matches the C ABI per
        // `nvjpeg.h`, or returns `Err`. Resolved fn pointers are copied
        // out before the `Symbol` borrow ends; calling them through
        // their FFI types stays sound for as long as `_library` keeps
        // the underlying `Library` alive.
        let (create_simple, destroy, state_create, state_destroy, get_image_info, decode) = unsafe {
            let create_simple: Symbol<nvjpegCreateSimpleFn> = library
                .get(b"nvjpegCreateSimple\0")
                .map_err(|e| symbol_err("nvjpegCreateSimple", e))?;
            let destroy: Symbol<nvjpegDestroyFn> = library
                .get(b"nvjpegDestroy\0")
                .map_err(|e| symbol_err("nvjpegDestroy", e))?;
            let state_create: Symbol<nvjpegJpegStateCreateFn> = library
                .get(b"nvjpegJpegStateCreate\0")
                .map_err(|e| symbol_err("nvjpegJpegStateCreate", e))?;
            let state_destroy: Symbol<nvjpegJpegStateDestroyFn> = library
                .get(b"nvjpegJpegStateDestroy\0")
                .map_err(|e| symbol_err("nvjpegJpegStateDestroy", e))?;
            let get_image_info: Symbol<nvjpegGetImageInfoFn> = library
                .get(b"nvjpegGetImageInfo\0")
                .map_err(|e| symbol_err("nvjpegGetImageInfo", e))?;
            let decode: Symbol<nvjpegDecodeFn> = library
                .get(b"nvjpegDecode\0")
                .map_err(|e| symbol_err("nvjpegDecode", e))?;
            (
                *create_simple,
                *destroy,
                *state_create,
                *state_destroy,
                *get_image_info,
                *decode,
            )
        };

        Ok(Self {
            _library: library,
            create_simple,
            destroy,
            state_create,
            state_destroy,
            get_image_info,
            decode,
        })
    }
}

/// Candidate paths the dynamic linker tries for `libnvjpeg.so.12`,
/// in priority order. Matches the engine's
/// `probe_nvjpeg_loadable` fallback list — keep the two in sync. A
/// fresh `libnvjpeg-dev-12-*` apt install often leaves the ldconfig
/// cache stale until the next `ldconfig` run; falling back to
/// explicit paths means runtime backend selection doesn't depend on
/// root-only cache state.
pub(crate) fn probe_nvjpeg_loadable_paths() -> &'static [&'static str] {
    &[
        "libnvjpeg.so.12",
        "/usr/local/cuda/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.9/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.8/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.7/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.6/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.5/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.4/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.3/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.2/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.1/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.0/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/lib/x86_64-linux-gnu/libnvjpeg.so.12",
    ]
}

fn symbol_err(symbol: &str, e: libloading::Error) -> Error {
    Error::GpuError(format!(
        "libnvjpeg.so.12 found but symbol {symbol} unresolvable: {e} \
         (ABI mismatch — nvJPEG header may be older than the loaded library)"
    ))
}

/// Translate a non-success `nvjpegStatus_t` into a `streamlib::Error`.
pub fn status_to_error(context: &str, status: c_int) -> Error {
    if status == NVJPEG_STATUS_SUCCESS {
        // Defensive — callers should only reach this with a non-success
        // status, but a guard makes misuse obvious.
        return Error::GpuError(format!(
            "{context}: unexpected status_to_error call with SUCCESS (0)"
        ));
    }
    Error::GpuError(format!(
        "{context}: nvjpegStatus_t = {status} ({})",
        nvjpeg_status_label(status),
    ))
}

/// Best-effort human-readable label for a `nvjpegStatus_t`. The exact
/// enum identifiers come from `nvjpeg.h`.
fn nvjpeg_status_label(status: c_int) -> &'static str {
    match status {
        0 => "NVJPEG_STATUS_SUCCESS",
        1 => "NVJPEG_STATUS_NOT_INITIALIZED",
        2 => "NVJPEG_STATUS_INVALID_PARAMETER",
        3 => "NVJPEG_STATUS_BAD_JPEG",
        4 => "NVJPEG_STATUS_JPEG_NOT_SUPPORTED",
        5 => "NVJPEG_STATUS_ALLOCATOR_FAILURE",
        6 => "NVJPEG_STATUS_EXECUTION_FAILED",
        7 => "NVJPEG_STATUS_ARCH_MISMATCH",
        8 => "NVJPEG_STATUS_INTERNAL_ERROR",
        9 => "NVJPEG_STATUS_IMPLEMENTATION_NOT_SUPPORTED",
        10 => "NVJPEG_STATUS_INCOMPLETE_BITSTREAM",
        _ => "unknown",
    }
}

// Keep the linker happy: rustc complains if `c_uint` and `c_void` are
// imported but unused after refactors. Bind them to a dummy `_` so the
// imports stay live in case future symbols need them.
const _: c_uint = 0;
