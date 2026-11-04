// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One import per pool slot. The first frame over a slot imports its memory;
//! every later frame over that slot reuses the import, and the surface-share
//! service sends it no fds. A cached import is never a claim: each frame
//! still takes and releases its own checkout lease, and the engine rehands a
//! slot by its leases alone.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use streamlib_surface_client::{
    SURFACE_SHARE_RETIRED_POOL_SLOTS_FIELD, split_pool_slot_and_frame_generation,
};

use super::HelperPixelBufferPoolSlotImport;
use super::storage_buffer::HelperStorageBufferPoolSlotImport;
use super::texture::HelperTexturePoolSlotImport;

/// One pool slot's memory as this helper imported it, shared by every frame
/// checked out over the slot.
#[derive(Clone)]
pub(crate) enum HelperPoolSlotImport {
    PixelBuffer(Arc<HelperPixelBufferPoolSlotImport>),
    Texture(Arc<HelperTexturePoolSlotImport>),
    StorageBuffer(Arc<HelperStorageBufferPoolSlotImport>),
}

/// Where a fresh import of a pool slot lands once it is made. Reserved while
/// the check-out that delivered the slot's fds holds the connection, so a
/// retirement notice on any later reply evicts it even before it is filled.
pub(crate) type HelperPoolSlotImportReservation = Arc<OnceLock<HelperPoolSlotImport>>;

/// This helper's imports, keyed by pool slot.
#[derive(Default)]
pub(crate) struct HelperPoolSlotImportsByPoolSlot {
    reservations_by_pool_slot_key: Mutex<HashMap<String, HelperPoolSlotImportReservation>>,
}

impl HelperPoolSlotImportsByPoolSlot {
    /// The import this helper holds for the slot of `published_pool_slot_key`,
    /// pinned for the caller by its clone.
    pub(crate) fn import_held_for(
        &self,
        published_pool_slot_key: &str,
    ) -> Option<HelperPoolSlotImport> {
        self.reservations_by_pool_slot_key
            .lock()
            .get(published_pool_slot_key)
            .and_then(|reservation| reservation.get().cloned())
    }

    /// Reserve the place a fresh import of `published_pool_slot_key` lands,
    /// replacing any earlier reservation of it.
    pub(crate) fn reserve_for_a_fresh_import(
        &self,
        published_pool_slot_key: &str,
    ) -> HelperPoolSlotImportReservation {
        let reservation = HelperPoolSlotImportReservation::default();
        self.reservations_by_pool_slot_key.lock().insert(
            published_pool_slot_key.to_string(),
            Arc::clone(&reservation),
        );
        reservation
    }

    /// Drop every import a surface-share reply lists as retired. A slot this
    /// helper no longer holds is ignored; a frame still reading a dropped
    /// import keeps it alive until that frame drops.
    pub(crate) fn drop_the_pool_slots_a_reply_lists_as_retired(&self, reply: &serde_json::Value) {
        let Some(retired_pool_slots) = reply
            .get(SURFACE_SHARE_RETIRED_POOL_SLOTS_FIELD)
            .and_then(|value| value.as_array())
        else {
            return;
        };
        let mut reservations_by_pool_slot_key = self.reservations_by_pool_slot_key.lock();
        for retired_pool_slot_key in retired_pool_slots.iter().filter_map(|key| key.as_str()) {
            reservations_by_pool_slot_key.remove(retired_pool_slot_key);
        }
    }

    /// Drop every import: the connection the service would have sent their
    /// retirement notices on is gone.
    pub(crate) fn drop_every_import(&self) {
        self.reservations_by_pool_slot_key.lock().clear();
    }

    /// How many pool slots this helper holds an import of.
    #[cfg(test)]
    pub(crate) fn held_pool_slot_count(&self) -> usize {
        self.reservations_by_pool_slot_key
            .lock()
            .values()
            .filter(|reservation| reservation.get().is_some())
            .count()
    }
}

/// The pool slot a published frame id (`<slot>#<generation>`) names; `None`
/// for every other surface id, which is imported per check-out and never
/// cached.
pub(crate) fn pool_slot_of_a_published_frame_id(surface_id: &str) -> Option<&str> {
    split_pool_slot_and_frame_generation(surface_id).map(|(pool_slot, _)| pool_slot)
}
