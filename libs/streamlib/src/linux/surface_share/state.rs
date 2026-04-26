// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-runtime surface table backing the runtime-internal surface-sharing
//! service. Stores DMA-BUF fds keyed by `surface_id` so polyglot
//! subprocesses can `check_out` them via `SCM_RIGHTS`.
//!
//! Each surface may hold up to [`streamlib_surface_client::MAX_DMA_BUF_PLANES`]
//! fds — one per plane for multi-plane DMA-BUFs under DRM format modifiers
//! (e.g. NV12 with separate Y and UV allocations). Single-plane surfaces
//! register a one-element vec; the multi-plane path is strictly additive.

use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

#[derive(Clone, Debug)]
pub struct SurfaceMetadata {
    pub surface_id: String,
    pub runtime_id: String,
    pub dma_buf_fds: Vec<RawFd>,
    pub plane_sizes: Vec<u64>,
    pub plane_offsets: Vec<u64>,
    /// Per-plane row pitch in bytes — what the consumer-side EGL or
    /// Vulkan import passes via `EGL_DMA_BUF_PLANE{N}_PITCH_EXT` /
    /// `VkSubresourceLayout::rowPitch`. One entry per plane fd; defaults
    /// to a vec of zeros for legacy registrations that didn't supply it.
    pub plane_strides: Vec<u64>,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub resource_type: String,
    /// DRM format modifier of the underlying VkImage. Zero means
    /// `DRM_FORMAT_MOD_LINEAR` (sampler-only on NVIDIA — see
    /// `docs/learnings/nvidia-egl-dmabuf-render-target.md`) or "not set"
    /// for legacy `VkBuffer`-backed surfaces (CPU-readable pixel buffers).
    /// Render-target adapters MUST receive a non-zero modifier picked
    /// from the EGL `external_only=FALSE` set; otherwise consumer-side
    /// FBO completeness will fail on NVIDIA.
    pub drm_format_modifier: u64,
    pub checkout_count: u64,
}

/// Thread-safe surface table for the runtime-internal surface-share service.
#[derive(Clone, Default)]
pub struct SurfaceShareState {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    surfaces: RwLock<HashMap<String, SurfaceMetadata>>,
    surface_counter: AtomicU64,
}

/// Result of [`SurfaceShareState::get_surface_planes`] — everything a
/// consumer needs to import the DMA-BUF as a Vulkan or EGL image.
#[derive(Clone, Debug)]
pub struct SurfacePlaneCheckout {
    pub dma_buf_fds: Vec<RawFd>,
    pub plane_sizes: Vec<u64>,
    pub plane_offsets: Vec<u64>,
    pub plane_strides: Vec<u64>,
    pub drm_format_modifier: u64,
}

/// Arguments to [`SurfaceShareState::register_surface`]. Grouped so the
/// signature stays legible as the per-plane fields grow.
pub struct SurfaceRegistration<'a> {
    pub surface_id: &'a str,
    pub runtime_id: &'a str,
    pub dma_buf_fds: Vec<RawFd>,
    pub plane_sizes: Vec<u64>,
    pub plane_offsets: Vec<u64>,
    /// Per-plane row pitch in bytes. Length must match `dma_buf_fds`.
    pub plane_strides: Vec<u64>,
    pub width: u32,
    pub height: u32,
    pub format: &'a str,
    pub resource_type: &'a str,
    /// DRM format modifier of the underlying VkImage. See
    /// [`SurfaceMetadata::drm_format_modifier`].
    pub drm_format_modifier: u64,
}

impl SurfaceShareState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a surface into the table.
    ///
    /// On rejection (duplicate surface_id), ownership of `dma_buf_fds` is
    /// returned to the caller so it can decide whether to close them or hand
    /// them to the next attempt. On success, the table owns the fds and
    /// closes each on [`Self::release_surface`].
    pub fn register_surface(
        &self,
        reg: SurfaceRegistration<'_>,
    ) -> Result<(), Vec<RawFd>> {
        let mut surfaces = self.inner.surfaces.write();

        if surfaces.contains_key(reg.surface_id) {
            return Err(reg.dma_buf_fds);
        }

        self.inner.surface_counter.fetch_add(1, Ordering::Relaxed);

        surfaces.insert(
            reg.surface_id.to_string(),
            SurfaceMetadata {
                surface_id: reg.surface_id.to_string(),
                runtime_id: reg.runtime_id.to_string(),
                dma_buf_fds: reg.dma_buf_fds,
                plane_sizes: reg.plane_sizes,
                plane_offsets: reg.plane_offsets,
                plane_strides: reg.plane_strides,
                width: reg.width,
                height: reg.height,
                format: reg.format.to_string(),
                resource_type: reg.resource_type.to_string(),
                drm_format_modifier: reg.drm_format_modifier,
                checkout_count: 0,
            },
        );
        Ok(())
    }

    /// Return a clone of the surface's plane fd vec plus its plane-layout
    /// arrays and the underlying VkImage's DRM format modifier. The
    /// returned fds are the table's own — callers that hand them out via
    /// SCM_RIGHTS must `dup` each fd first.
    pub fn get_surface_planes(
        &self,
        surface_id: &str,
    ) -> Option<SurfacePlaneCheckout> {
        let mut surfaces = self.inner.surfaces.write();
        surfaces.get_mut(surface_id).map(|metadata| {
            metadata.checkout_count += 1;
            SurfacePlaneCheckout {
                dma_buf_fds: metadata.dma_buf_fds.clone(),
                plane_sizes: metadata.plane_sizes.clone(),
                plane_offsets: metadata.plane_offsets.clone(),
                plane_strides: metadata.plane_strides.clone(),
                drm_format_modifier: metadata.drm_format_modifier,
            }
        })
    }

    pub fn release_surface(&self, surface_id: &str, runtime_id: &str) -> bool {
        let mut surfaces = self.inner.surfaces.write();
        if let Some(metadata) = surfaces.get(surface_id) {
            if metadata.runtime_id == runtime_id {
                for fd in &metadata.dma_buf_fds {
                    unsafe { libc::close(*fd) };
                }
                surfaces.remove(surface_id);
                return true;
            }
        }
        false
    }

    pub fn get_surfaces(&self) -> Vec<SurfaceMetadata> {
        self.inner.surfaces.read().values().cloned().collect()
    }

    /// Surface ids registered by `runtime_id`. Used by the EPOLLHUP watchdog
    /// to find what to release when a subprocess connection drops.
    pub fn surface_ids_by_runtime(&self, runtime_id: &str) -> Vec<String> {
        self.inner
            .surfaces
            .read()
            .values()
            .filter(|m| m.runtime_id == runtime_id)
            .map(|m| m.surface_id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg<'a>(surface_id: &'a str, runtime_id: &'a str, resource_type: &'a str) -> SurfaceRegistration<'a> {
        SurfaceRegistration {
            surface_id,
            runtime_id,
            dma_buf_fds: vec![-1],
            plane_sizes: vec![0],
            plane_offsets: vec![0],
            plane_strides: vec![0],
            width: 1920,
            height: 1080,
            format: "Rgba8Unorm",
            resource_type,
            drm_format_modifier: 0,
        }
    }

    #[test]
    fn register_surface_with_resource_type() {
        let state = SurfaceShareState::new();
        assert!(state
            .register_surface(reg("buf-001", "runtime-1", "pixel_buffer"))
            .is_ok());
        assert!(state
            .register_surface(reg("tex-001", "runtime-1", "texture"))
            .is_ok());

        let surfaces = state.get_surfaces();
        assert_eq!(surfaces.len(), 2);
        let buf = surfaces.iter().find(|s| s.surface_id == "buf-001").unwrap();
        assert_eq!(buf.resource_type, "pixel_buffer");
        let tex = surfaces.iter().find(|s| s.surface_id == "tex-001").unwrap();
        assert_eq!(tex.resource_type, "texture");
    }

    #[test]
    fn duplicate_surface_id_rejected() {
        let state = SurfaceShareState::new();
        assert!(state.register_surface(reg("dup", "rt", "texture")).is_ok());
        let rejected = state
            .register_surface(reg("dup", "rt", "texture"))
            .expect_err("duplicate must be rejected");
        assert_eq!(rejected, vec![-1], "rejected fds returned to caller");
    }

    /// The watchdog uses `surface_ids_by_runtime` to discover what to
    /// release when a subprocess connection drops. The query must group by
    /// `runtime_id` precisely — surfaces from sibling runtimes must not
    /// appear in the result, or one crash would clean up another runtime's
    /// state.
    #[test]
    fn surface_ids_by_runtime_groups_by_owner() {
        let state = SurfaceShareState::new();
        state
            .register_surface(reg("a-1", "runtime-A", "pixel_buffer"))
            .expect("a-1");
        state
            .register_surface(reg("a-2", "runtime-A", "pixel_buffer"))
            .expect("a-2");
        state
            .register_surface(reg("b-1", "runtime-B", "pixel_buffer"))
            .expect("b-1");

        let mut for_a = state.surface_ids_by_runtime("runtime-A");
        for_a.sort();
        assert_eq!(for_a, vec!["a-1".to_string(), "a-2".to_string()]);

        let for_b = state.surface_ids_by_runtime("runtime-B");
        assert_eq!(for_b, vec!["b-1".to_string()]);

        assert!(state.surface_ids_by_runtime("runtime-C").is_empty());

        // After release, the owner's set shrinks and others are unaffected.
        assert!(state.release_surface("a-1", "runtime-A"));
        let mut for_a_after = state.surface_ids_by_runtime("runtime-A");
        for_a_after.sort();
        assert_eq!(for_a_after, vec!["a-2".to_string()]);
        assert_eq!(
            state.surface_ids_by_runtime("runtime-B"),
            vec!["b-1".to_string()]
        );
    }

    /// Releasing a surface registered with multiple plane fds must close
    /// every fd — the state is the last owner of the table's fd dups and
    /// leaking any plane would leak the whole DMA-BUF. Verified via pipes:
    /// register hands the write end to the table, and after release the
    /// read end yields EOF on the next `read`. EOF is sticky and tied to
    /// the pipe's underlying kernel object, so unlike `fcntl(F_GETFD)` on
    /// a raw fd number, the assertion does not race against parallel
    /// threads recycling fd-table slots.
    #[test]
    fn release_surface_closes_every_plane_fd() {
        let state = SurfaceShareState::new();

        // Three pipes; we keep the read ends, hand the write ends to the
        // table.
        let mut read_fds: Vec<RawFd> = Vec::with_capacity(3);
        let mut write_fds: Vec<RawFd> = Vec::with_capacity(3);
        for _ in 0..3 {
            let mut fds = [0i32; 2];
            let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
            assert_eq!(rc, 0, "pipe: {}", std::io::Error::last_os_error());
            read_fds.push(fds[0]);
            write_fds.push(fds[1]);
        }

        state
            .register_surface(SurfaceRegistration {
                surface_id: "multi",
                runtime_id: "rt",
                dma_buf_fds: write_fds,
                plane_sizes: vec![8192, 2048, 2048],
                plane_offsets: vec![0, 0, 0],
                plane_strides: vec![64, 32, 32],
                width: 640,
                height: 480,
                format: "Nv12VideoRange",
                resource_type: "pixel_buffer",
                drm_format_modifier: 0,
            })
            .expect("register multi-plane");

        assert!(state.release_surface("multi", "rt"));

        // With the write ends closed, every read end now yields EOF (0
        // bytes) on the next read — the kernel signals that no more data
        // is coming and the pipe will never refill.
        for fd in &read_fds {
            let mut buf = [0u8; 1];
            let n = unsafe {
                libc::read(*fd, buf.as_mut_ptr() as *mut libc::c_void, 1)
            };
            assert_eq!(
                n, 0,
                "pipe read end {} should yield EOF after write end was closed by release_surface",
                fd
            );
            unsafe { libc::close(*fd) };
        }
    }
}
