// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-runtime IOSurface table behind the Mach surface-share service.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use parking_lot::RwLock;

use streamlib_surface_client::OwnedMachSendRight;

use super::CrossProcessTimelinePairsBySurface;
use crate::apple::iosurface::RetainedIOSurfaceSharedAcrossThreads;
use crate::core::context::surface_share_wire_verbs::VkImageCreateInfoFields;
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
    /// The surface's timeline pair as shared-event send rights, when the
    /// registrant sent them; every lookup hands out a fresh reference to each.
    pub timeline_send_rights: Option<Arc<SharedTimelineSendRights>>,
    /// The image a `texture` registration's surface backs; `None` for a
    /// pixel buffer.
    pub texture_image: Option<Arc<RegisteredTextureImage>>,
}

/// What a texture registration carries beyond its surface: the recipe a
/// helper rebuilds the image from, and the layout the image was last left in.
#[derive(Debug)]
pub struct RegisteredTextureImage {
    pub(crate) recipe: VkImageCreateInfoFields,
    current_image_layout: AtomicI32,
}

impl RegisteredTextureImage {
    /// A texture image built from `recipe`, in `current_image_layout`.
    pub(crate) fn new(recipe: VkImageCreateInfoFields, current_image_layout: i32) -> Self {
        Self {
            recipe,
            current_image_layout: AtomicI32::new(current_image_layout),
        }
    }

    /// The `VkImageLayout` the image was last published in.
    pub fn current_image_layout(&self) -> i32 {
        self.current_image_layout.load(Ordering::Acquire)
    }

    fn update_image_layout(&self, layout: i32) {
        self.current_image_layout.store(layout, Ordering::Release);
    }
}

/// Send rights to a surface's `produce_done` and `consume_done` Metal shared
/// events.
#[derive(Debug)]
pub struct SharedTimelineSendRights {
    /// The timeline the producer signals when a frame is written.
    pub produce_done: OwnedMachSendRight,
    /// The timeline a consumer signals when it has released a frame.
    pub consume_done: OwnedMachSendRight,
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
    cross_process_timeline_pairs: Arc<CrossProcessTimelinePairsBySurface>,
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

    /// Publish the layout `surface_id`'s texture image was left in; `false`
    /// when no texture is registered under it.
    pub fn update_image_layout(&self, surface_id: &str, layout: i32) -> bool {
        match self
            .inner
            .surfaces
            .read()
            .get(pool_slot_key_of_surface_id(surface_id))
            .and_then(|registration| registration.texture_image.as_ref())
        {
            Some(texture_image) => {
                texture_image.update_image_layout(layout);
                true
            }
            None => false,
        }
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

    /// The engine's timeline pairs by surface, shared with the surface store
    /// that registers them.
    pub fn cross_process_timeline_pairs(&self) -> &Arc<CrossProcessTimelinePairsBySurface> {
        &self.inner.cross_process_timeline_pairs
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
                self.inner.cross_process_timeline_pairs.remove(surface_id);
                true
            }
            _ => false,
        }
    }
}
