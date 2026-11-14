// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Every interleaving of claims and a rehand of one claim table row, explored
//! by loom: no reader is left holding a claim on a slot the engine rehanded.
//!
//! Runs only in a loom build:
//! `RUSTFLAGS="--cfg loom" cargo test -p streamlib-ipc-types --release --test gpu_pool_claim_table_loom_model`.

#![cfg(loom)]

use std::sync::atomic::{AtomicBool, Ordering};

use loom::sync::Arc;
use loom::thread;
use streamlib_ipc_types::{GpuPoolClaimTableRow, GpuPoolClaimantColumn, GpuPoolSlotRehandTest};

const PUBLISHED_GENERATION: u64 = 5;

/// Whether the engine is overwriting the slot, and how many readers hold a
/// claim that held; the two never overlap.
#[derive(Default)]
struct SlotUse {
    overwritten_by_the_rehand: loom::sync::atomic::AtomicBool,
    readers_holding_a_claim: loom::sync::atomic::AtomicUsize,
}

fn a_reader_claims_reads_and_releases(
    row: &GpuPoolClaimTableRow,
    slot_use: &SlotUse,
    column: GpuPoolClaimantColumn,
) {
    if row.claim_frame(column, PUBLISHED_GENERATION).is_ok() {
        slot_use
            .readers_holding_a_claim
            .fetch_add(1, loom::sync::atomic::Ordering::SeqCst);
        assert!(
            !slot_use
                .overwritten_by_the_rehand
                .load(loom::sync::atomic::Ordering::SeqCst),
            "a reader holds a claim on a slot the engine is overwriting"
        );
        slot_use
            .readers_holding_a_claim
            .fetch_sub(1, loom::sync::atomic::Ordering::SeqCst);
        row.release_frame_claim(column);
    }
}

fn the_engine_rehands_if_it_can(row: &GpuPoolClaimTableRow, slot_use: &SlotUse) {
    if row.retire_for_a_rehand(PUBLISHED_GENERATION) == GpuPoolSlotRehandTest::RetiredForTheRehand {
        slot_use
            .overwritten_by_the_rehand
            .store(true, loom::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            slot_use
                .readers_holding_a_claim
                .load(loom::sync::atomic::Ordering::SeqCst),
            0,
            "the engine rehanded a slot a reader still claims"
        );
        slot_use
            .overwritten_by_the_rehand
            .store(false, loom::sync::atomic::Ordering::SeqCst);
        row.publish_generation(PUBLISHED_GENERATION + 1);
    }
}

fn model(reader_columns: &'static [usize]) {
    static EXPLORED_A_REHAND: AtomicBool = AtomicBool::new(false);
    loom::model(move || {
        let row = Arc::new(GpuPoolClaimTableRow::unpublished());
        row.publish_generation(PUBLISHED_GENERATION);
        let slot_use = Arc::new(SlotUse::default());

        let readers: Vec<_> = reader_columns
            .iter()
            .map(|&column_index| {
                let row = Arc::clone(&row);
                let slot_use = Arc::clone(&slot_use);
                let column = GpuPoolClaimantColumn::at(column_index).unwrap();
                thread::spawn(move || a_reader_claims_reads_and_releases(&row, &slot_use, column))
            })
            .collect();
        the_engine_rehands_if_it_can(&row, &slot_use);
        for reader in readers {
            reader.join().unwrap();
        }
        if row.published_generation() == PUBLISHED_GENERATION + 1 {
            EXPLORED_A_REHAND.store(true, Ordering::Relaxed);
        }
        assert!(!row.is_claimed(), "a released claim left a count behind");
    });
    assert!(
        EXPLORED_A_REHAND.load(Ordering::Relaxed),
        "no interleaving reached a rehand, so the model proves nothing"
    );
}

#[test]
fn one_reader_and_the_engine_never_share_a_rehanded_slot() {
    model(&[0]);
}

#[test]
fn two_readers_and_the_engine_never_share_a_rehanded_slot() {
    model(&[0, 1]);
}
