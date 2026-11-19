// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use pyo3::{PyResult, Python};

use super::pending_staged_write_back_to_surface::{
    PendingStagedWriteBackToSurface, StagedWriteBackSource,
};
use super::pool_slots_holding_a_staged_edit::{
    PoolSlotHeldForAStagedEdit, PoolSlotsHoldingAStagedEdit, ReadingTheFrameIn,
    SurfaceHandleIdentity,
};

const A_FRAME_OF_THE_SLOT: &str = "pool-slot-a#7";
const THE_NEXT_FRAME_OF_THE_SLOT: &str = "pool-slot-a#8";
const A_FRAME_OF_ANOTHER_SLOT: &str = "pool-slot-b#7";

/// Two handles over one surface, as `with frame.cpu()` nested in another
/// over the same frame opens.
struct TwoHandlesOverOneSurface {
    pool_slots: Arc<PoolSlotsHoldingAStagedEdit>,
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
            pool_slots: Arc::default(),
            outer: SurfaceHandleIdentity::of_a_stand_in(&outer_memory),
            inner: SurfaceHandleIdentity::of_a_stand_in(&inner_memory),
            _memory_each_handle_shares: (outer_memory, inner_memory),
        }
    }

    fn hold_for(
        &self,
        staged_by: SurfaceHandleIdentity,
        door: StagedWriteBackSource,
    ) -> PyResult<PoolSlotHeldForAStagedEdit> {
        self.pool_slots
            .hold_until_the_edit_settles(A_FRAME_OF_THE_SLOT, staged_by, door)
    }

    fn read_in_by(
        &self,
        reader: SurfaceHandleIdentity,
        door: StagedWriteBackSource,
    ) -> PyResult<ReadingTheFrameIn> {
        self.pool_slots
            .before_reading_the_frame_in(A_FRAME_OF_THE_SLOT, reader, door)
    }
}

const CPU: StagedWriteBackSource = StagedWriteBackSource::CpuReadbackStaging;
const DEVICE: StagedWriteBackSource = StagedWriteBackSource::DeviceExportStaging;

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
    let _outer_edit = handles.hold_for(handles.outer, CPU).unwrap();

    let refusal = handles
        .pool_slots
        .before_reading_the_frame_in(THE_NEXT_FRAME_OF_THE_SLOT, handles.inner, CPU)
        .expect_err("a read-in would wipe the outer scope's edit")
        .to_string();
    assert!(
        refusal.contains(THE_NEXT_FRAME_OF_THE_SLOT) && refusal.contains("the CPU array"),
        "the refusal names the surface and the door the edit went through: {refusal}"
    );
}

#[test]
fn a_read_in_through_the_other_door_leaves_the_edit_in_its_own_staging() {
    let handles = TwoHandlesOverOneSurface::new();
    let _outer_edit = handles.hold_for(handles.outer, CPU).unwrap();

    for reader in [handles.outer, handles.inner] {
        assert_eq!(
            handles.read_in_by(reader, DEVICE).unwrap(),
            ReadingTheFrameIn::CopyTheFrameIn
        );
    }
}

#[test]
fn a_second_edit_over_a_held_slot_is_refused_through_either_door() {
    let handles = TwoHandlesOverOneSurface::new();
    let _outer_edit = handles.hold_for(handles.outer, CPU).unwrap();

    for door in [CPU, DEVICE] {
        let refusal = handles
            .hold_for(handles.inner, door)
            .err()
            .expect("publishing both edits would overwrite one")
            .to_string();
        assert!(
            refusal.contains("the CPU array") && refusal.contains(door.door_in_a_refusal()),
            "{refusal}"
        );
    }
    assert!(
        handles.read_in_by(handles.inner, CPU).is_err(),
        "the refused hold left the outer scope's slot held"
    );
}

#[test]
fn the_cpu_door_reads_the_frame_in_once_then_keeps_its_edit() {
    let handles = TwoHandlesOverOneSurface::new();
    let _cpu_door_edit = handles.hold_for(handles.outer, CPU).unwrap();

    assert_eq!(
        handles.read_in_by(handles.outer, CPU).unwrap(),
        ReadingTheFrameIn::CopyTheFrameIn
    );
    assert_eq!(
        handles.read_in_by(handles.outer, CPU).unwrap(),
        ReadingTheFrameIn::KeepTheStagedEdit
    );
}

#[test]
fn the_device_door_keeps_its_edit_from_its_first_read_in_after_staging() {
    let handles = TwoHandlesOverOneSurface::new();
    let _device_door_edit = handles.hold_for(handles.outer, DEVICE).unwrap();

    assert_eq!(
        handles.read_in_by(handles.outer, DEVICE).unwrap(),
        ReadingTheFrameIn::KeepTheStagedEdit
    );
}

#[test]
fn a_settled_edit_frees_the_slot_and_another_slot_is_never_held() {
    let handles = TwoHandlesOverOneSurface::new();
    let outer_edit = handles.hold_for(handles.outer, CPU).unwrap();
    assert_eq!(
        handles
            .pool_slots
            .before_reading_the_frame_in(A_FRAME_OF_ANOTHER_SLOT, handles.inner, CPU)
            .unwrap(),
        ReadingTheFrameIn::CopyTheFrameIn
    );

    drop(outer_edit);

    assert_eq!(
        handles.read_in_by(handles.inner, CPU).unwrap(),
        ReadingTheFrameIn::CopyTheFrameIn
    );
    let _inner_edit = handles
        .hold_for(handles.inner, CPU)
        .expect("a freed slot takes the next edit");
}

#[test]
fn native_code_is_refused_only_where_it_would_meet_a_staged_edit() {
    let handles = TwoHandlesOverOneSurface::new();
    let pool_slots = &handles.pool_slots;
    {
        let _cpu_door_edit = handles.hold_for(handles.outer, CPU).unwrap();
        let refusal = pool_slots
            .before_native_code_reads_the_frame_in(A_FRAME_OF_THE_SLOT)
            .expect_err("the native read-in shares the CPU staging");
        assert!(refusal.contains("the CPU array"), "{refusal}");
        assert!(
            pool_slots
                .before_native_code_writes_over_the_surface(A_FRAME_OF_THE_SLOT)
                .is_err()
        );
    }
    {
        let _device_door_edit = handles.hold_for(handles.outer, DEVICE).unwrap();
        pool_slots
            .before_native_code_reads_the_frame_in(A_FRAME_OF_THE_SLOT)
            .expect("the device edit sits in other memory");
        assert!(
            pool_slots
                .before_native_code_writes_over_the_surface(A_FRAME_OF_THE_SLOT)
                .is_err(),
            "the device edit's publish would overwrite the native write"
        );
    }
    pool_slots
        .before_native_code_writes_over_the_surface(A_FRAME_OF_THE_SLOT)
        .expect("nothing is staged once both edits settled");
}

#[test]
fn an_armed_edit_holds_its_slot_until_it_is_discarded() {
    let handles = TwoHandlesOverOneSurface::new();
    let outer_cell = PendingStagedWriteBackToSurface::new_unarmed();
    let holds_taken = std::cell::Cell::new(0);
    outer_cell
        .arm(CPU, || {
            holds_taken.set(holds_taken.get() + 1);
            handles.hold_for(handles.outer, CPU).map(Some)
        })
        .expect("the first arm stages");
    outer_cell
        .arm(CPU, || panic!("re-arming takes no second hold"))
        .expect("re-arming the same door is one edit");
    assert_eq!(holds_taken.get(), 1);
    assert!(handles.read_in_by(handles.inner, CPU).is_err());

    outer_cell.discard();

    assert_eq!(
        handles.read_in_by(handles.inner, CPU).unwrap(),
        ReadingTheFrameIn::CopyTheFrameIn
    );
}

#[test]
fn an_arm_whose_hold_is_refused_stays_unarmed() {
    let handles = TwoHandlesOverOneSurface::new();
    let _outer_edit = handles.hold_for(handles.outer, CPU).unwrap();
    let inner_cell = PendingStagedWriteBackToSurface::new_unarmed();

    assert!(
        inner_cell
            .arm(DEVICE, || handles.hold_for(handles.inner, DEVICE).map(Some))
            .is_err()
    );
    inner_cell
        .arm(CPU, || Ok(None))
        .expect("the refused arm left nothing armed to clash with");
}
