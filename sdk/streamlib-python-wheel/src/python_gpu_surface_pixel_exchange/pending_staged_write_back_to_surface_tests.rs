// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::pending_staged_write_back_to_surface::PendingStagedWriteBackToSurface;
use super::*;

/// Two doors in one lock scope stage into two different buffers, each
/// filled from the surface before the other's edit existed. Publishing
/// whichever armed last would copy pixels that predate the first edit
/// back over the surface — losing it, silently, far from the write.
#[test]
fn a_second_staging_source_in_one_scope_is_refused_rather_than_replacing_the_first() {
    Python::initialize();
    let pending = PendingStagedWriteBackToSurface::new_unarmed();

    pending
        .arm(StagedWriteBackSource::DeviceExportStaging)
        .expect("the first source arms");
    pending
        .arm(StagedWriteBackSource::DeviceExportStaging)
        .expect("re-arming the same source is one pending edit, not two");

    let refusal = pending
        .arm(StagedWriteBackSource::CpuReadbackStaging)
        .expect_err("a second, different source must be refused")
        .to_string();
    assert!(
        refusal.contains("the device tensor") && refusal.contains("the CPU array"),
        "the refusal must name both doors so the author knows which two collided: {refusal:?}"
    );

    // The refusal must not have disturbed what was already armed: the
    // first door's edit is still the one that publishes.
    pending
        .arm(StagedWriteBackSource::DeviceExportStaging)
        .expect("the first source is still the armed one");
}

/// The cell belongs to the surface, not the handle or the scope.
///
/// `as_device_tensor()` mints a scope over the *same*
/// `GpuSurfaceOwnedMemory` the handle holds, so a CPU staged edit and
/// a device edit are two stagings over one surface — each filled
/// before the other's edit existed. While the two doors kept private
/// cells, neither refusal could see the other's edit and whichever
/// published last silently overwrote the first.
#[test]
fn a_device_scope_meets_the_cpu_doors_arm_because_the_cell_is_the_surfaces() {
    Python::initialize();
    // The cell a `GpuSurfaceOwnedMemory` now holds; a handle and every
    // device-tensor scope it mints reach this one instance, which is
    // what lets the second door's refusal see the first door's edit.
    let surfaces_pending_edit = PendingStagedWriteBackToSurface::new_unarmed();

    // What the handle's CPU door does under a write lock.
    surfaces_pending_edit
        .arm(StagedWriteBackSource::CpuReadbackStaging)
        .expect("the CPU door stages first");

    // What a device-tensor scope minted off that same handle then does.
    let refusal = surfaces_pending_edit
        .arm(StagedWriteBackSource::DeviceExportStaging)
        .expect_err("a device scope over a surface holding a CPU staged edit must be refused")
        .to_string();
    assert!(
        refusal.contains("the CPU array") && refusal.contains("the device tensor"),
        "the refusal must name both doors: {refusal:?}"
    );
}

/// Discarding frees the scope to take the other door — the refusal is
/// about two *live* pending edits, not a latch on the handle.
#[test]
fn discarding_releases_the_scope_to_stage_through_the_other_door() {
    Python::initialize();
    let pending = PendingStagedWriteBackToSurface::new_unarmed();

    pending
        .arm(StagedWriteBackSource::CpuReadbackStaging)
        .expect("the first source arms");
    pending.discard();
    pending
        .arm(StagedWriteBackSource::DeviceExportStaging)
        .expect("with nothing pending, the other door is free to stage");
}
