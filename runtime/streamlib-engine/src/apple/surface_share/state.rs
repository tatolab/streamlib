// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-runtime IOSurface table behind the Mach surface-share service.

use std::collections::HashMap;
use std::sync::Arc;

use objc2_core_foundation::CFRetained;
use objc2_io_surface::IOSurfaceRef;
use parking_lot::RwLock;

use crate::core::context::SurfaceCheckOutLeaseRegistry;
use crate::core::rhi::pool_slot_key_of_surface_id;

/// An IOSurface the table holds a reference to.
///
/// The table only retains, releases and reads the surface's geometry and
/// in-use state, all of which IOSurface allows from any thread.
#[derive(Clone)]
pub struct IOSurfaceRetainedByTheShareTable(CFRetained<IOSurfaceRef>);

// SAFETY: see the type's doc — nothing the table does with the surface is
// thread-affine.
unsafe impl Send for IOSurfaceRetainedByTheShareTable {}
// SAFETY: as above.
unsafe impl Sync for IOSurfaceRetainedByTheShareTable {}

impl IOSurfaceRetainedByTheShareTable {
    /// Hold `iosurface` in the table.
    pub fn new(iosurface: CFRetained<IOSurfaceRef>) -> Self {
        Self(iosurface)
    }
}

impl std::ops::Deref for IOSurfaceRetainedByTheShareTable {
    type Target = IOSurfaceRef;
    fn deref(&self) -> &IOSurfaceRef {
        &self.0
    }
}

/// One registered surface.
#[derive(Clone)]
pub struct IOSurfaceShareRegistration {
    pub surface_id: String,
    pub runtime_id: String,
    pub iosurface: IOSurfaceRetainedByTheShareTable,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub resource_type: String,
    pub checkout_count: u64,
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

    /// The registration behind `surface_id` — a published frame id resolves
    /// through its pool slot — counted as one more checkout.
    pub fn get_surface_for_lookup(&self, surface_id: &str) -> Option<IOSurfaceShareRegistration> {
        let mut surfaces = self.inner.surfaces.write();
        surfaces
            .get_mut(pool_slot_key_of_surface_id(surface_id))
            .map(|registration| {
                registration.checkout_count += 1;
                registration.clone()
            })
    }

    /// Drop `surface_id`'s registration when `runtime_id` made it.
    pub fn release_surface(&self, surface_id: &str, runtime_id: &str) -> bool {
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

    /// Every registered surface id.
    pub fn surface_ids(&self) -> Vec<String> {
        self.inner.surfaces.read().keys().cloned().collect()
    }

    /// Surface ids `runtime_id` registered — what a dropped out-of-process
    /// connection's registrations are released by.
    pub fn surface_ids_by_runtime(&self, runtime_id: &str) -> Vec<String> {
        self.inner
            .surfaces
            .read()
            .values()
            .filter(|registration| registration.runtime_id == runtime_id)
            .map(|registration| registration.surface_id.clone())
            .collect()
    }

    /// The checkout leases cross-process consumers hold against this table,
    /// shared with the pixel-buffer pool.
    pub fn check_out_leases(&self) -> &Arc<SurfaceCheckOutLeaseRegistry> {
        &self.inner.check_out_leases
    }
}
