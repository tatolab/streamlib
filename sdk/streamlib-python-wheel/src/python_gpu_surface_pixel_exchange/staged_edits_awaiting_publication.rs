// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The pool slots whose export staging holds an edit that a surface handle
//! staged and has not yet published or discarded.
//!
//! A helper keeps one staging per pool slot and door, shared by every handle
//! over that slot, so reading a frame into it wipes whatever edit it holds.
//! While an edit is outstanding, the handle that staged it keeps that edit
//! instead of reading the frame in again, and any other read-in over the slot
//! is refused by name.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::PyResult;
use pyo3::exceptions::PyRuntimeError;

use super::pending_staged_write_back_to_surface::StagedWriteBackSource;

/// Which surface handle staged an edit: the address of the memory it and
/// every capsule minted from it share, stable while any of them lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct SurfaceHandleIdentity(usize);

impl SurfaceHandleIdentity {
    pub(crate) fn of<T>(shared_memory: &Arc<T>) -> Self {
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

/// The pool slots of one helper whose staging holds an unpublished edit.
#[derive(Default)]
pub(crate) struct StagedEditsAwaitingPublication {
    by_pool_slot: Mutex<HashMap<String, StagedEdit>>,
}

impl StagedEditsAwaitingPublication {
    /// Hold `pool_slot_key` for the edit `staged_by` stages through
    /// `staged_through`, until the returned hold drops. `frame_read_in` says
    /// whether the staging already holds the frame, as the device door's
    /// does by the time it stages; the CPU door stages first and reads in
    /// after. A slot another handle already holds stays that handle's.
    pub(crate) fn hold_until_the_edit_settles(
        self: &Arc<Self>,
        pool_slot_key: &str,
        staged_by: SurfaceHandleIdentity,
        staged_through: StagedWriteBackSource,
        frame_read_in: bool,
    ) -> StagedEditAwaitingPublication {
        self.by_pool_slot
            .lock()
            .entry(pool_slot_key.to_string())
            .or_insert(StagedEdit {
                staged_by,
                staged_through,
                frame_read_in,
            });
        StagedEditAwaitingPublication {
            staged_edits: Arc::clone(self),
            pool_slot_key: pool_slot_key.to_string(),
            staged_by,
        }
    }

    /// What reading the frame of `surface_id` into its `door` staging does
    /// for `reader`, or the refusal when another handle, or another door,
    /// has an unpublished edit staged over the same pool slot.
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
        if staged_edit.staged_by != reader || staged_edit.staged_through != door {
            return Err(PyRuntimeError::new_err(format!(
                "surface {surface_id:?} has an edit staged through {} that an enclosing scope has \
                 not published yet; reading the frame in here would overwrite that edit with the \
                 pixels it replaced. Read and write through the enclosing scope's view, or open \
                 this one after that scope closes",
                staged_edit.staged_through.door_in_a_refusal(),
            )));
        }
        if staged_edit.frame_read_in {
            return Ok(ReadingTheFrameIn::KeepTheStagedEdit);
        }
        staged_edit.frame_read_in = true;
        Ok(ReadingTheFrameIn::CopyTheFrameIn)
    }
}

/// One pool slot held for one handle's unpublished edit; dropping it, when
/// the edit is published or discarded or the handle goes, frees the slot.
pub(crate) struct StagedEditAwaitingPublication {
    staged_edits: Arc<StagedEditsAwaitingPublication>,
    pool_slot_key: String,
    staged_by: SurfaceHandleIdentity,
}

impl Drop for StagedEditAwaitingPublication {
    fn drop(&mut self) {
        let mut by_pool_slot = self.staged_edits.by_pool_slot.lock();
        if let Entry::Occupied(held) = by_pool_slot.entry(std::mem::take(&mut self.pool_slot_key))
            && held.get().staged_by == self.staged_by
        {
            held.remove();
        }
    }
}
