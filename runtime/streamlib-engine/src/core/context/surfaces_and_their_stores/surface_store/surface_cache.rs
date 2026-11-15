// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;

use crate::core::rhi::PixelBuffer;

/// Maximum number of entries in the SurfaceCache before eviction.
const MAX_SURFACE_CACHE_SIZE: usize = 512;

/// Surface metadata stored alongside the cached pixel buffer.
#[derive(Debug, Clone)]
pub struct CachedSurface {
    /// The resolved pixel buffer.
    pub pixel_buffer: PixelBuffer,
    /// Number of times this surface has been checked out.
    pub checkout_count: u64,
}

/// Local cache for resolved surfaces.
pub(super) struct SurfaceCache {
    /// Map from surface ID to cached pixel buffer.
    surfaces: HashMap<String, CachedSurface>,
}

impl SurfaceCache {
    pub(super) fn new() -> Self {
        Self {
            surfaces: HashMap::new(),
        }
    }

    pub(super) fn insert(&mut self, surface_id: String, pixel_buffer: PixelBuffer) {
        self.surfaces.insert(
            surface_id,
            CachedSurface {
                pixel_buffer,
                checkout_count: 1,
            },
        );
        if self.surfaces.len() > MAX_SURFACE_CACHE_SIZE {
            tracing::warn!(
                "SurfaceCache: exceeded {} entries ({}), clearing",
                MAX_SURFACE_CACHE_SIZE,
                self.surfaces.len()
            );
            self.surfaces.clear();
        }
    }

    pub(super) fn remove(&mut self, surface_id: &str) {
        self.surfaces.remove(surface_id);
    }

    /// The cached buffer for `surface_id`, counted as one more checkout.
    pub(super) fn checked_out_clone(&mut self, surface_id: &str) -> Option<PixelBuffer> {
        let cached = self.surfaces.get_mut(surface_id)?;
        cached.checkout_count += 1;
        tracing::trace!(
            "SurfaceStore: Cache hit for '{}' (checkout #{})",
            surface_id,
            cached.checkout_count
        );
        Some(cached.pixel_buffer.clone())
    }

    /// The cached buffer for `surface_id`, uncounted.
    pub(super) fn cached_clone(&self, surface_id: &str) -> Option<PixelBuffer> {
        self.surfaces
            .get(surface_id)
            .map(|cached| cached.pixel_buffer.clone())
    }

    pub(super) fn clear(&mut self) {
        self.surfaces.clear();
    }
}
