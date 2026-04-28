// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-surface adapter state: the cached `EGLImage` + `GL_TEXTURE_2D`
//! id and the read/write contention counters the trait's typestate
//! enforces at the API level.

use khronos_egl as egl;
use streamlib_adapter_abi::{SurfaceId, SurfaceRegistration};

#[allow(dead_code)] // referenced via SurfaceState; kept private to the adapter
type EglImage = egl::Image;

/// Inputs the host hands to
/// [`crate::OpenGlSurfaceAdapter::register_host_surface`].
///
/// `dma_buf_fd` is the FD exported from a host-allocated `VkImage`
/// (via `streamlib`'s `HostVulkanTexture::export_dma_buf_fd`). The adapter
/// dups it during EGL import; the caller may close their copy after
/// `register_host_surface` returns.
///
/// `drm_format_modifier` is the modifier the host's allocator chose
/// for the `VkImage` — per the NVIDIA EGL DMA-BUF render-target
/// learning, the host MUST pick a tiled, render-target-capable
/// modifier or the resulting GL texture is sampler-only.
pub struct HostSurfaceRegistration {
    pub dma_buf_fd: i32,
    pub width: u32,
    pub height: u32,
    /// `DRM_FORMAT_*` four-character code for the surface's pixel
    /// layout (e.g. `DRM_FORMAT_ABGR8888` for `Bgra8`/`Rgba8`).
    pub drm_fourcc: u32,
    pub drm_format_modifier: u64,
    pub plane_offset: u64,
    pub plane_stride: u64,
}

/// Per-surface state held inside the adapter's
/// `Mutex<HashMap<SurfaceId, _>>`. The registry mutex owns
/// the EGLImage / GL texture lifetime — neither can outlive the
/// registry entry.
pub(crate) struct SurfaceState {
    #[allow(dead_code)] // tracing / debug only
    pub(crate) surface_id: SurfaceId,
    pub(crate) image: egl::Image,
    pub(crate) texture: u32,
    pub(crate) read_holders: u64,
    pub(crate) write_held: bool,
}

// SAFETY: `egl::Image` wraps a raw `EGLImage` pointer and is `!Send`
// / `!Sync` by default. The state is only ever accessed while
// holding the adapter's `Mutex` *and* the EGL runtime's make-current
// lock, so concurrent dereference cannot occur. The wrapped pointer
// is opaque to other threads — they can only read/write it under
// both locks.
unsafe impl Send for SurfaceState {}
unsafe impl Sync for SurfaceState {}

impl SurfaceRegistration for SurfaceState {
    fn write_held(&self) -> bool {
        self.write_held
    }
    fn read_holders(&self) -> u64 {
        self.read_holders
    }
    fn set_write_held(&mut self, held: bool) {
        self.write_held = held;
    }
    fn inc_read_holders(&mut self) {
        self.read_holders += 1;
    }
    fn dec_read_holders(&mut self) {
        self.read_holders = self.read_holders.saturating_sub(1);
    }
}
