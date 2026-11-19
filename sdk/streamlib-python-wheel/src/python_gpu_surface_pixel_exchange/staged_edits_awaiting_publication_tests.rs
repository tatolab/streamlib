// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use pyo3::Python;

use super::pending_staged_write_back_to_surface::{
    PendingStagedWriteBackToSurface, StagedWriteBackSource,
};
use super::staged_edits_awaiting_publication::{
    ReadingTheFrameIn, StagedEditsAwaitingPublication, SurfaceHandleIdentity,
};

const A_FRAME_OF_THE_SLOT: &str = "pool-slot-a#7";
const THE_NEXT_FRAME_OF_THE_SLOT: &str = "pool-slot-a#8";
const A_FRAME_OF_ANOTHER_SLOT: &str = "pool-slot-b#7";

/// Two handles over one surface, as `with frame.cpu()` nested in another
/// over the same frame opens.
struct TwoHandlesOverOneSurface {
    staged_edits: Arc<StagedEditsAwaitingPublication>,
    outer: SurfaceHandleIdentity,
    inner: SurfaceHandleIdentity,
    _memory_each_handle_shares: (Arc<()>, Arc<()>),
}

impl TwoHandlesOverOneSurface {
    fn new() -> Self {
        Python::initialize();
        let outer_memory = Arc::new(());
        let inner_memory = Arc::new(());
        Self {
            staged_edits: Arc::default(),
            outer: SurfaceHandleIdentity::of(&outer_memory),
            inner: SurfaceHandleIdentity::of(&inner_memory),
            _memory_each_handle_shares: (outer_memory, inner_memory),
        }
    }

    fn pool_slot_key() -> &'static str {
        streamlib::sdk::rhi::pool_slot_key_of_surface_id(A_FRAME_OF_THE_SLOT)
    }
}

#[test]
fn every_frame_of_one_pool_slot_shares_the_hold() {
    assert_eq!(
        streamlib::sdk::rhi::pool_slot_key_of_surface_id(A_FRAME_OF_THE_SLOT),
        streamlib::sdk::rhi::pool_slot_key_of_surface_id(THE_NEXT_FRAME_OF_THE_SLOT),
    );
    assert_ne!(
        streamlib::sdk::rhi::pool_slot_key_of_surface_id(A_FRAME_OF_THE_SLOT),
        streamlib::sdk::rhi::pool_slot_key_of_surface_id(A_FRAME_OF_ANOTHER_SLOT),
    );
}

#[test]
fn an_inner_scopes_read_in_over_an_outer_scopes_unpublished_edit_is_refused_by_name() {
    let handles = TwoHandlesOverOneSurface::new();
    let _outer_edit = handles.staged_edits.hold_until_the_edit_settles(
        TwoHandlesOverOneSurface::pool_slot_key(),
        handles.outer,
        StagedWriteBackSource::CpuReadbackStaging,
        true,
    );

    for door in [
        StagedWriteBackSource::CpuReadbackStaging,
        StagedWriteBackSource::DeviceExportStaging,
    ] {
        let refusal = handles
            .staged_edits
            .before_reading_the_frame_in(A_FRAME_OF_THE_SLOT, handles.inner, door)
            .expect_err("a read-in would wipe the outer scope's edit")
            .to_string();
        assert!(
            refusal.contains(A_FRAME_OF_THE_SLOT) && refusal.contains("the CPU array"),
            "the refusal names the surface and the door the edit went through: {refusal}"
        );
    }
}

#[test]
fn the_staging_handle_reads_the_frame_in_once_then_keeps_its_edit() {
    let handles = TwoHandlesOverOneSurface::new();
    let _cpu_door_edit = handles.staged_edits.hold_until_the_edit_settles(
        TwoHandlesOverOneSurface::pool_slot_key(),
        handles.outer,
        StagedWriteBackSource::CpuReadbackStaging,
        false,
    );
    let read_in = || {
        handles
            .staged_edits
            .before_reading_the_frame_in(
                A_FRAME_OF_THE_SLOT,
                handles.outer,
                StagedWriteBackSource::CpuReadbackStaging,
            )
            .expect("the staging handle reads its own staging")
    };

    assert_eq!(read_in(), ReadingTheFrameIn::CopyTheFrameIn);
    assert_eq!(read_in(), ReadingTheFrameIn::KeepTheStagedEdit);
}

#[test]
fn the_staging_handle_reading_through_its_other_door_is_refused() {
    let handles = TwoHandlesOverOneSurface::new();
    let _device_door_edit = handles.staged_edits.hold_until_the_edit_settles(
        TwoHandlesOverOneSurface::pool_slot_key(),
        handles.outer,
        StagedWriteBackSource::DeviceExportStaging,
        true,
    );

    let refusal = handles
        .staged_edits
        .before_reading_the_frame_in(
            A_FRAME_OF_THE_SLOT,
            handles.outer,
            StagedWriteBackSource::CpuReadbackStaging,
        )
        .expect_err("the CPU staging does not hold the device door's edit")
        .to_string();
    assert!(refusal.contains("the device tensor"), "{refusal}");
}

#[test]
fn a_settled_edit_frees_the_slot_and_another_slot_is_never_held() {
    let handles = TwoHandlesOverOneSurface::new();
    let outer_edit = handles.staged_edits.hold_until_the_edit_settles(
        TwoHandlesOverOneSurface::pool_slot_key(),
        handles.outer,
        StagedWriteBackSource::CpuReadbackStaging,
        true,
    );
    assert_eq!(
        handles
            .staged_edits
            .before_reading_the_frame_in(
                A_FRAME_OF_ANOTHER_SLOT,
                handles.inner,
                StagedWriteBackSource::CpuReadbackStaging,
            )
            .expect("another pool slot has its own staging"),
        ReadingTheFrameIn::CopyTheFrameIn
    );

    drop(outer_edit);

    assert_eq!(
        handles
            .staged_edits
            .before_reading_the_frame_in(
                A_FRAME_OF_THE_SLOT,
                handles.inner,
                StagedWriteBackSource::CpuReadbackStaging,
            )
            .expect("nothing is staged once the edit settled"),
        ReadingTheFrameIn::CopyTheFrameIn
    );
}

#[test]
fn an_inner_hold_that_drops_leaves_the_outer_edit_held() {
    let handles = TwoHandlesOverOneSurface::new();
    let _outer_edit = handles.staged_edits.hold_until_the_edit_settles(
        TwoHandlesOverOneSurface::pool_slot_key(),
        handles.outer,
        StagedWriteBackSource::CpuReadbackStaging,
        true,
    );

    drop(handles.staged_edits.hold_until_the_edit_settles(
        TwoHandlesOverOneSurface::pool_slot_key(),
        handles.inner,
        StagedWriteBackSource::CpuReadbackStaging,
        false,
    ));

    assert!(
        handles
            .staged_edits
            .before_reading_the_frame_in(
                A_FRAME_OF_THE_SLOT,
                handles.inner,
                StagedWriteBackSource::CpuReadbackStaging,
            )
            .is_err(),
        "the inner scope's settled hold freed the outer scope's slot"
    );
}

#[test]
fn an_armed_edit_holds_its_slot_until_it_is_discarded() {
    let handles = TwoHandlesOverOneSurface::new();
    let outer_cell = PendingStagedWriteBackToSurface::new_unarmed();
    let holds_taken = std::cell::Cell::new(0);
    let hold = || {
        holds_taken.set(holds_taken.get() + 1);
        Some(handles.staged_edits.hold_until_the_edit_settles(
            TwoHandlesOverOneSurface::pool_slot_key(),
            handles.outer,
            StagedWriteBackSource::CpuReadbackStaging,
            true,
        ))
    };
    outer_cell
        .arm(StagedWriteBackSource::CpuReadbackStaging, hold)
        .expect("the first arm stages");
    outer_cell
        .arm(StagedWriteBackSource::CpuReadbackStaging, || {
            panic!("re-arming takes no second hold")
        })
        .expect("re-arming the same door is one edit");
    assert_eq!(holds_taken.get(), 1);
    let inner_read_in = || {
        handles.staged_edits.before_reading_the_frame_in(
            A_FRAME_OF_THE_SLOT,
            handles.inner,
            StagedWriteBackSource::CpuReadbackStaging,
        )
    };
    assert!(inner_read_in().is_err());

    outer_cell.discard();

    assert_eq!(
        inner_read_in().expect("a discarded edit frees its slot"),
        ReadingTheFrameIn::CopyTheFrameIn
    );
}
