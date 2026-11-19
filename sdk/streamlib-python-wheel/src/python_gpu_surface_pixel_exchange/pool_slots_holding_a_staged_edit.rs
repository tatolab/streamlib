// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The pool slots whose export staging holds an edit that a surface handle
//! staged and has not yet published or discarded.
//!
//! A helper keeps one staging per pool slot and door, shared by every handle
//! over that slot and by native code, so reading a frame into a staging wipes
//! whatever edit it holds, and a second edit staged over the same slot is
//! overwritten by whichever publishes last. While an edit is outstanding, the
//! handle that staged it keeps that edit instead of reading the frame in
//! again; every other read-in into that staging, and every other edit over
//! the slot, is refused by name.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::PyResult;
use pyo3::exceptions::PyRuntimeError;

use super::gpu_surface_owned_memory::GpuSurfaceOwnedMemory;
use super::pending_staged_write_back_to_surface::StagedWriteBackSource;

/// Which surface handle staged an edit: the address of the memory it and
/// every capsule minted from it share, stable while any of them lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct SurfaceHandleIdentity(usize);

impl SurfaceHandleIdentity {
    pub(crate) fn of(shared_memory: &Arc<GpuSurfaceOwnedMemory>) -> Self {
        Self::of_the_memory_at(shared_memory)
    }

    #[cfg(test)]
    pub(super) fn of_a_stand_in<T>(stand_in_memory: &Arc<T>) -> Self {
        Self::of_the_memory_at(stand_in_memory)
    }

    fn of_the_memory_at<T>(shared_memory: &Arc<T>) -> Self {
        Self(Arc::as_ptr(shared_memory) as usize)
    }
}

/// What a read-in over a pool slot does with its staging.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReadingTheFrameIn {
    /// Copy the frame's current pixels into the staging.
    CopyTheFrameIn,
    /// The staging already holds this handle's unpublished edit over the
    /// frame, which a copy would wipe; hand it out as it is.
    KeepTheStagedEdit,
}

struct StagedEdit {
    staged_by: SurfaceHandleIdentity,
    staged_through: StagedWriteBackSource,
    frame_read_in: bool,
}

impl StagedEdit {
    fn refusal(&self, surface_id: &str, what_was_refused: &str) -> String {
        format!(
            "surface {surface_id:?} has an edit staged through {} that another scope has not \
             published yet; {what_was_refused} would overwrite that edit. Read and write through \
             that scope's view, or do this after that scope closes",
            self.staged_through.door_in_a_refusal(),
        )
    }
}

/// The pool slots of one helper whose staging holds an unpublished edit.
#[derive(Default)]
pub(crate) struct PoolSlotsHoldingAStagedEdit {
    by_pool_slot: Mutex<HashMap<String, StagedEdit>>,
}

impl PoolSlotsHoldingAStagedEdit {
    /// Hold the pool slot of `surface_id` for the edit `staged_by` stages
    /// through `staged_through`, until the returned hold drops, or the
    /// refusal when another handle's edit already holds the slot.
    pub(crate) fn hold_until_the_edit_settles(
        self: &Arc<Self>,
        surface_id: &str,
        staged_by: SurfaceHandleIdentity,
        staged_through: StagedWriteBackSource,
    ) -> PyResult<PoolSlotHeldForAStagedEdit> {
        let pool_slot_key = streamlib::sdk::rhi::pool_slot_key_of_surface_id(surface_id).to_owned();
        match self.by_pool_slot.lock().entry(pool_slot_key.clone()) {
            Entry::Occupied(held) if held.get().staged_by != staged_by => {
                return Err(PyRuntimeError::new_err(held.get().refusal(
                    surface_id,
                    &format!(
                        "staging a second edit through {}",
                        staged_through.door_in_a_refusal()
                    ),
                )));
            }
            Entry::Occupied(_) => {}
            Entry::Vacant(vacant) => {
                vacant.insert(StagedEdit {
                    staged_by,
                    staged_through,
                    frame_read_in: staged_through.reads_the_frame_in_before_it_stages(),
                });
            }
        }
        Ok(PoolSlotHeldForAStagedEdit {
            pool_slots: Arc::clone(self),
            pool_slot_key,
            staged_by,
        })
    }

    /// What reading the frame of `surface_id` into its `door` staging does
    /// for `reader`, or the refusal when another handle has an unpublished
    /// edit in that staging.
    pub(crate) fn before_reading_the_frame_in(
        &self,
        surface_id: &str,
        reader: SurfaceHandleIdentity,
        door: StagedWriteBackSource,
    ) -> PyResult<ReadingTheFrameIn> {
        let pool_slot_key = streamlib::sdk::rhi::pool_slot_key_of_surface_id(surface_id);
        let mut by_pool_slot = self.by_pool_slot.lock();
        let Some(staged_edit) = by_pool_slot.get_mut(pool_slot_key) else {
            return Ok(ReadingTheFrameIn::CopyTheFrameIn);
        };
        // The other door's staging is other memory: the read-in leaves the
        // edit where it is.
        if staged_edit.staged_through != door {
            return Ok(ReadingTheFrameIn::CopyTheFrameIn);
        }
        if staged_edit.staged_by != reader {
            return Err(PyRuntimeError::new_err(staged_edit.refusal(
                surface_id,
                &format!("reading the frame into {}", door.door_in_a_refusal()),
            )));
        }
        if staged_edit.frame_read_in {
            return Ok(ReadingTheFrameIn::KeepTheStagedEdit);
        }
        staged_edit.frame_read_in = true;
        Ok(ReadingTheFrameIn::CopyTheFrameIn)
    }

    /// The refusal when native code reading `surface_id` through the CPU
    /// staging would wipe an edit a handle staged there.
    pub(crate) fn before_native_code_reads_the_frame_in(
        &self,
        surface_id: &str,
    ) -> Result<(), String> {
        let pool_slot_key = streamlib::sdk::rhi::pool_slot_key_of_surface_id(surface_id);
        match self.by_pool_slot.lock().get(pool_slot_key) {
            Some(staged_edit)
                if staged_edit.staged_through == StagedWriteBackSource::CpuReadbackStaging =>
            {
                Err(staged_edit.refusal(surface_id, "reading it into native memory"))
            }
            _ => Ok(()),
        }
    }

    /// The refusal when native code writing over `surface_id` would collide
    /// with an edit a handle staged over the same slot: the CPU staging it
    /// writes through, or a publish that would later overwrite its pixels.
    pub(crate) fn before_native_code_writes_over_the_surface(
        &self,
        surface_id: &str,
    ) -> Result<(), String> {
        let pool_slot_key = streamlib::sdk::rhi::pool_slot_key_of_surface_id(surface_id);
        match self.by_pool_slot.lock().get(pool_slot_key) {
            Some(staged_edit) => {
                Err(staged_edit.refusal(surface_id, "writing native memory over it"))
            }
            None => Ok(()),
        }
    }
}

/// One pool slot held for one handle's unpublished edit; dropping it, when
/// the edit is published or discarded or the handle goes, frees the slot.
#[must_use = "dropping the hold frees the pool slot"]
pub(crate) struct PoolSlotHeldForAStagedEdit {
    pool_slots: Arc<PoolSlotsHoldingAStagedEdit>,
    pool_slot_key: String,
    staged_by: SurfaceHandleIdentity,
}

impl Drop for PoolSlotHeldForAStagedEdit {
    fn drop(&mut self) {
        let mut by_pool_slot = self.pool_slots.by_pool_slot.lock();
        if let Entry::Occupied(held) = by_pool_slot.entry(std::mem::take(&mut self.pool_slot_key))
            && held.get().staged_by == self.staged_by
        {
            held.remove();
        }
    }
}
