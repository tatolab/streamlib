// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The claim tables this importer was handed, one per GPU pool, and the claims
//! its frames take in them: a pooled frame is held against producer reuse in
//! shared memory, with no surface-share request per frame.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use streamlib_ipc_types::{
    GpuPoolClaimTableSharedRows, GpuPoolClaimantColumn, GpuPoolFrameRecycled,
    GpuPoolSlotFrameDescriptorBody,
};
use streamlib_surface_client::{
    GpuPoolClaimTableHandOver, SURFACE_SHARE_CLAIM_TABLE_OF_GPU_POOL_OP,
    SURFACE_SHARE_GPU_POOL_FIELD, gpu_pool_uuid_wire_name, published_frame_id_of_pool_slot,
    refusal_the_surface_share_service_answered, split_pool_slot_and_frame_generation,
};

use crate::{SurfaceShareConnectionOfThisImporter, SurfaceShareTransferredHandle};

/// Live claims kept before the dead entries are swept.
const LIVE_CLAIMS_HELD_BEFORE_A_SWEEP: usize = 256;

/// One pool's claim table, mapped into this importer, and the column this
/// importer's process claims in.
pub struct GpuPoolClaimTableOfThisImporter {
    shared_rows: GpuPoolClaimTableSharedRows,
    claimant_column: GpuPoolClaimantColumn,
}

/// One frame this importer claimed in its pool's table; dropping it lets go of
/// the claim. Whatever reads the frame's slot holds this until its own GPU
/// reads have completed.
pub struct GpuPoolSlotFrameClaimOfThisImporter {
    claim_table: Arc<GpuPoolClaimTableOfThisImporter>,
    claimed_frame: GpuPoolSlotFrameDescriptorBody,
    pool_slot_key: String,
}

impl GpuPoolSlotFrameClaimOfThisImporter {
    /// The descriptor the frame was claimed by.
    pub fn claimed_frame(&self) -> &GpuPoolSlotFrameDescriptorBody {
        &self.claimed_frame
    }

    /// The key of the pool slot the frame was claimed over.
    pub fn pool_slot_key(&self) -> &str {
        &self.pool_slot_key
    }
}

impl Drop for GpuPoolSlotFrameClaimOfThisImporter {
    fn drop(&mut self) {
        if let Some(row) = self
            .claim_table
            .shared_rows
            .rows()
            .get(self.claimed_frame.slot as usize)
        {
            row.release_frame_claim(self.claim_table.claimant_column);
        }
    }
}

/// Why a pool's claim table hand-over did not land: a request that never got
/// an answer, or a table this process could not map, is asked again on the
/// next frame; an answered refusal is final.
enum GpuPoolClaimTableHandOverFailure {
    RequestFailed(String),
    AdoptionFailed(String),
    Refused(String),
}

/// What a claim of a published frame id came to.
pub enum GpuPoolFrameClaimAttemptOfThisImporter {
    /// The frame is claimed until the claim drops.
    Claimed(Arc<GpuPoolSlotFrameClaimOfThisImporter>),
    /// The slot no longer publishes the frame.
    Recycled(GpuPoolFrameRecycled),
    /// No claim table this importer holds covers the frame; it is held through
    /// a checkout lease instead.
    NotInAClaimTable,
}

/// The slot a claim table row was leased to when this importer was handed
/// the table, under the lease that row carried.
struct PoolSlotLeasedARow {
    pool_slot_key: String,
    row_lease_number: u64,
}

/// This importer's claim tables, the pool slot each of their rows is leased to,
/// and the newest frame of each slot this importer read or acquired.
#[derive(Default)]
pub struct GpuPoolClaimTablesOfThisImporter {
    claim_tables_by_pool: Mutex<HashMap<[u8; 16], Arc<GpuPoolClaimTableOfThisImporter>>>,
    pools_whose_claim_table_was_refused: Mutex<HashSet<[u8; 16]>>,
    leased_slot_by_pool_and_row: Mutex<HashMap<([u8; 16], u32), PoolSlotLeasedARow>>,
    newest_frame_by_pool_slot_key: Mutex<HashMap<String, GpuPoolSlotFrameDescriptorBody>>,
    live_claims_by_published_frame_id:
        Mutex<HashMap<String, Weak<GpuPoolSlotFrameClaimOfThisImporter>>>,
}

impl GpuPoolClaimTablesOfThisImporter {
    /// Remember that the service refused `pool`'s claim table, so its frames
    /// are read through checkouts without asking again.
    pub fn note_a_refused_hand_over(&self, pool: [u8; 16]) {
        self.pools_whose_claim_table_was_refused.lock().insert(pool);
    }

    /// Whether the service refused `pool`'s claim table to this importer.
    pub fn the_hand_over_was_refused(&self, pool: &[u8; 16]) -> bool {
        self.pools_whose_claim_table_was_refused
            .lock()
            .contains(pool)
    }

    /// Whether this importer holds `frame`'s pool table and knows the slot its
    /// row is leased to now: a row the engine leased again since the hand-over
    /// carries a newer lease number, and its slot is unknown until the next.
    pub fn knows_the_slot_of(&self, frame: &GpuPoolSlotFrameDescriptorBody) -> bool {
        let Some(claim_table) = self.claim_tables_by_pool.lock().get(&frame.pool).cloned() else {
            return false;
        };
        let Some(row) = claim_table.shared_rows.rows().get(frame.slot as usize) else {
            return false;
        };
        self.leased_slot_by_pool_and_row
            .lock()
            .get(&(frame.pool, frame.slot))
            .is_some_and(|leased_slot| leased_slot.row_lease_number == row.lease_number())
    }

    /// Take in a `claim_table_of_gpu_pool` reply: map the table the first
    /// time, and replace what this importer knew of the pool's rows.
    pub fn adopt_a_hand_over(
        &self,
        pool: [u8; 16],
        claim_table_shared_rows_handle: SurfaceShareTransferredHandle,
        hand_over: GpuPoolClaimTableHandOver,
    ) -> Result<(), String> {
        let claimant_column = GpuPoolClaimantColumn::at(hand_over.claimant_column_index)
            .ok_or_else(|| {
                format!(
                    "the claim table hand-over names claimant column {}, past the table's last",
                    hand_over.claimant_column_index
                )
            })?;
        if let std::collections::hash_map::Entry::Vacant(unmapped_pool) =
            self.claim_tables_by_pool.lock().entry(pool)
        {
            let shared_rows = GpuPoolClaimTableSharedRows::adopt(claim_table_shared_rows_handle)
                .map_err(|adoption_failure| adoption_failure.to_string())?;
            unmapped_pool.insert(Arc::new(GpuPoolClaimTableOfThisImporter {
                shared_rows,
                claimant_column,
            }));
        }
        let mut leased_slot_by_pool_and_row = self.leased_slot_by_pool_and_row.lock();
        leased_slot_by_pool_and_row.retain(|(row_pool, _), _| *row_pool != pool);
        for leased_row in hand_over.rows_leased_to_slots {
            leased_slot_by_pool_and_row.insert(
                (pool, leased_row.row_index),
                PoolSlotLeasedARow {
                    pool_slot_key: leased_row.pool_slot_key,
                    row_lease_number: leased_row.row_lease_number,
                },
            );
        }
        Ok(())
    }

    /// Remember `frame` as the newest of its slot unless a newer one is
    /// known, answering the slot's key; `None` when this importer does not know
    /// the slot its row is leased to.
    pub fn note_a_pooled_frame(&self, frame: GpuPoolSlotFrameDescriptorBody) -> Option<String> {
        let pool_slot_key = self.pool_slot_key_of(&frame)?;
        self.newest_frame_by_pool_slot_key
            .lock()
            .entry(pool_slot_key.clone())
            .and_modify(|newest| {
                if frame.generation > newest.generation {
                    *newest = frame;
                }
            })
            .or_insert(frame);
        Some(pool_slot_key)
    }

    /// The key of the slot `frame`'s claim table row is leased to, when this
    /// importer knows it.
    pub fn pool_slot_key_of(&self, frame: &GpuPoolSlotFrameDescriptorBody) -> Option<String> {
        self.leased_slot_by_pool_and_row
            .lock()
            .get(&(frame.pool, frame.slot))
            .map(|leased_slot| leased_slot.pool_slot_key.clone())
    }

    /// Remember `frame` as the newest of the slot `published_frame_id` names:
    /// a frame this importer acquired from its own output pool.
    pub fn note_an_acquired_pooled_frame(
        &self,
        published_frame_id: &str,
        frame: GpuPoolSlotFrameDescriptorBody,
    ) {
        if let Some((pool_slot_key, _)) = split_pool_slot_and_frame_generation(published_frame_id) {
            self.newest_frame_by_pool_slot_key
                .lock()
                .insert(pool_slot_key.to_string(), frame);
        }
    }

    /// The descriptor of the frame `published_frame_id` names, as this importer
    /// last knew it; `None` when it knows no newer or other frame of the slot.
    pub fn descriptor_of_the_frame_named(
        &self,
        published_frame_id: &str,
    ) -> Option<GpuPoolSlotFrameDescriptorBody> {
        let (pool_slot_key, generation) = split_pool_slot_and_frame_generation(published_frame_id)?;
        self.newest_frame_by_pool_slot_key
            .lock()
            .get(pool_slot_key)
            .filter(|frame| frame.generation == generation)
            .copied()
    }

    /// Record the layout a run left the frame `published_frame_id` names in.
    pub fn note_the_layout_a_run_left(&self, published_frame_id: &str, image_layout: i32) {
        let Some((pool_slot_key, generation)) =
            split_pool_slot_and_frame_generation(published_frame_id)
        else {
            return;
        };
        if let Some(frame) = self
            .newest_frame_by_pool_slot_key
            .lock()
            .get_mut(pool_slot_key)
            .filter(|frame| frame.generation == generation)
        {
            frame.image_layout = image_layout;
        }
    }

    /// The claim this importer still holds on `published_frame_id`, if any.
    pub fn live_claim_of(
        &self,
        published_frame_id: &str,
    ) -> Option<Arc<GpuPoolSlotFrameClaimOfThisImporter>> {
        self.live_claims_by_published_frame_id
            .lock()
            .get(published_frame_id)
            .and_then(Weak::upgrade)
    }

    /// Claim the frame `published_frame_id` names in its pool's table — the
    /// claim this importer already holds on it, or a fresh one.
    pub fn claim_the_frame_named(
        &self,
        published_frame_id: &str,
    ) -> GpuPoolFrameClaimAttemptOfThisImporter {
        if let Some(live_claim) = self.live_claim_of(published_frame_id) {
            return GpuPoolFrameClaimAttemptOfThisImporter::Claimed(live_claim);
        }
        let Some(frame) = self.descriptor_of_the_frame_named(published_frame_id) else {
            return GpuPoolFrameClaimAttemptOfThisImporter::NotInAClaimTable;
        };
        let Some(claim_table) = self.claim_tables_by_pool.lock().get(&frame.pool).cloned() else {
            return GpuPoolFrameClaimAttemptOfThisImporter::NotInAClaimTable;
        };
        let Some(row) = claim_table.shared_rows.rows().get(frame.slot as usize) else {
            return GpuPoolFrameClaimAttemptOfThisImporter::NotInAClaimTable;
        };
        if let Err(recycled) = row.claim_frame(claim_table.claimant_column, frame.generation) {
            return GpuPoolFrameClaimAttemptOfThisImporter::Recycled(recycled);
        }
        let Some((pool_slot_key, _)) = split_pool_slot_and_frame_generation(published_frame_id)
        else {
            return GpuPoolFrameClaimAttemptOfThisImporter::NotInAClaimTable;
        };
        let claim = Arc::new(GpuPoolSlotFrameClaimOfThisImporter {
            claim_table,
            claimed_frame: frame,
            pool_slot_key: pool_slot_key.to_string(),
        });
        let mut live_claims = self.live_claims_by_published_frame_id.lock();
        if live_claims.len() >= LIVE_CLAIMS_HELD_BEFORE_A_SWEEP {
            live_claims.retain(|_, claim| claim.strong_count() > 0);
        }
        live_claims.insert(published_frame_id.to_string(), Arc::downgrade(&claim));
        GpuPoolFrameClaimAttemptOfThisImporter::Claimed(claim)
    }

    /// Claim the frame `published_frame_id` names in its pool's table;
    /// `None` for a frame no table this importer holds covers, which a
    /// checkout holds instead, and refused when the slot moved past it.
    pub fn claim_the_frame_named_refusing_a_recycled_one(
        &self,
        published_frame_id: &str,
    ) -> Result<Option<Arc<GpuPoolSlotFrameClaimOfThisImporter>>, String> {
        match self.claim_the_frame_named(published_frame_id) {
            GpuPoolFrameClaimAttemptOfThisImporter::Claimed(claimed) => Ok(Some(claimed)),
            GpuPoolFrameClaimAttemptOfThisImporter::NotInAClaimTable => Ok(None),
            GpuPoolFrameClaimAttemptOfThisImporter::Recycled(recycled) => Err(format!(
                "frame {published_frame_id:?} was recycled: its slot publishes generation {}{}, \
                 and a published frame is never answered with newer pixels",
                recycled.published_generation,
                if recycled.slot_was_retiring {
                    " and is being handed out again"
                } else {
                    ""
                }
            )),
        }
    }

    /// Take in a pooled frame a read carried and claim it, so its slot is
    /// never rehanded while the frame is held: the first frame of a pool, or
    /// of a slot this importer does not know yet, fetches the pool's claim
    /// table and its slot keys over `surface_share_connection` once. A pool
    /// whose table the service refuses, such as one past its last claimant
    /// column, is read through checkouts, and answers no claim.
    pub fn note_and_claim_a_pooled_frame<PoolSlotImport: Clone + Send + Sync + 'static>(
        &self,
        pooled_frame: GpuPoolSlotFrameDescriptorBody,
        surface_share_connection: &SurfaceShareConnectionOfThisImporter<PoolSlotImport>,
    ) -> Result<Option<Arc<GpuPoolSlotFrameClaimOfThisImporter>>, String> {
        if self.the_hand_over_was_refused(&pooled_frame.pool) {
            return Ok(None);
        }
        if !self.knows_the_slot_of(&pooled_frame) {
            match self
                .fetch_the_claim_table_of_gpu_pool(pooled_frame.pool, surface_share_connection)
            {
                Ok(()) => {}
                Err(GpuPoolClaimTableHandOverFailure::RequestFailed(request_failure)) => {
                    tracing::warn!(
                        "GPU pool {}: the claim table request failed ({request_failure}); this \
                         frame is read through a checkout and the next frame asks again",
                        gpu_pool_uuid_wire_name(&pooled_frame.pool)
                    );
                    return Ok(None);
                }
                Err(GpuPoolClaimTableHandOverFailure::AdoptionFailed(adoption_failure)) => {
                    tracing::warn!(
                        "GPU pool {}: this importer could not take in the claim table the \
                         service handed over ({adoption_failure}); this frame is read through a \
                         checkout and the next frame asks again",
                        gpu_pool_uuid_wire_name(&pooled_frame.pool)
                    );
                    return Ok(None);
                }
                Err(GpuPoolClaimTableHandOverFailure::Refused(refusal)) => {
                    tracing::warn!(
                        "GPU pool {}: {refusal}; this importer reads the pool's frames through \
                         checkouts",
                        gpu_pool_uuid_wire_name(&pooled_frame.pool)
                    );
                    self.note_a_refused_hand_over(pooled_frame.pool);
                    return Ok(None);
                }
            }
        }
        let Some(pool_slot_key) = self.note_a_pooled_frame(pooled_frame) else {
            return Err(format!(
                "a frame of GPU pool {} names claim table row {}, which the pool's claim table \
                 hand-over leases to no slot",
                gpu_pool_uuid_wire_name(&pooled_frame.pool),
                pooled_frame.slot
            ));
        };
        self.claim_the_frame_named_refusing_a_recycled_one(&published_frame_id_of_pool_slot(
            &pool_slot_key,
            pooled_frame.generation,
        ))
    }

    fn fetch_the_claim_table_of_gpu_pool<PoolSlotImport: Clone + Send + Sync + 'static>(
        &self,
        pool: [u8; 16],
        surface_share_connection: &SurfaceShareConnectionOfThisImporter<PoolSlotImport>,
    ) -> Result<(), GpuPoolClaimTableHandOverFailure> {
        let pool_wire_name = gpu_pool_uuid_wire_name(&pool);
        let (reply, received_handles) = surface_share_connection
            .request(&serde_json::json!({
                "op": SURFACE_SHARE_CLAIM_TABLE_OF_GPU_POOL_OP,
                SURFACE_SHARE_GPU_POOL_FIELD: pool_wire_name,
            }))
            .map_err(GpuPoolClaimTableHandOverFailure::RequestFailed)?;
        if let Some(refusal) = refusal_the_surface_share_service_answered(&reply) {
            return Err(GpuPoolClaimTableHandOverFailure::Refused(format!(
                "the surface-share service refused the claim table of GPU pool \
                 {pool_wire_name}: {refusal}"
            )));
        }
        let claim_table_shared_rows_handle =
            received_handles.into_iter().next().ok_or_else(|| {
                GpuPoolClaimTableHandOverFailure::Refused(format!(
                    "the claim table hand-over of GPU pool {pool_wire_name} carried no handle to \
                     the table's memory"
                ))
            })?;
        let hand_over = GpuPoolClaimTableHandOver::from_reply(&reply)
            .map_err(GpuPoolClaimTableHandOverFailure::Refused)?;
        self.adopt_a_hand_over(pool, claim_table_shared_rows_handle, hand_over)
            .map_err(GpuPoolClaimTableHandOverFailure::AdoptionFailed)
    }
}

#[cfg(test)]
mod tests {
    use streamlib_surface_client::GpuPoolClaimTableRowLeasedToASlot;

    use super::*;

    fn a_hand_over_of_one_row(
        row_index: u32,
        pool_slot_key: &str,
        row_lease_number: u64,
    ) -> GpuPoolClaimTableHandOver {
        GpuPoolClaimTableHandOver {
            claimant_column_index: 2,
            rows_leased_to_slots: vec![GpuPoolClaimTableRowLeasedToASlot {
                row_index,
                pool_slot_key: pool_slot_key.to_string(),
                row_lease_number,
            }],
        }
    }

    fn the_frame(pool: [u8; 16], slot: u32, generation: u64) -> GpuPoolSlotFrameDescriptorBody {
        GpuPoolSlotFrameDescriptorBody {
            pool,
            slot,
            generation,
            timeline_value: 0,
            image_layout: 0,
        }
    }

    #[cfg(target_os = "linux")]
    fn a_hand_over_handle_of(
        engine_side: &GpuPoolClaimTableSharedRows,
    ) -> SurfaceShareTransferredHandle {
        engine_side.memfd().try_clone_to_owned().unwrap()
    }

    #[cfg(target_os = "macos")]
    fn a_hand_over_handle_of(
        engine_side: &GpuPoolClaimTableSharedRows,
    ) -> SurfaceShareTransferredHandle {
        engine_side.memory_entry_send_right().unwrap()
    }

    fn a_table_and_its_engine_side() -> (
        GpuPoolClaimTablesOfThisImporter,
        GpuPoolClaimTableSharedRows,
        [u8; 16],
    ) {
        let engine_side = GpuPoolClaimTableSharedRows::create("importer-claims").unwrap();
        let pool = [7u8; 16];
        let row_lease_number = engine_side.rows()[3].begin_a_lease();
        let tables = GpuPoolClaimTablesOfThisImporter::default();
        tables
            .adopt_a_hand_over(
                pool,
                a_hand_over_handle_of(&engine_side),
                a_hand_over_of_one_row(3, "slot-d", row_lease_number),
            )
            .unwrap();
        (tables, engine_side, pool)
    }

    #[test]
    fn a_row_the_engine_leased_again_since_the_hand_over_is_a_slot_this_importer_does_not_know() {
        let (tables, engine_side, pool) = a_table_and_its_engine_side();
        engine_side.rows()[3].publish_generation(5);
        assert!(tables.knows_the_slot_of(&the_frame(pool, 3, 5)));

        engine_side.rows()[3].begin_a_lease();
        engine_side.rows()[3].publish_generation(6);

        assert!(
            !tables.knows_the_slot_of(&the_frame(pool, 3, 6)),
            "the row's slot is asked for again rather than read as the slot before's"
        );
    }

    #[test]
    fn a_read_frame_is_claimed_in_shared_memory_until_its_last_holder_drops() {
        let (tables, engine_side, pool) = a_table_and_its_engine_side();
        engine_side.rows()[3].publish_generation(5);
        assert_eq!(
            tables.note_a_pooled_frame(the_frame(pool, 3, 5)).as_deref(),
            Some("slot-d")
        );

        let GpuPoolFrameClaimAttemptOfThisImporter::Claimed(claim) =
            tables.claim_the_frame_named("slot-d#5")
        else {
            panic!("the frame read is claimed");
        };
        let GpuPoolFrameClaimAttemptOfThisImporter::Claimed(same_claim) =
            tables.claim_the_frame_named("slot-d#5")
        else {
            panic!("a second claim of a held frame reuses the first");
        };
        assert!(Arc::ptr_eq(&claim, &same_claim));
        assert!(engine_side.rows()[3].is_claimed());

        drop(claim);
        assert!(engine_side.rows()[3].is_claimed());
        drop(same_claim);
        assert!(!engine_side.rows()[3].is_claimed());
        assert!(tables.live_claim_of("slot-d#5").is_none());
    }

    #[test]
    fn a_frame_the_slot_moved_past_is_refused_as_recycled() {
        let (tables, engine_side, pool) = a_table_and_its_engine_side();
        tables.note_a_pooled_frame(the_frame(pool, 3, 5));
        engine_side.rows()[3].publish_generation(6);

        assert!(matches!(
            tables.claim_the_frame_named("slot-d#5"),
            GpuPoolFrameClaimAttemptOfThisImporter::Recycled(GpuPoolFrameRecycled {
                claimed_generation: 5,
                published_generation: 6,
                ..
            })
        ));
        assert!(!engine_side.rows()[3].is_claimed());
    }

    #[test]
    fn a_frame_no_table_covers_is_left_to_a_checkout() {
        let (tables, _engine_side, pool) = a_table_and_its_engine_side();
        assert!(tables.note_a_pooled_frame(the_frame(pool, 9, 1)).is_none());
        assert!(!tables.knows_the_slot_of(&the_frame(pool, 9, 1)));

        assert!(matches!(
            tables.claim_the_frame_named("slot-z#1"),
            GpuPoolFrameClaimAttemptOfThisImporter::NotInAClaimTable
        ));
        assert!(matches!(
            tables.claim_the_frame_named("slot-d#4"),
            GpuPoolFrameClaimAttemptOfThisImporter::NotInAClaimTable
        ));
    }

    #[test]
    fn a_run_stamps_its_layout_on_the_acquired_frame_it_bound() {
        let tables = GpuPoolClaimTablesOfThisImporter::default();
        tables.note_an_acquired_pooled_frame("slot-a#2", the_frame([1u8; 16], 0, 2));

        tables.note_the_layout_a_run_left("slot-a#2", 1);
        tables.note_the_layout_a_run_left("slot-a#1", 7);

        assert_eq!(
            tables
                .descriptor_of_the_frame_named("slot-a#2")
                .map(|frame| frame.image_layout),
            Some(1)
        );
        assert!(tables.descriptor_of_the_frame_named("slot-a#1").is_none());
    }

    #[test]
    fn a_delayed_older_frame_of_a_slot_never_replaces_the_newer_one_read() {
        let (tables, _engine_side, pool) = a_table_and_its_engine_side();
        tables.note_a_pooled_frame(the_frame(pool, 3, 6));

        tables.note_a_pooled_frame(the_frame(pool, 3, 5));

        assert!(tables.descriptor_of_the_frame_named("slot-d#6").is_some());
        assert!(tables.descriptor_of_the_frame_named("slot-d#5").is_none());
    }
}
