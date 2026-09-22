// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-runtime IOSurface table behind the Mach surface-share service.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::apple::iosurface::RetainedIOSurfaceSharedAcrossThreads;
use crate::core::context::{SurfaceCheckOutLeaseRegistry, SurfaceShareRegistrationsByRuntime};
use crate::core::rhi::pool_slot_key_of_surface_id;

/// One registered surface.
#[derive(Clone)]
pub struct IOSurfaceShareRegistration {
    /// The pool slot key (or checked-in id) the surface is registered under.
    pub surface_id: String,
    /// The runtime that registered it, and the only one that may release it.
    pub runtime_id: String,
    /// The surface itself; every lookup mints a fresh port to it.
    pub iosurface: RetainedIOSurfaceSharedAcrossThreads,
    /// Width in pixels, as the registration stated it.
    pub width: u32,
    /// Height in pixels, as the registration stated it.
    pub height: u32,
    /// The pixel format's wire name, as the registration stated it.
    pub format: String,
    /// `pixel_buffer` or `texture`, as the registration stated it.
    pub resource_type: String,
}

/// Thread-safe IOSurface table for the runtime-internal Mach surface-share
/// service.
#[derive(Clone, Default)]
pub struct IOSurfaceShareState {
    inner: Arc<IOSurfaceShareStateInner>,
}

#[derive(Default)]
struct IOSurfaceShareStateInner {
    surfaces: RwLock<HashMap<String, IOSurfaceShareRegistration>>,
    check_out_leases: Arc<SurfaceCheckOutLeaseRegistry>,
}

impl IOSurfaceShareState {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `registration`, refusing — and handing back — one whose id is
    /// already registered.
    pub fn register_surface(
        &self,
        registration: IOSurfaceShareRegistration,
    ) -> Result<(), IOSurfaceShareRegistration> {
        let mut surfaces = self.inner.surfaces.write();
        if surfaces.contains_key(&registration.surface_id) {
            return Err(registration);
        }
        surfaces.insert(registration.surface_id.clone(), registration);
        Ok(())
    }

    /// The registration behind `surface_id`; a published frame id resolves
    /// through its pool slot.
    pub fn registration_of(&self, surface_id: &str) -> Option<IOSurfaceShareRegistration> {
        self.inner
            .surfaces
            .read()
            .get(pool_slot_key_of_surface_id(surface_id))
            .cloned()
    }

    /// Every registered surface id.
    pub fn surface_ids(&self) -> Vec<String> {
        self.inner.surfaces.read().keys().cloned().collect()
    }

    /// The checkout leases cross-process consumers hold against this table,
    /// shared with the pixel-buffer pool.
    pub fn check_out_leases(&self) -> &Arc<SurfaceCheckOutLeaseRegistry> {
        &self.inner.check_out_leases
    }
}

impl SurfaceShareRegistrationsByRuntime for IOSurfaceShareState {
    fn surface_ids_by_runtime(&self, runtime_id: &str) -> Vec<String> {
        self.inner
            .surfaces
            .read()
            .values()
            .filter(|registration| registration.runtime_id == runtime_id)
            .map(|registration| registration.surface_id.clone())
            .collect()
    }

    fn release_surface(&self, surface_id: &str, runtime_id: &str) -> bool {
        let surface_id = pool_slot_key_of_surface_id(surface_id);
        let mut surfaces = self.inner.surfaces.write();
        match surfaces.get(surface_id) {
            Some(registration) if registration.runtime_id == runtime_id => {
                surfaces.remove(surface_id);
                true
            }
            _ => false,
        }
    }
}
