// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! EGL probe for render-target-capable DRM format modifiers.
//!
//! Queries `eglQueryDmaBufModifiersEXT` on a host EGL display and filters out
//! `external_only=TRUE` modifiers. Returned modifiers are safe to pass to a
//! consumer-side EGL `EGL_DMA_BUF_PLANE0_MODIFIER_LO/HI_EXT` import for use as
//! a `GL_TEXTURE_2D` color attachment.
//!
//! See `docs/learnings/nvidia-egl-dmabuf-render-target.md` — linear DMA-BUFs
//! are sampler-only on NVIDIA Linux; tiled modifiers picked from this probe
//! are render-target-capable.
//!
//! `libEGL.so.1` is loaded dynamically. When EGL is unavailable (headless CI,
//! systems without `libEGL`, or display servers that decline to initialize),
//! the probe returns an empty table and the caller is responsible for picking
//! a fallback path (typically: refuse to allocate a render-target image and
//! surface a `GpuError`).

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::Arc;

use libloading::Library;
use thiserror::Error;

/// DRM FOURCC codes for the surface formats the runtime cares about.
///
/// `fourcc::DRM_FORMAT_*` values from `<drm/drm_fourcc.h>` — packed little-
/// endian ASCII so e.g. `'A'|'R'<<8|'2'<<16|'4'<<24` = `AR24` = `ARGB8888`.
/// We list the constants here rather than depending on a `drm-fourcc` crate
/// because the set is small and stable.
pub mod fourcc {
    /// `'A'|'R'<<8|'2'<<16|'4'<<24` — `XRGB8888` packed.
    pub const DRM_FORMAT_ARGB8888: u32 = 0x3432_5241;
    /// `'A'|'B'<<8|'2'<<16|'4'<<24` — `ABGR8888` packed.
    pub const DRM_FORMAT_ABGR8888: u32 = 0x3432_4241;
    /// `'N'|'V'<<8|'1'<<16|'2'<<24` — NV12 (Y plane + interleaved UV).
    pub const DRM_FORMAT_NV12: u32 = 0x3231_564E;
}

/// `DRM_FORMAT_MOD_LINEAR` — sampler-only on NVIDIA Linux; included as the
/// universally-supported but non-render-target fallback.
pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;

/// Reasons the EGL probe couldn't enumerate render-target modifiers.
///
/// All variants are fall-back-to-linear conditions, not hard failures —
/// the runtime keeps booting even when EGL is missing.
#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("libEGL.so.1 not loadable: {0}")]
    LibraryNotFound(String),
    #[error("required EGL symbol '{0}' missing")]
    SymbolMissing(&'static str),
    #[error("eglGetDisplay returned EGL_NO_DISPLAY")]
    NoDisplay,
    #[error("eglInitialize failed (EGL error 0x{0:04x})")]
    InitFailed(u32),
    #[error("EGL_EXT_image_dma_buf_import_modifiers extension not advertised")]
    ExtensionMissing,
    #[error("eglQueryDmaBufModifiersEXT failed for fourcc 0x{0:08x} (EGL error 0x{1:04x})")]
    QueryFailed(u32, u32),
}

/// Render-target-capable DRM modifiers for each probed format.
///
/// Empty for formats EGL didn't return any `external_only=FALSE` modifier
/// for. Callers asking for a format that isn't in the table get an empty
/// slice — the convention is: empty list ⇒ no render-target path is
/// available for this format on this driver, fall back to linear with a
/// `tracing::warn!`.
#[derive(Debug, Clone, Default)]
pub struct DrmModifierTable {
    rt_modifiers: HashMap<u32, Vec<u64>>,
}

impl DrmModifierTable {
    /// Empty table — used when EGL probing failed and the caller falls back
    /// to linear-only allocation.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Render-target-capable modifiers for a DRM FOURCC, in the order EGL
    /// returned them. Vulkan's `VkImageDrmFormatModifierListCreateInfoEXT`
    /// will pick from this list at image-create time.
    pub fn rt_modifiers(&self, fourcc: u32) -> &[u64] {
        self.rt_modifiers
            .get(&fourcc)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Whether the probe found at least one RT-capable modifier for `fourcc`.
    pub fn has_rt_modifier(&self, fourcc: u32) -> bool {
        !self.rt_modifiers(fourcc).is_empty()
    }

    /// Number of probed formats with at least one RT-capable modifier.
    pub fn formats_with_rt_modifier(&self) -> usize {
        self.rt_modifiers
            .values()
            .filter(|v| !v.is_empty())
            .count()
    }
}

/// EGL constants and types the probe needs.
///
/// We dlopen libEGL and call extension functions via `eglGetProcAddress`, so
/// no `khronos-egl` dep is required and a missing libEGL becomes a graceful
/// `ProbeError::LibraryNotFound` rather than a link-time failure.
#[allow(non_camel_case_types, dead_code)]
mod egl {
    use std::ffi::c_void;

    pub type EGLDisplay = *mut c_void;
    pub type EGLBoolean = u32;
    pub type EGLint = i32;
    pub type EGLuint64KHR = u64;
    pub type EGLNativeDisplayType = *mut c_void;

    pub const EGL_NO_DISPLAY: EGLDisplay = std::ptr::null_mut();
    pub const EGL_DEFAULT_DISPLAY: EGLNativeDisplayType = std::ptr::null_mut();
    pub const EGL_TRUE: EGLBoolean = 1;
    pub const EGL_FALSE: EGLBoolean = 0;
    pub const EGL_EXTENSIONS: EGLint = 0x3055;
}

/// Probed EGL function pointers.
///
/// Held inside `Probe` for the duration of the probe; dropped before the
/// table is returned so libEGL can be unloaded without leaving dangling
/// symbol pointers.
struct EglFns {
    _lib: Arc<Library>,
    egl_get_display: unsafe extern "C" fn(egl::EGLNativeDisplayType) -> egl::EGLDisplay,
    egl_initialize: unsafe extern "C" fn(egl::EGLDisplay, *mut egl::EGLint, *mut egl::EGLint) -> egl::EGLBoolean,
    egl_terminate: unsafe extern "C" fn(egl::EGLDisplay) -> egl::EGLBoolean,
    egl_query_string: unsafe extern "C" fn(egl::EGLDisplay, egl::EGLint) -> *const c_char,
    egl_get_proc_address: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    egl_get_error: unsafe extern "C" fn() -> egl::EGLint,
    /// Loaded via `eglGetProcAddress`. `None` when the
    /// `EGL_EXT_image_dma_buf_import_modifiers` extension is not advertised.
    egl_query_dma_buf_modifiers: Option<
        unsafe extern "C" fn(
            egl::EGLDisplay,
            egl::EGLint,                  // format (DRM FOURCC)
            egl::EGLint,                  // max_modifiers (0 to query count)
            *mut egl::EGLuint64KHR,       // modifiers out
            *mut egl::EGLBoolean,         // external_only out (per-modifier)
            *mut egl::EGLint,             // num_modifiers out
        ) -> egl::EGLBoolean,
    >,
}

impl EglFns {
    fn load() -> Result<Self, ProbeError> {
        let lib = unsafe { Library::new("libEGL.so.1") }
            .or_else(|_| unsafe { Library::new("libEGL.so") })
            .map_err(|e| ProbeError::LibraryNotFound(e.to_string()))?;
        let lib = Arc::new(lib);

        unsafe fn sym<T: Copy>(lib: &Library, name: &'static [u8]) -> Result<T, ProbeError> {
            let symbol: libloading::Symbol<T> = unsafe { lib.get(name) }
                .map_err(|_| ProbeError::SymbolMissing(
                    std::str::from_utf8(&name[..name.len().saturating_sub(1)]).unwrap_or("?"),
                ))?;
            Ok(*symbol)
        }

        let egl_get_display = unsafe { sym(&lib, b"eglGetDisplay\0")? };
        let egl_initialize = unsafe { sym(&lib, b"eglInitialize\0")? };
        let egl_terminate = unsafe { sym(&lib, b"eglTerminate\0")? };
        let egl_query_string = unsafe { sym(&lib, b"eglQueryString\0")? };
        let egl_get_proc_address = unsafe { sym(&lib, b"eglGetProcAddress\0")? };
        let egl_get_error = unsafe { sym(&lib, b"eglGetError\0")? };

        Ok(Self {
            _lib: lib,
            egl_get_display,
            egl_initialize,
            egl_terminate,
            egl_query_string,
            egl_get_proc_address,
            egl_get_error,
            egl_query_dma_buf_modifiers: None,
        })
    }

    /// Resolve `eglQueryDmaBufModifiersEXT` after `eglInitialize`. The
    /// extension function is only valid once a display is initialized.
    fn resolve_modifier_query(&mut self, _display: egl::EGLDisplay) -> Result<(), ProbeError> {
        let name = CString::new("eglQueryDmaBufModifiersEXT").unwrap();
        let raw = unsafe { (self.egl_get_proc_address)(name.as_ptr()) };
        if raw.is_null() {
            return Err(ProbeError::ExtensionMissing);
        }
        // eglGetProcAddress returns a `void(*)()` cast — extension fn is
        // `EGLBoolean(EGLDisplay, EGLint, EGLint, EGLuint64KHR*, EGLBoolean*, EGLint*)`.
        let typed: unsafe extern "C" fn(
            egl::EGLDisplay,
            egl::EGLint,
            egl::EGLint,
            *mut egl::EGLuint64KHR,
            *mut egl::EGLBoolean,
            *mut egl::EGLint,
        ) -> egl::EGLBoolean = unsafe { std::mem::transmute(raw) };
        self.egl_query_dma_buf_modifiers = Some(typed);
        Ok(())
    }
}

/// FOURCC formats the probe interrogates by default. Mirrors the
/// `SurfaceFormat` set carried over the surface-adapter ABI (BGRA, RGBA,
/// NV12).
const DEFAULT_PROBE_FORMATS: &[u32] = &[
    fourcc::DRM_FORMAT_ABGR8888,
    fourcc::DRM_FORMAT_ARGB8888,
    fourcc::DRM_FORMAT_NV12,
];

/// Run the EGL probe on `EGL_DEFAULT_DISPLAY` and return a populated
/// [`DrmModifierTable`].
///
/// On any failure (missing libEGL, no display server, extension not
/// advertised), returns the error and the caller decides whether to
/// degrade to [`DrmModifierTable::empty`] or surface the failure.
#[tracing::instrument(level = "info", name = "drm_modifier_probe", skip_all)]
pub fn probe_default_display() -> Result<DrmModifierTable, ProbeError> {
    probe_with_formats(DEFAULT_PROBE_FORMATS)
}

/// Run the EGL probe with an explicit FOURCC list. Exposed for tests that
/// want to interrogate a single format.
pub fn probe_with_formats(formats: &[u32]) -> Result<DrmModifierTable, ProbeError> {
    let mut fns = EglFns::load()?;

    let display = unsafe { (fns.egl_get_display)(egl::EGL_DEFAULT_DISPLAY) };
    if display == egl::EGL_NO_DISPLAY {
        return Err(ProbeError::NoDisplay);
    }

    let mut major = 0;
    let mut minor = 0;
    let init_ok = unsafe { (fns.egl_initialize)(display, &mut major, &mut minor) };
    if init_ok != egl::EGL_TRUE {
        let err = unsafe { (fns.egl_get_error)() } as u32;
        return Err(ProbeError::InitFailed(err));
    }

    // Use a guard so eglTerminate runs even on early-return. The guard
    // holds a copied function pointer (fn pointers are Copy) plus the
    // display handle, so it doesn't borrow `fns` and the extension
    // resolve below can take `&mut fns` freely.
    struct DisplayGuard {
        terminate: unsafe extern "C" fn(egl::EGLDisplay) -> egl::EGLBoolean,
        display: egl::EGLDisplay,
    }
    impl Drop for DisplayGuard {
        fn drop(&mut self) {
            unsafe { (self.terminate)(self.display) };
        }
    }
    let _guard = DisplayGuard {
        terminate: fns.egl_terminate,
        display,
    };

    // Verify the extension is advertised on this display before chasing the
    // proc address.
    let exts_ptr = unsafe { (fns.egl_query_string)(display, egl::EGL_EXTENSIONS) };
    if exts_ptr.is_null() {
        return Err(ProbeError::ExtensionMissing);
    }
    let exts = unsafe { CStr::from_ptr(exts_ptr) }
        .to_str()
        .unwrap_or("");
    if !exts.split_whitespace().any(|tok| tok == "EGL_EXT_image_dma_buf_import_modifiers") {
        return Err(ProbeError::ExtensionMissing);
    }

    fns.resolve_modifier_query(display)?;

    let query = fns
        .egl_query_dma_buf_modifiers
        .expect("resolve_modifier_query set this");

    let mut table = DrmModifierTable::default();

    for &fourcc in formats {
        // First call with max=0, NULLs out, gets the count.
        let mut count: egl::EGLint = 0;
        let ok = unsafe {
            query(
                display,
                fourcc as egl::EGLint,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut count,
            )
        };
        if ok != egl::EGL_TRUE {
            let err = unsafe { (fns.egl_get_error)() } as u32;
            tracing::warn!(
                "drm_modifier_probe: eglQueryDmaBufModifiersEXT count for fourcc 0x{:08x} failed (EGL 0x{:04x})",
                fourcc,
                err
            );
            continue;
        }
        if count == 0 {
            tracing::debug!(
                "drm_modifier_probe: no modifiers advertised for fourcc 0x{:08x}",
                fourcc
            );
            continue;
        }

        let mut modifiers = vec![0u64; count as usize];
        let mut external_only = vec![egl::EGL_FALSE; count as usize];
        let mut returned: egl::EGLint = 0;
        let ok = unsafe {
            query(
                display,
                fourcc as egl::EGLint,
                count,
                modifiers.as_mut_ptr(),
                external_only.as_mut_ptr(),
                &mut returned,
            )
        };
        if ok != egl::EGL_TRUE {
            let err = unsafe { (fns.egl_get_error)() } as u32;
            return Err(ProbeError::QueryFailed(fourcc, err));
        }

        // Filter to render-target-capable: external_only must be EGL_FALSE.
        let rt: Vec<u64> = modifiers
            .iter()
            .zip(external_only.iter())
            .take(returned as usize)
            .filter_map(|(m, ext)| if *ext == egl::EGL_FALSE { Some(*m) } else { None })
            .collect();

        tracing::info!(
            "drm_modifier_probe: fourcc 0x{:08x} → {} modifier(s) total, {} render-target-capable",
            fourcc,
            returned,
            rt.len(),
        );

        if !rt.is_empty() {
            table.rt_modifiers.insert(fourcc, rt);
        }
    }

    tracing::info!(
        "drm_modifier_probe: EGL {}.{}, {} format(s) with render-target-capable modifiers",
        major,
        minor,
        table.formats_with_rt_modifier(),
    );

    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty table reports zero RT modifiers for any format and an empty
    /// slice on lookup — the no-EGL fallback shape.
    #[test]
    fn empty_table_reports_no_rt_modifiers() {
        let table = DrmModifierTable::empty();
        assert_eq!(table.formats_with_rt_modifier(), 0);
        assert!(!table.has_rt_modifier(fourcc::DRM_FORMAT_ABGR8888));
        assert!(table.rt_modifiers(fourcc::DRM_FORMAT_ABGR8888).is_empty());
    }

    /// Live EGL probe — best-effort, skips when no EGL is available.
    /// On NVIDIA Linux this should report ≥12 RT-capable modifiers for
    /// `DRM_FORMAT_ABGR8888` (the tiled NVIDIA modifiers documented in
    /// `docs/learnings/nvidia-egl-dmabuf-render-target.md`); on Mesa the
    /// count is driver-dependent. We assert only that the probe ran and
    /// either returned a known error or a sane table.
    #[test]
    fn probe_default_display_runs_or_skips_cleanly() {
        match probe_default_display() {
            Ok(table) => {
                let n = table.formats_with_rt_modifier();
                println!(
                    "EGL probe ok: {} format(s) with RT modifiers",
                    n
                );
                // Don't assert n>0 — vivid-only / headless CI may legitimately
                // return zero formats. The probe ran without panic; that is
                // the assertion.
            }
            Err(e) => {
                println!("EGL probe skipped: {e}");
            }
        }
    }

    /// FOURCC packing is little-endian ASCII per `<drm/drm_fourcc.h>`.
    /// Locking the constants here prevents the silent drift that would
    /// otherwise corrupt every modifier query for the affected format.
    #[test]
    fn fourcc_constants_are_ascii_packed() {
        assert_eq!(
            &fourcc::DRM_FORMAT_ARGB8888.to_le_bytes(),
            b"AR24",
        );
        assert_eq!(
            &fourcc::DRM_FORMAT_ABGR8888.to_le_bytes(),
            b"AB24",
        );
        assert_eq!(&fourcc::DRM_FORMAT_NV12.to_le_bytes(), b"NV12");
    }
}
