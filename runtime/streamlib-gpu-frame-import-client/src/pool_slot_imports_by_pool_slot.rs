// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One import per pool slot. The first frame over a slot imports its memory;
//! every later frame over that slot reuses the import, and the surface-share
//! service sends it no handles. A cached import is never a claim: each frame
//! still holds itself against producer reuse, by a checkout lease or a claim
//! in its pool's claim table.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use streamlib_surface_client::{
    SURFACE_SHARE_RETIRED_POOL_SLOTS_FIELD, split_pool_slot_and_frame_generation,
};

/// Where a fresh import of a pool slot lands once it is made. Reserved while
/// the reply that delivered the slot's handles holds the connection, so a
/// retirement notice on any later reply evicts it even before it is filled.
pub type PoolSlotImportReservation<PoolSlotImport> = Arc<OnceLock<PoolSlotImport>>;

/// This importer's imports, keyed by pool slot.
pub struct PoolSlotImportsByPoolSlot<PoolSlotImport> {
    reservations_by_pool_slot_key:
        Mutex<HashMap<String, PoolSlotImportReservation<PoolSlotImport>>>,
}

impl<PoolSlotImport> Default for PoolSlotImportsByPoolSlot<PoolSlotImport> {
    fn default() -> Self {
        Self {
            reservations_by_pool_slot_key: Mutex::new(HashMap::new()),
        }
    }
}

impl<PoolSlotImport: Clone> PoolSlotImportsByPoolSlot<PoolSlotImport> {
    /// The import this importer holds for the slot of `published_pool_slot_key`,
    /// pinned for the caller by its clone.
    pub fn import_held_for(&self, published_pool_slot_key: &str) -> Option<PoolSlotImport> {
        self.reservations_by_pool_slot_key
            .lock()
            .get(published_pool_slot_key)
            .and_then(|reservation| reservation.get().cloned())
    }

    /// Reserve the place a fresh import of `published_pool_slot_key` lands,
    /// replacing any earlier reservation of it.
    pub fn reserve_for_a_fresh_import(
        &self,
        published_pool_slot_key: &str,
    ) -> PoolSlotImportReservation<PoolSlotImport> {
        let reservation = PoolSlotImportReservation::default();
        self.reservations_by_pool_slot_key.lock().insert(
            published_pool_slot_key.to_string(),
            Arc::clone(&reservation),
        );
        reservation
    }

    /// Drop every import a surface-share reply lists as retired. A slot this
    /// importer no longer holds is ignored; a frame still reading a dropped
    /// import keeps it alive until that frame drops.
    pub fn drop_the_pool_slots_a_reply_lists_as_retired(&self, reply: &serde_json::Value) {
        let mut reservations_by_pool_slot_key = self.reservations_by_pool_slot_key.lock();
        for retired_pool_slot_key in pool_slots_a_reply_lists_as_retired(reply) {
            reservations_by_pool_slot_key.remove(retired_pool_slot_key);
        }
    }

    /// Drop every import: the connection the service would have sent their
    /// retirement notices on is gone.
    pub fn drop_every_import(&self) {
        self.reservations_by_pool_slot_key.lock().clear();
    }

    /// How many pool slots this importer holds an import of.
    #[cfg(feature = "test-support")]
    pub fn held_pool_slot_count(&self) -> usize {
        self.reservations_by_pool_slot_key
            .lock()
            .values()
            .filter(|reservation| reservation.get().is_some())
            .count()
    }
}

/// The pool slot keys a surface-share reply lists as retired.
pub fn pool_slots_a_reply_lists_as_retired(
    reply: &serde_json::Value,
) -> impl Iterator<Item = &str> {
    reply
        .get(SURFACE_SHARE_RETIRED_POOL_SLOTS_FIELD)
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|key| key.as_str())
}

/// The pool slot a published frame id (`<slot>#<generation>`) names; `None`
/// for every other surface id, which is imported per check-out and never
/// cached.
pub fn pool_slot_of_a_published_frame_id(surface_id: &str) -> Option<&str> {
    split_pool_slot_and_frame_generation(surface_id).map(|(pool_slot, _)| pool_slot)
}
