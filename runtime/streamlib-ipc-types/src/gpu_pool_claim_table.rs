// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The claim table a GPU pool shares with every interpreter reading its frames:
//! one row per slot, the frame generation the slot currently publishes, and a
//! claim count per claimant column.
//!
//! A reader claims frame `g` by raising its column's count and then loading the
//! row's generation; the engine rehands a slot only after marking its
//! generation retiring and seeing every count still zero. Under one `SeqCst`
//! order a claim either lands before the engine's second count read, which then
//! backs off, or its generation load follows the retiring mark and refuses.
//! Each side fences between its store and its load: the store-then-load pair
//! on two locations is the shape only a `SeqCst` fence orders on every model.

#[cfg(loom)]
use loom::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};
#[cfg(not(loom))]
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

/// Claimant columns one row holds: the most interpreters one pool's frames
/// can be claimed by at once.
pub const GPU_POOL_CLAIM_TABLE_CLAIMANT_COLUMNS: usize = 64;

/// Rows one claim table holds: the most slots one GPU pool can publish from,
/// counting slots retired and replaced over its life.
pub const GPU_POOL_CLAIM_TABLE_ROWS: usize = 64;

/// The bit of a row's published generation that marks the slot as being
/// rehanded: no claim of any of its frames is admitted while it is set.
pub const GPU_POOL_CLAIM_TABLE_RETIRING_GENERATION_BIT: u64 = 1 << 63;

/// Byte offset of [`GpuPoolClaimTableRow`]'s published generation.
pub const GPU_POOL_CLAIM_TABLE_ROW_PUBLISHED_GENERATION_OFFSET: usize = 0;
/// Byte offset of [`GpuPoolClaimTableRow`]'s first claimant column count.
pub const GPU_POOL_CLAIM_TABLE_ROW_CLAIM_COUNTS_OFFSET: usize = 8;
/// Byte offset of [`GpuPoolClaimTableRow`]'s lease number.
pub const GPU_POOL_CLAIM_TABLE_ROW_LEASE_NUMBER_OFFSET: usize = 264;
/// Bytes one [`GpuPoolClaimTableRow`] takes, padding to its 64-byte alignment
/// included.
pub const GPU_POOL_CLAIM_TABLE_ROW_BYTES: usize = 320;
/// Bytes one claim table's shared memory takes.
pub const GPU_POOL_CLAIM_TABLE_BYTES: usize =
    GPU_POOL_CLAIM_TABLE_ROWS * GPU_POOL_CLAIM_TABLE_ROW_BYTES;

/// One slot's row of a GPU pool's claim table, as it lies in shared memory.
///
/// Zeroed memory is a valid row: generation 0, which no published frame
/// carries, and no claims.
#[repr(C, align(64))]
pub struct GpuPoolClaimTableRow {
    published_generation: AtomicU64,
    claim_count_by_claimant_column: [AtomicU32; GPU_POOL_CLAIM_TABLE_CLAIMANT_COLUMNS],
    /// How many slots the row has been leased to, the current one included:
    /// a reader that mapped the row to a slot under an older lease knows its
    /// mapping is stale.
    lease_number: AtomicU64,
}

/// One interpreter's column in a claim table, below
/// [`GPU_POOL_CLAIM_TABLE_CLAIMANT_COLUMNS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GpuPoolClaimantColumn(u8);

impl GpuPoolClaimantColumn {
    /// The column at `column_index`, or `None` past the last one.
    pub fn at(column_index: usize) -> Option<Self> {
        (column_index < GPU_POOL_CLAIM_TABLE_CLAIMANT_COLUMNS).then_some(Self(column_index as u8))
    }

    /// This column's index in a row.
    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// A claim refused because the slot no longer publishes the claimed frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuPoolFrameRecycled {
    /// The generation the claim named.
    pub claimed_generation: u64,
    /// The generation the slot published when the claim was refused, its
    /// retiring bit cleared.
    pub published_generation: u64,
    /// Whether the slot was mid-rehand when the claim was refused.
    pub slot_was_retiring: bool,
}

/// What a rehand test concluded about a slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuPoolSlotRehandTest {
    /// No claim is held and none can land: the slot is retiring until
    /// [`GpuPoolClaimTableRow::publish_generation`] hands it out again.
    RetiredForTheRehand,
    /// A claim is held; the slot publishes the generation it did before.
    HeldByAClaim,
}

impl GpuPoolClaimTableRow {
    /// A row with no published frame and no claims.
    #[cfg(not(loom))]
    pub const fn unpublished() -> Self {
        Self {
            published_generation: AtomicU64::new(0),
            claim_count_by_claimant_column: [const { AtomicU32::new(0) };
                GPU_POOL_CLAIM_TABLE_CLAIMANT_COLUMNS],
            lease_number: AtomicU64::new(0),
        }
    }

    /// A row with no published frame and no claims.
    #[cfg(loom)]
    pub fn unpublished() -> Self {
        Self {
            published_generation: AtomicU64::new(0),
            claim_count_by_claimant_column: std::array::from_fn(|_| AtomicU32::new(0)),
            lease_number: AtomicU64::new(0),
        }
    }

    /// Claim frame `generation` for `column`: the claim holds until
    /// [`Self::release_frame_claim`], and the slot is not rehanded under it.
    pub fn claim_frame(
        &self,
        column: GpuPoolClaimantColumn,
        generation: u64,
    ) -> Result<(), GpuPoolFrameRecycled> {
        if generation & GPU_POOL_CLAIM_TABLE_RETIRING_GENERATION_BIT != 0 {
            let published_generation = self.published_generation.load(Ordering::SeqCst);
            return Err(GpuPoolFrameRecycled {
                claimed_generation: generation,
                published_generation: published_generation
                    & !GPU_POOL_CLAIM_TABLE_RETIRING_GENERATION_BIT,
                slot_was_retiring: true,
            });
        }
        let column_count = &self.claim_count_by_claimant_column[column.index()];
        column_count.fetch_add(1, Ordering::SeqCst);
        fence(Ordering::SeqCst);
        let published_generation = self.published_generation.load(Ordering::SeqCst);
        if published_generation == generation {
            return Ok(());
        }
        column_count.fetch_sub(1, Ordering::SeqCst);
        Err(GpuPoolFrameRecycled {
            claimed_generation: generation,
            published_generation: published_generation
                & !GPU_POOL_CLAIM_TABLE_RETIRING_GENERATION_BIT,
            slot_was_retiring: published_generation & GPU_POOL_CLAIM_TABLE_RETIRING_GENERATION_BIT
                != 0,
        })
    }

    /// Let go of one claim [`Self::claim_frame`] took for `column`.
    pub fn release_frame_claim(&self, column: GpuPoolClaimantColumn) {
        self.claim_count_by_claimant_column[column.index()].fetch_sub(1, Ordering::SeqCst);
    }

    /// Whether any column holds a claim.
    pub fn is_claimed(&self) -> bool {
        self.claim_count_by_claimant_column
            .iter()
            .any(|column_count| column_count.load(Ordering::SeqCst) != 0)
    }

    /// The generation the slot publishes, its retiring bit included.
    pub fn published_generation(&self) -> u64 {
        self.published_generation.load(Ordering::SeqCst)
    }

    /// Test whether the slot publishing `published_generation` can be
    /// rehanded, retiring it when it can. Run by the slot's one owner, under
    /// the lock its ring decides reuse under.
    pub fn retire_for_a_rehand(&self, published_generation: u64) -> GpuPoolSlotRehandTest {
        if self.is_claimed() {
            return GpuPoolSlotRehandTest::HeldByAClaim;
        }
        self.published_generation.store(
            published_generation | GPU_POOL_CLAIM_TABLE_RETIRING_GENERATION_BIT,
            Ordering::SeqCst,
        );
        fence(Ordering::SeqCst);
        if self.is_claimed() {
            self.published_generation
                .store(published_generation, Ordering::SeqCst);
            return GpuPoolSlotRehandTest::HeldByAClaim;
        }
        GpuPoolSlotRehandTest::RetiredForTheRehand
    }

    /// Publish `generation`, admitting claims of it: a fresh slot's first
    /// frame, or a slot [`Self::retire_for_a_rehand`] retired.
    pub fn publish_generation(&self, generation: u64) {
        debug_assert_eq!(generation & GPU_POOL_CLAIM_TABLE_RETIRING_GENERATION_BIT, 0);
        self.published_generation
            .store(generation, Ordering::SeqCst);
    }

    /// Lease the row to a new slot, answering the lease's number. Run by the
    /// table's owner before the slot publishes its first frame.
    pub fn begin_a_lease(&self) -> u64 {
        self.lease_number.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// The number of the row's current lease; 0 for a row never leased.
    pub fn lease_number(&self) -> u64 {
        self.lease_number.load(Ordering::SeqCst)
    }

    /// Clear `column`'s claims: its interpreter's process has been reaped, so
    /// nothing it claimed is still read.
    pub fn clear_claimant_column(&self, column: GpuPoolClaimantColumn) {
        self.claim_count_by_claimant_column[column.index()].store(0, Ordering::SeqCst);
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn a_claim_table_row_is_a_generation_sixty_four_counts_and_a_lease_number_on_a_cache_line_boundary()
     {
        assert_eq!(
            std::mem::size_of::<GpuPoolClaimTableRow>(),
            GPU_POOL_CLAIM_TABLE_ROW_BYTES
        );
        assert_eq!(std::mem::align_of::<GpuPoolClaimTableRow>(), 64);
        assert_eq!(
            std::mem::offset_of!(GpuPoolClaimTableRow, published_generation),
            GPU_POOL_CLAIM_TABLE_ROW_PUBLISHED_GENERATION_OFFSET
        );
        assert_eq!(
            std::mem::offset_of!(GpuPoolClaimTableRow, claim_count_by_claimant_column),
            GPU_POOL_CLAIM_TABLE_ROW_CLAIM_COUNTS_OFFSET
        );
        assert_eq!(
            std::mem::offset_of!(GpuPoolClaimTableRow, lease_number),
            GPU_POOL_CLAIM_TABLE_ROW_LEASE_NUMBER_OFFSET
        );
        assert_eq!(GPU_POOL_CLAIM_TABLE_BYTES, 64 * 320);
    }

    #[test]
    fn each_lease_of_a_row_is_numbered_past_the_one_before() {
        let row = GpuPoolClaimTableRow::unpublished();
        assert_eq!(row.lease_number(), 0);
        assert_eq!(row.begin_a_lease(), 1);
        assert_eq!(row.begin_a_lease(), 2);
        assert_eq!(row.lease_number(), 2);
    }

    #[test]
    fn a_claim_of_the_published_frame_holds_the_slot_until_it_is_released() {
        let row = GpuPoolClaimTableRow::unpublished();
        let column = GpuPoolClaimantColumn::at(3).unwrap();
        row.publish_generation(7);

        row.claim_frame(column, 7).unwrap();
        assert_eq!(
            row.retire_for_a_rehand(7),
            GpuPoolSlotRehandTest::HeldByAClaim
        );
        assert_eq!(row.published_generation(), 7);

        row.release_frame_claim(column);
        assert_eq!(
            row.retire_for_a_rehand(7),
            GpuPoolSlotRehandTest::RetiredForTheRehand
        );
    }

    #[test]
    fn a_claim_of_a_retiring_or_newer_slot_is_refused_and_leaves_no_count() {
        let row = GpuPoolClaimTableRow::unpublished();
        let column = GpuPoolClaimantColumn::at(0).unwrap();
        row.publish_generation(2);
        assert_eq!(
            row.retire_for_a_rehand(2),
            GpuPoolSlotRehandTest::RetiredForTheRehand
        );

        assert_eq!(
            row.claim_frame(column, 2),
            Err(GpuPoolFrameRecycled {
                claimed_generation: 2,
                published_generation: 2,
                slot_was_retiring: true,
            })
        );
        row.publish_generation(3);
        assert_eq!(
            row.claim_frame(column, 2),
            Err(GpuPoolFrameRecycled {
                claimed_generation: 2,
                published_generation: 3,
                slot_was_retiring: false,
            })
        );
        assert!(!row.is_claimed());
    }

    #[test]
    fn a_claim_naming_the_retiring_bit_never_matches_a_retiring_slot() {
        let row = GpuPoolClaimTableRow::unpublished();
        let column = GpuPoolClaimantColumn::at(0).unwrap();
        row.publish_generation(2);
        assert_eq!(
            row.retire_for_a_rehand(2),
            GpuPoolSlotRehandTest::RetiredForTheRehand
        );

        assert!(
            row.claim_frame(column, 2 | GPU_POOL_CLAIM_TABLE_RETIRING_GENERATION_BIT)
                .is_err()
        );
        assert!(!row.is_claimed());
    }

    #[test]
    fn clearing_a_reaped_interpreters_column_frees_the_slot_it_left_claimed() {
        let row = GpuPoolClaimTableRow::unpublished();
        let reaped = GpuPoolClaimantColumn::at(63).unwrap();
        row.publish_generation(1);
        row.claim_frame(reaped, 1).unwrap();
        row.claim_frame(reaped, 1).unwrap();

        row.clear_claimant_column(reaped);

        assert_eq!(
            row.retire_for_a_rehand(1),
            GpuPoolSlotRehandTest::RetiredForTheRehand
        );
    }

    #[test]
    fn there_is_no_sixty_fifth_column() {
        assert!(GpuPoolClaimantColumn::at(63).is_some());
        assert!(GpuPoolClaimantColumn::at(64).is_none());
    }
}
