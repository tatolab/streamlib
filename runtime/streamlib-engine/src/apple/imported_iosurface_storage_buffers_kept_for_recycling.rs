// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A stream's IOSurfaces imported as storage buffers, zero-copy, and kept for
//! the next time their producer — a camera, a codec session's pool — recycles
//! them.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use objc2_io_surface::{IOSurfaceID, IOSurfaceRef};

use crate::core::Result;
use crate::core::context::GpuContextLimitedAccess;
use crate::vulkan::rhi::ImportedIOSurfaceStorageBuffer;

/// How many distinct IOSurfaces one stream keeps imported. A producer recycles
/// a handful from its own pool, so a stream past this is churning surfaces and
/// starts over rather than growing.
const MOST_IMPORTED_IOSURFACES_KEPT: usize = 16;

/// One stream's imported IOSurfaces, keyed by the surface's id.
#[derive(Default)]
pub(crate) struct ImportedIOSurfaceStorageBuffersKeptForRecycling {
    imported_by_iosurface_id: HashMap<IOSurfaceID, ImportedIOSurfaceStorageBuffer>,
}

impl ImportedIOSurfaceStorageBuffersKeptForRecycling {
    /// Whether `iosurface` is imported already.
    pub(crate) fn holds(&self, iosurface: &IOSurfaceRef) -> bool {
        self.imported_by_iosurface_id.contains_key(&iosurface.id())
    }

    /// How many surfaces are imported.
    pub(crate) fn count(&self) -> usize {
        self.imported_by_iosurface_id.len()
    }

    /// The storage buffer over `iosurface`, imported on the first frame it
    /// carries. A refused import is the error, and leaves the set as it was.
    ///
    /// A new surface past the bound starts the set over, releasing every other
    /// import — so the caller must have retired any GPU work still reading one
    /// before asking for a surface it does not [`Self::holds`].
    pub(crate) fn imported_for(
        &mut self,
        gpu_context: &GpuContextLimitedAccess,
        iosurface: &IOSurfaceRef,
    ) -> Result<&ImportedIOSurfaceStorageBuffer> {
        if !self.holds(iosurface) && self.count() >= MOST_IMPORTED_IOSURFACES_KEPT {
            self.imported_by_iosurface_id.clear();
        }
        match self.imported_by_iosurface_id.entry(iosurface.id()) {
            Entry::Occupied(imported) => Ok(imported.into_mut()),
            Entry::Vacant(slot) => Ok(slot.insert(
                gpu_context.escalate(|full| full.import_iosurface_as_storage_buffer(iosurface))?,
            )),
        }
    }
}
