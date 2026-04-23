// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-runtime surface table backing the runtime-internal surface-sharing
//! service. Stores DMA-BUF fds keyed by `surface_id` so polyglot
//! subprocesses can `check_out` them via `SCM_RIGHTS`.

use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

#[derive(Clone, Debug)]
pub struct SurfaceMetadata {
    pub surface_id: String,
    pub runtime_id: String,
    pub dma_buf_fd: RawFd,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub resource_type: String,
    pub checkout_count: u64,
}

/// Thread-safe surface table for the runtime-internal broker.
#[derive(Clone, Default)]
pub struct SurfaceBrokerState {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    surfaces: RwLock<HashMap<String, SurfaceMetadata>>,
    surface_counter: AtomicU64,
}

impl SurfaceBrokerState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_surface(
        &self,
        surface_id: &str,
        runtime_id: &str,
        dma_buf_fd: RawFd,
        width: u32,
        height: u32,
        format: &str,
        resource_type: &str,
    ) -> bool {
        let mut surfaces = self.inner.surfaces.write();

        if surfaces.contains_key(surface_id) {
            return false;
        }

        self.inner.surface_counter.fetch_add(1, Ordering::Relaxed);

        surfaces.insert(
            surface_id.to_string(),
            SurfaceMetadata {
                surface_id: surface_id.to_string(),
                runtime_id: runtime_id.to_string(),
                dma_buf_fd,
                width,
                height,
                format: format.to_string(),
                resource_type: resource_type.to_string(),
                checkout_count: 0,
            },
        );
        true
    }

    pub fn get_surface_dma_buf_fd(&self, surface_id: &str) -> Option<RawFd> {
        let mut surfaces = self.inner.surfaces.write();
        surfaces.get_mut(surface_id).map(|metadata| {
            metadata.checkout_count += 1;
            metadata.dma_buf_fd
        })
    }

    pub fn release_surface(&self, surface_id: &str, runtime_id: &str) -> bool {
        let mut surfaces = self.inner.surfaces.write();
        if let Some(metadata) = surfaces.get(surface_id) {
            if metadata.runtime_id == runtime_id {
                unsafe { libc::close(metadata.dma_buf_fd) };
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

    #[test]
    fn register_surface_with_resource_type() {
        let state = SurfaceBrokerState::new();
        assert!(state.register_surface(
            "buf-001", "runtime-1", -1, 1920, 1080, "Rgba8Unorm", "pixel_buffer"
        ));
        assert!(state.register_surface(
            "tex-001", "runtime-1", -1, 1920, 1080, "Rgba8Unorm", "texture"
        ));

        let surfaces = state.get_surfaces();
        assert_eq!(surfaces.len(), 2);
        let buf = surfaces.iter().find(|s| s.surface_id == "buf-001").unwrap();
        assert_eq!(buf.resource_type, "pixel_buffer");
        let tex = surfaces.iter().find(|s| s.surface_id == "tex-001").unwrap();
        assert_eq!(tex.resource_type, "texture");
    }

    #[test]
    fn duplicate_surface_id_rejected() {
        let state = SurfaceBrokerState::new();
        assert!(state.register_surface("dup", "rt", -1, 640, 480, "Rgba8Unorm", "texture"));
        assert!(!state.register_surface("dup", "rt", -1, 640, 480, "Rgba8Unorm", "texture"));
    }
}
