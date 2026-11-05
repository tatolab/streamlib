// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The claim tables this helper was handed, one per GPU pool, and the claims
//! its frames take in them: a pooled frame is held against producer reuse in
//! shared memory, with no surface-share request per frame.

use std::collections::{HashMap, HashSet};
use std::os::fd::OwnedFd;
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use streamlib::sdk::context::{
    GpuPoolClaimTableMemfd, GpuPoolClaimantColumn, GpuPoolFrameRecycled,
    GpuPoolSlotFrameDescriptorBody,
};
use streamlib_surface_client::{GpuPoolClaimTableHandOver, split_pool_slot_and_frame_generation};

/// Live claims kept before the dead entries are swept.
const LIVE_CLAIMS_HELD_BEFORE_A_SWEEP: usize = 256;

/// One pool's claim table, mapped into this helper, and the column this
/// helper's process claims in.
pub(crate) struct HelperGpuPoolClaimTable {
    shared_rows: GpuPoolClaimTableMemfd,
    claimant_column: GpuPoolClaimantColumn,
}

/// One frame this helper claimed in its pool's table; dropping it lets go of
/// the claim. Whatever reads the frame's slot holds this until its own GPU
/// reads have completed.
pub(crate) struct HelperGpuPoolSlotFrameClaim {
    claim_table: Arc<HelperGpuPoolClaimTable>,
    claimed_frame: GpuPoolSlotFrameDescriptorBody,
}

impl HelperGpuPoolSlotFrameClaim {
    /// The descriptor the frame was claimed by.
    pub(crate) fn claimed_frame(&self) -> &GpuPoolSlotFrameDescriptorBody {
        &self.claimed_frame
    }
}

impl Drop for HelperGpuPoolSlotFrameClaim {
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

/// What a claim of a published frame id came to.
pub(crate) enum HelperGpuPoolFrameClaimAttempt {
    /// The frame is claimed until the claim drops.
    Claimed(Arc<HelperGpuPoolSlotFrameClaim>),
    /// The slot no longer publishes the frame.
    Recycled(GpuPoolFrameRecycled),
    /// No claim table this helper holds covers the frame; it is held through
    /// a checkout lease instead.
    NotInAClaimTable,
}

/// This helper's claim tables, the pool slot each of their rows is leased to,
/// and the newest frame of each slot this helper read or acquired.
#[derive(Default)]
pub(crate) struct HelperGpuPoolClaimTables {
    claim_tables_by_pool: Mutex<HashMap<[u8; 16], Arc<HelperGpuPoolClaimTable>>>,
    pools_whose_claim_table_was_refused: Mutex<HashSet<[u8; 16]>>,
    pool_slot_key_by_pool_and_row: Mutex<HashMap<([u8; 16], u32), String>>,
    newest_frame_by_pool_slot_key: Mutex<HashMap<String, GpuPoolSlotFrameDescriptorBody>>,
    live_claims_by_published_frame_id: Mutex<HashMap<String, Weak<HelperGpuPoolSlotFrameClaim>>>,
}

impl HelperGpuPoolClaimTables {
    /// Remember that the service refused `pool`'s claim table, so its frames
    /// are read through checkouts without asking again.
    pub(crate) fn note_a_refused_hand_over(&self, pool: [u8; 16]) {
        self.pools_whose_claim_table_was_refused.lock().insert(pool);
    }

    /// Whether the service refused `pool`'s claim table to this helper.
    pub(crate) fn the_hand_over_was_refused(&self, pool: &[u8; 16]) -> bool {
        self.pools_whose_claim_table_was_refused
            .lock()
            .contains(pool)
    }

    /// Whether this helper read or acquired any pooled frame.
    pub(crate) fn knows_any_pooled_frame(&self) -> bool {
        !self.newest_frame_by_pool_slot_key.lock().is_empty()
    }

    /// Whether this helper holds `frame`'s pool table and knows its row's slot.
    pub(crate) fn knows_the_slot_of(&self, frame: &GpuPoolSlotFrameDescriptorBody) -> bool {
        self.claim_tables_by_pool.lock().contains_key(&frame.pool)
            && self
                .pool_slot_key_by_pool_and_row
                .lock()
                .contains_key(&(frame.pool, frame.slot))
    }

    /// Take in a `claim_table_of_gpu_pool` reply: map the table the first
    /// time, and replace what this helper knew of the pool's rows.
    pub(crate) fn adopt_a_hand_over(
        &self,
        pool: [u8; 16],
        claim_table_memfd: OwnedFd,
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
            let shared_rows = GpuPoolClaimTableMemfd::adopt(claim_table_memfd)
                .map_err(|adoption_failure| adoption_failure.to_string())?;
            unmapped_pool.insert(Arc::new(HelperGpuPoolClaimTable {
                shared_rows,
                claimant_column,
            }));
        }
        let mut pool_slot_key_by_pool_and_row = self.pool_slot_key_by_pool_and_row.lock();
        pool_slot_key_by_pool_and_row.retain(|(row_pool, _), _| *row_pool != pool);
        for (row_index, pool_slot_key) in hand_over.pool_slot_key_by_row {
            pool_slot_key_by_pool_and_row.insert((pool, row_index), pool_slot_key);
        }
        Ok(())
    }

    /// Remember `frame` as the newest of its slot unless a newer one is
    /// known, answering the slot's key; `None` when this helper does not know
    /// the slot its row is leased to.
    pub(crate) fn note_a_pooled_frame(
        &self,
        frame: GpuPoolSlotFrameDescriptorBody,
    ) -> Option<String> {
        let pool_slot_key = self
            .pool_slot_key_by_pool_and_row
            .lock()
            .get(&(frame.pool, frame.slot))
            .cloned()?;
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

    /// Remember `frame` as the newest of the slot `published_frame_id` names:
    /// a frame this helper acquired from its own output pool.
    pub(crate) fn note_an_acquired_pooled_frame(
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

    /// The descriptor of the frame `published_frame_id` names, as this helper
    /// last knew it; `None` when it knows no newer or other frame of the slot.
    pub(crate) fn descriptor_of_the_frame_named(
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
    pub(crate) fn note_the_layout_a_run_left(&self, published_frame_id: &str, image_layout: i32) {
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

    /// The claim this helper still holds on `published_frame_id`, if any.
    pub(crate) fn live_claim_of(
        &self,
        published_frame_id: &str,
    ) -> Option<Arc<HelperGpuPoolSlotFrameClaim>> {
        self.live_claims_by_published_frame_id
            .lock()
            .get(published_frame_id)
            .and_then(Weak::upgrade)
    }

    /// Claim the frame `published_frame_id` names in its pool's table — the
    /// claim this helper already holds on it, or a fresh one.
    pub(crate) fn claim_the_frame_named(
        &self,
        published_frame_id: &str,
    ) -> HelperGpuPoolFrameClaimAttempt {
        if let Some(live_claim) = self.live_claim_of(published_frame_id) {
            return HelperGpuPoolFrameClaimAttempt::Claimed(live_claim);
        }
        let Some(frame) = self.descriptor_of_the_frame_named(published_frame_id) else {
            return HelperGpuPoolFrameClaimAttempt::NotInAClaimTable;
        };
        let Some(claim_table) = self.claim_tables_by_pool.lock().get(&frame.pool).cloned() else {
            return HelperGpuPoolFrameClaimAttempt::NotInAClaimTable;
        };
        let Some(row) = claim_table.shared_rows.rows().get(frame.slot as usize) else {
            return HelperGpuPoolFrameClaimAttempt::NotInAClaimTable;
        };
        if let Err(recycled) = row.claim_frame(claim_table.claimant_column, frame.generation) {
            return HelperGpuPoolFrameClaimAttempt::Recycled(recycled);
        }
        let claim = Arc::new(HelperGpuPoolSlotFrameClaim {
            claim_table,
            claimed_frame: frame,
        });
        let mut live_claims = self.live_claims_by_published_frame_id.lock();
        if live_claims.len() >= LIVE_CLAIMS_HELD_BEFORE_A_SWEEP {
            live_claims.retain(|_, claim| claim.strong_count() > 0);
        }
        live_claims.insert(published_frame_id.to_string(), Arc::downgrade(&claim));
        HelperGpuPoolFrameClaimAttempt::Claimed(claim)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd as _;

    fn a_hand_over_of_one_row(row_index: u32, pool_slot_key: &str) -> GpuPoolClaimTableHandOver {
        GpuPoolClaimTableHandOver {
            claimant_column_index: 2,
            pool_slot_key_by_row: vec![(row_index, pool_slot_key.to_string())],
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

    fn a_table_and_its_engine_side() -> (HelperGpuPoolClaimTables, GpuPoolClaimTableMemfd, [u8; 16])
    {
        let engine_side = GpuPoolClaimTableMemfd::create("helper-claims").unwrap();
        let pool = [7u8; 16];
        let tables = HelperGpuPoolClaimTables::default();
        // SAFETY: F_DUPFD_CLOEXEC of a valid memfd returns a fresh fd.
        let handed_over = unsafe {
            OwnedFd::from_raw_fd(libc::fcntl(
                std::os::fd::AsRawFd::as_raw_fd(&engine_side.memfd()),
                libc::F_DUPFD_CLOEXEC,
                0,
            ))
        };
        tables
            .adopt_a_hand_over(pool, handed_over, a_hand_over_of_one_row(3, "slot-d"))
            .unwrap();
        (tables, engine_side, pool)
    }

    #[test]
    fn a_read_frame_is_claimed_in_shared_memory_until_its_last_holder_drops() {
        let (tables, engine_side, pool) = a_table_and_its_engine_side();
        engine_side.rows()[3].publish_generation(5);
        assert_eq!(
            tables.note_a_pooled_frame(the_frame(pool, 3, 5)).as_deref(),
            Some("slot-d")
        );

        let HelperGpuPoolFrameClaimAttempt::Claimed(claim) =
            tables.claim_the_frame_named("slot-d#5")
        else {
            panic!("the frame read is claimed");
        };
        let HelperGpuPoolFrameClaimAttempt::Claimed(same_claim) =
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
            HelperGpuPoolFrameClaimAttempt::Recycled(GpuPoolFrameRecycled {
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
            HelperGpuPoolFrameClaimAttempt::NotInAClaimTable
        ));
        assert!(matches!(
            tables.claim_the_frame_named("slot-d#4"),
            HelperGpuPoolFrameClaimAttempt::NotInAClaimTable
        ));
    }

    #[test]
    fn a_run_stamps_its_layout_on_the_acquired_frame_it_bound() {
        let tables = HelperGpuPoolClaimTables::default();
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
