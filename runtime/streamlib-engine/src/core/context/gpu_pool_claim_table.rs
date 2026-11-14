// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The claim tables GPU pools share with the interpreters reading their frames.
//!
//! Each table is named by a UUID, the `pool` a `GpuPoolSlotFrame` descriptor
//! carries, and leases one row to each slot published from it. A pool past
//! [`GPU_POOL_CLAIM_TABLE_ROWS`] slots leases from a second table. Each
//! interpreter is assigned a column at its hand-over, and the column is
//! cleared only once that interpreter's process has been reaped.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use streamlib_ipc_types::{
    GPU_POOL_CLAIM_TABLE_CLAIMANT_COLUMNS, GPU_POOL_CLAIM_TABLE_RETIRING_GENERATION_BIT,
    GPU_POOL_CLAIM_TABLE_ROWS, GpuPoolClaimTableRow, GpuPoolClaimTableSharedRows,
    GpuPoolClaimantColumn,
};
use streamlib_surface_client::GpuPoolClaimTableRowLeasedToASlot;

use super::SurfaceStore;
use crate::core::{Error, Result};

/// The process a claimant column is assigned to: its pid and which process
/// holding that pid it is, so a later process that reuses the pid is a
/// different claimant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GpuPoolClaimantProcessId {
    pub pid: u32,
    /// Differs for every process that holds the pid: its start time in clock
    /// ticks since boot on Linux, its audit pid version on macOS.
    pub incarnation_of_the_pid: u64,
}

/// What a table knows of one of its rows besides the shared counts.
#[derive(Default)]
struct GpuPoolClaimTableRowOccupant {
    /// The pool slot key of the slot the row is leased to.
    pool_slot_key: Option<String>,
    /// The number of the row's current lease, as its shared row carries it.
    row_lease_number: u64,
}

#[derive(Default)]
struct GpuPoolClaimTableBookkeeping {
    occupant_by_row: Vec<GpuPoolClaimTableRowOccupant>,
    /// Rows no slot holds and no claim is left on.
    vacant_rows: Vec<u32>,
    column_by_claimant: HashMap<GpuPoolClaimantProcessId, GpuPoolClaimantColumn>,
}

/// One claim table: its UUID, its shared rows, and which slot and claimant
/// each row and column belongs to.
pub struct GpuPoolClaimTable {
    pool_uuid: [u8; 16],
    pool_name: String,
    shared_rows: GpuPoolClaimTableSharedRows,
    bookkeeping: Mutex<GpuPoolClaimTableBookkeeping>,
}

impl GpuPoolClaimTable {
    fn create(pool_name: &str) -> Result<Self> {
        let pool_uuid = *uuid::Uuid::new_v4().as_bytes();
        Ok(Self {
            pool_uuid,
            pool_name: pool_name.to_string(),
            shared_rows: GpuPoolClaimTableSharedRows::create(pool_name).map_err(
                |claim_table_failure| {
                    Error::Runtime(format!(
                        "GPU pool {pool_name:?} could not create its claim table: \
                         {claim_table_failure}"
                    ))
                },
            )?,
            bookkeeping: Mutex::new(GpuPoolClaimTableBookkeeping {
                occupant_by_row: (0..GPU_POOL_CLAIM_TABLE_ROWS)
                    .map(|_| GpuPoolClaimTableRowOccupant::default())
                    .collect(),
                vacant_rows: (0..GPU_POOL_CLAIM_TABLE_ROWS as u32).rev().collect(),
                column_by_claimant: HashMap::new(),
            }),
        })
    }

    /// The UUID a `GpuPoolSlotFrame` descriptor names this table by.
    pub fn pool_uuid(&self) -> [u8; 16] {
        self.pool_uuid
    }

    /// The pool this table serves, for messages.
    pub fn pool_name(&self) -> &str {
        &self.pool_name
    }

    /// The shared rows, for handing the table to another process.
    pub(crate) fn shared_rows(&self) -> &GpuPoolClaimTableSharedRows {
        &self.shared_rows
    }

    fn row(&self, row_index: u32) -> &GpuPoolClaimTableRow {
        &self.shared_rows.rows()[row_index as usize]
    }

    fn bookkeeping(&self) -> std::sync::MutexGuard<'_, GpuPoolClaimTableBookkeeping> {
        self.bookkeeping
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Each leased row, the pool slot key of the slot holding it, and the
    /// lease's number.
    pub fn rows_leased_to_slots(&self) -> Vec<GpuPoolClaimTableRowLeasedToASlot> {
        self.bookkeeping()
            .occupant_by_row
            .iter()
            .enumerate()
            .filter_map(|(row_index, occupant)| {
                occupant.pool_slot_key.clone().map(|pool_slot_key| {
                    GpuPoolClaimTableRowLeasedToASlot {
                        row_index: row_index as u32,
                        pool_slot_key,
                        row_lease_number: occupant.row_lease_number,
                    }
                })
            })
            .collect()
    }

    /// The column `claimant` claims in, assigned on its first hand-over;
    /// refused past [`GPU_POOL_CLAIM_TABLE_CLAIMANT_COLUMNS`] claimants.
    pub fn claimant_column_of(
        &self,
        claimant: GpuPoolClaimantProcessId,
    ) -> Result<GpuPoolClaimantColumn> {
        let mut bookkeeping = self.bookkeeping();
        if let Some(&column) = bookkeeping.column_by_claimant.get(&claimant) {
            return Ok(column);
        }
        let column = (0..GPU_POOL_CLAIM_TABLE_CLAIMANT_COLUMNS)
            .filter_map(GpuPoolClaimantColumn::at)
            .find(|column| {
                !bookkeeping
                    .column_by_claimant
                    .values()
                    .any(|taken| taken == column)
            })
            .ok_or_else(|| Error::GpuPoolClaimantColumnsExhausted {
                pool: self.pool_name.clone(),
                claimant_column_limit: GPU_POOL_CLAIM_TABLE_CLAIMANT_COLUMNS,
            })?;
        bookkeeping.column_by_claimant.insert(claimant, column);
        Ok(column)
    }

    /// Clear and free `claimant`'s column: its process has been reaped, so
    /// nothing it claimed is still read. Answers whether it held one.
    fn clear_the_column_of_a_reaped_claimant(&self, claimant: GpuPoolClaimantProcessId) -> bool {
        let mut bookkeeping = self.bookkeeping();
        let Some(column) = bookkeeping.column_by_claimant.remove(&claimant) else {
            return false;
        };
        for row in self.shared_rows.rows() {
            row.clear_claimant_column(column);
        }
        drop(bookkeeping);
        true
    }

    fn lease_a_row(self: &Arc<Self>, pool_slot_key: &str) -> Option<GpuPoolClaimTableRowLease> {
        let row_index = {
            let mut bookkeeping = self.bookkeeping();
            let row_index = bookkeeping.vacant_rows.pop()?;
            let occupant = &mut bookkeeping.occupant_by_row[row_index as usize];
            occupant.pool_slot_key = Some(pool_slot_key.to_string());
            occupant.row_lease_number = self.row(row_index).begin_a_lease();
            row_index
        };
        let generation_before_this_slot = self.row(row_index).published_generation()
            & !GPU_POOL_CLAIM_TABLE_RETIRING_GENERATION_BIT;
        Some(GpuPoolClaimTableRowLease {
            table: Arc::clone(self),
            row_index,
            generation_before_this_slot,
        })
    }

    fn vacate_row(&self, row_index: u32) {
        let row = self.row(row_index);
        row.retire_every_frame_it_published();
        let mut bookkeeping = self.bookkeeping();
        bookkeeping.occupant_by_row[row_index as usize].pool_slot_key = None;
        if row.is_claimed() {
            tracing::warn!(
                "GPU pool {}: a slot left row {row_index} while a claim was still on it; the row \
                 is not leased again",
                self.pool_name
            );
            return;
        }
        bookkeeping.vacant_rows.push(row_index);
    }
}

trait RetireEveryFrameItPublished {
    fn retire_every_frame_it_published(&self);
}

impl RetireEveryFrameItPublished for GpuPoolClaimTableRow {
    fn retire_every_frame_it_published(&self) {
        let published_generation = self.published_generation();
        if published_generation & GPU_POOL_CLAIM_TABLE_RETIRING_GENERATION_BIT == 0 {
            let _held_or_retired = self.retire_for_a_rehand(published_generation);
        }
    }
}

/// One slot's row of a claim table, held for as long as the slot lives.
pub struct GpuPoolClaimTableRowLease {
    table: Arc<GpuPoolClaimTable>,
    row_index: u32,
    generation_before_this_slot: u64,
}

impl GpuPoolClaimTableRowLease {
    /// The row's shared counts.
    pub fn row(&self) -> &GpuPoolClaimTableRow {
        self.table.row(self.row_index)
    }

    /// The row's index, the `slot` a `GpuPoolSlotFrame` descriptor carries.
    pub fn row_index(&self) -> u32 {
        self.row_index
    }

    /// The table the row is in.
    pub fn table(&self) -> &Arc<GpuPoolClaimTable> {
        &self.table
    }

    /// The last generation an earlier slot published over this row, which
    /// this slot's first frame must pass.
    pub fn generation_before_this_slot(&self) -> u64 {
        self.generation_before_this_slot
    }
}

impl Drop for GpuPoolClaimTableRowLease {
    fn drop(&mut self) {
        self.table.vacate_row(self.row_index);
    }
}

/// Every claim table the engine's GPU pools lease rows from, by UUID.
#[derive(Default)]
pub struct GpuPoolClaimTables {
    table_by_pool_uuid: Mutex<HashMap<[u8; 16], Weak<GpuPoolClaimTable>>>,
}

impl GpuPoolClaimTables {
    /// The table a `GpuPoolSlotFrame` descriptor names, while a pool holds it.
    pub fn table_named(&self, pool_uuid: &[u8; 16]) -> Option<Arc<GpuPoolClaimTable>> {
        self.table_by_pool_uuid
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(pool_uuid)
            .and_then(Weak::upgrade)
    }

    fn create_table(&self, pool_name: &str) -> Result<Arc<GpuPoolClaimTable>> {
        let table = Arc::new(GpuPoolClaimTable::create(pool_name)?);
        let mut table_by_pool_uuid = self
            .table_by_pool_uuid
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        table_by_pool_uuid.retain(|_, table| table.strong_count() > 0);
        table_by_pool_uuid.insert(table.pool_uuid, Arc::downgrade(&table));
        Ok(table)
    }

    /// Clear the column a reaped process held in every table. Answers how
    /// many tables it held one in.
    pub fn clear_every_column_of_a_reaped_claimant(
        &self,
        claimant: GpuPoolClaimantProcessId,
    ) -> usize {
        let tables: Vec<Arc<GpuPoolClaimTable>> = self
            .table_by_pool_uuid
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        tables
            .iter()
            .filter(|table| table.clear_the_column_of_a_reaped_claimant(claimant))
            .count()
    }
}

/// The claim tables one GPU pool leases its slots' rows from.
pub struct GpuPoolClaimTablesOfOnePool {
    pool_name: String,
    tables: Vec<Arc<GpuPoolClaimTable>>,
}

impl GpuPoolClaimTablesOfOnePool {
    /// A pool with no table yet; its first slot creates one.
    pub fn named(pool_name: impl Into<String>) -> Self {
        Self {
            pool_name: pool_name.into(),
            tables: Vec::new(),
        }
    }

    /// A row for the slot `pool_slot_key`, from a table this pool holds or a
    /// new one when every row is leased.
    pub fn lease_a_row_for_a_slot(
        &mut self,
        every_claim_table: &GpuPoolClaimTables,
        pool_slot_key: &str,
    ) -> Result<GpuPoolClaimTableRowLease> {
        if let Some(lease) = self
            .tables
            .iter()
            .find_map(|table| table.lease_a_row(pool_slot_key))
        {
            return Ok(lease);
        }
        let table = every_claim_table.create_table(&self.pool_name)?;
        let lease = table.lease_a_row(pool_slot_key).ok_or_else(|| {
            Error::Runtime(format!(
                "a fresh claim table of GPU pool {} leased no row",
                self.pool_name
            ))
        })?;
        self.tables.push(table);
        Ok(lease)
    }
}

/// A claim table row for a slot registered through `surface_store`, so other
/// processes claim its frames in shared memory; `None` when no service shares
/// the pool, or when no row could be leased, in which case its frames cross by
/// name and are checked out.
pub(crate) fn gpu_pool_claim_table_row_for_a_shared_slot(
    claim_tables_of_the_pool: &mut GpuPoolClaimTablesOfOnePool,
    surface_store: Option<&SurfaceStore>,
    pool_slot_key: &str,
) -> Option<GpuPoolClaimTableRowLease> {
    let check_out_leases = surface_store?.check_out_leases()?;
    match claim_tables_of_the_pool
        .lease_a_row_for_a_slot(check_out_leases.gpu_pool_claim_tables(), pool_slot_key)
    {
        Ok(row) => Some(row),
        Err(refusal) => {
            tracing::warn!(
                "pool slot {pool_slot_key}: no claim table row ({refusal}); its frames cross by \
                 name and are checked out"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_ipc_types::GpuPoolSlotRehandTest;

    #[test]
    fn a_pool_past_one_tables_rows_leases_from_a_second_named_table() {
        let every_claim_table = GpuPoolClaimTables::default();
        let mut pool = GpuPoolClaimTablesOfOnePool::named("wide-pool");

        let leases: Vec<_> = (0..=GPU_POOL_CLAIM_TABLE_ROWS)
            .map(|slot| {
                pool.lease_a_row_for_a_slot(&every_claim_table, &format!("slot-{slot}"))
                    .unwrap()
            })
            .collect();

        let first_table = leases[0].table().pool_uuid();
        let second_table = leases[GPU_POOL_CLAIM_TABLE_ROWS].table().pool_uuid();
        assert_ne!(first_table, second_table);
        assert!(every_claim_table.table_named(&first_table).is_some());
        assert_eq!(
            every_claim_table
                .table_named(&second_table)
                .unwrap()
                .rows_leased_to_slots(),
            vec![GpuPoolClaimTableRowLeasedToASlot {
                row_index: 0,
                pool_slot_key: format!("slot-{GPU_POOL_CLAIM_TABLE_ROWS}"),
                row_lease_number: 1,
            }]
        );
    }

    #[test]
    fn a_row_leased_again_starts_past_every_generation_the_slot_before_published() {
        let every_claim_table = GpuPoolClaimTables::default();
        let mut pool = GpuPoolClaimTablesOfOnePool::named("reused-rows");
        let first_slot = pool
            .lease_a_row_for_a_slot(&every_claim_table, "first")
            .unwrap();
        first_slot.row().publish_generation(41);
        let row_index = first_slot.row_index();
        drop(first_slot);

        let second_slot = pool
            .lease_a_row_for_a_slot(&every_claim_table, "second")
            .unwrap();

        assert_eq!(second_slot.row_index(), row_index);
        assert_eq!(second_slot.generation_before_this_slot(), 41);
        assert_eq!(
            second_slot.row().lease_number(),
            2,
            "a reader that mapped the row under the first lease sees its mapping is stale"
        );
        assert!(
            second_slot
                .row()
                .claim_frame(GpuPoolClaimantColumn::at(0).unwrap(), 41)
                .is_err(),
            "the slot before's last frame is retired"
        );
    }

    #[test]
    fn a_sixty_fifth_claimant_is_refused_naming_the_pool_and_the_limit() {
        let every_claim_table = GpuPoolClaimTables::default();
        let mut pool = GpuPoolClaimTablesOfOnePool::named("crowded");
        let lease = pool
            .lease_a_row_for_a_slot(&every_claim_table, "slot")
            .unwrap();
        for claimant in 0..GPU_POOL_CLAIM_TABLE_CLAIMANT_COLUMNS as u32 {
            lease
                .table()
                .claimant_column_of(GpuPoolClaimantProcessId {
                    pid: claimant,
                    incarnation_of_the_pid: 0,
                })
                .unwrap();
        }

        let refusal = lease
            .table()
            .claimant_column_of(GpuPoolClaimantProcessId {
                pid: 9_999,
                incarnation_of_the_pid: 0,
            })
            .unwrap_err()
            .to_string();

        assert!(
            refusal.contains("crowded") && refusal.contains("64"),
            "{refusal}"
        );
    }

    #[test]
    fn a_reaped_claimants_column_clears_in_every_table_and_frees_for_another() {
        let every_claim_table = GpuPoolClaimTables::default();
        let mut pool = GpuPoolClaimTablesOfOnePool::named("reaped");
        let lease = pool
            .lease_a_row_for_a_slot(&every_claim_table, "slot")
            .unwrap();
        lease.row().publish_generation(3);
        let reaped = GpuPoolClaimantProcessId {
            pid: 4242,
            incarnation_of_the_pid: 0,
        };
        let column = lease.table().claimant_column_of(reaped).unwrap();
        lease.row().claim_frame(column, 3).unwrap();
        assert_eq!(
            lease.row().retire_for_a_rehand(3),
            GpuPoolSlotRehandTest::HeldByAClaim
        );

        assert_eq!(
            every_claim_table.clear_every_column_of_a_reaped_claimant(reaped),
            1
        );

        assert_eq!(
            lease.row().retire_for_a_rehand(3),
            GpuPoolSlotRehandTest::RetiredForTheRehand
        );
        assert_eq!(
            lease
                .table()
                .claimant_column_of(GpuPoolClaimantProcessId {
                    pid: 1,
                    incarnation_of_the_pid: 0
                })
                .unwrap(),
            column
        );
    }

    #[test]
    fn a_later_process_reusing_a_claimants_pid_is_handed_its_own_column() {
        let every_claim_table = GpuPoolClaimTables::default();
        let mut pool = GpuPoolClaimTablesOfOnePool::named("reused-pid");
        let lease = pool
            .lease_a_row_for_a_slot(&every_claim_table, "slot")
            .unwrap();
        let exited = GpuPoolClaimantProcessId {
            pid: 4242,
            incarnation_of_the_pid: 100,
        };
        let reusing_its_pid = GpuPoolClaimantProcessId {
            pid: 4242,
            incarnation_of_the_pid: 200,
        };

        let column_of_the_exited = lease.table().claimant_column_of(exited).unwrap();
        let column_of_the_reuser = lease.table().claimant_column_of(reusing_its_pid).unwrap();

        assert_ne!(column_of_the_exited, column_of_the_reuser);
    }
}
