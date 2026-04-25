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
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub resource_type: String,
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

/// Arguments to [`SurfaceShareState::register_surface`]. Grouped so the
/// signature stays legible as the per-plane fields grow.
pub struct SurfaceRegistration<'a> {
    pub surface_id: &'a str,
    pub runtime_id: &'a str,
    pub dma_buf_fds: Vec<RawFd>,
    pub plane_sizes: Vec<u64>,
    pub plane_offsets: Vec<u64>,
    pub width: u32,
    pub height: u32,
    pub format: &'a str,
    pub resource_type: &'a str,
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
                width: reg.width,
                height: reg.height,
                format: reg.format.to_string(),
                resource_type: reg.resource_type.to_string(),
                checkout_count: 0,
            },
        );
        Ok(())
    }

    /// Return a clone of the surface's plane fd vec plus its plane-layout
    /// arrays. The returned fds are the table's own — callers that hand them
    /// out via SCM_RIGHTS must `dup` each fd first.
    pub fn get_surface_planes(
        &self,
        surface_id: &str,
    ) -> Option<(Vec<RawFd>, Vec<u64>, Vec<u64>)> {
        let mut surfaces = self.inner.surfaces.write();
        surfaces.get_mut(surface_id).map(|metadata| {
            metadata.checkout_count += 1;
            (
                metadata.dma_buf_fds.clone(),
                metadata.plane_sizes.clone(),
                metadata.plane_offsets.clone(),
            )
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
            width: 1920,
            height: 1080,
            format: "Rgba8Unorm",
            resource_type,
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

    /// Releasing a surface registered with multiple plane fds must close
    /// every fd — the state is the last owner of the table's fd dups and
    /// leaking any plane would leak the whole DMA-BUF.
    #[test]
    fn release_surface_closes_every_plane_fd() {
        let state = SurfaceShareState::new();
        // Pair of real, independently-owned memfds so libc::close actually
        // observable succeeds / fails per fd.
        let plane_fds: Vec<RawFd> = (0..3)
            .map(|i| {
                let name = std::ffi::CString::new(format!("release-plane-{}", i)).unwrap();
                let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
                assert!(fd >= 0, "memfd {}: {}", i, std::io::Error::last_os_error());
                fd
            })
            .collect();

        state
            .register_surface(SurfaceRegistration {
                surface_id: "multi",
                runtime_id: "rt",
                dma_buf_fds: plane_fds.clone(),
                plane_sizes: vec![8192, 2048, 2048],
                plane_offsets: vec![0, 0, 0],
                width: 640,
                height: 480,
                format: "Nv12VideoRange",
                resource_type: "pixel_buffer",
            })
            .expect("register multi-plane");

        assert!(state.release_surface("multi", "rt"));

        // Each fd should be closed now. fcntl(F_GETFD) on a closed fd
        // returns -1 with EBADF.
        for fd in &plane_fds {
            let ret = unsafe { libc::fcntl(*fd, libc::F_GETFD) };
            assert_eq!(
                ret, -1,
                "plane fd {} should be closed after release_surface",
                fd
            );
            let errno = std::io::Error::last_os_error().raw_os_error();
            assert_eq!(errno, Some(libc::EBADF));
        }
    }
}
