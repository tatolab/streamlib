// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The one staged edit outstanding against a surface, whichever door staged
//! it.

use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use super::gpu_surface_owned_memory::GpuSurfaceOwnedMemory;
use super::linux::{
    publish_cpu_staged_write_back_to_surface, publish_device_write_back_to_surface,
};
use super::pool_slots_holding_a_staged_edit::PoolSlotHeldForAStagedEdit;

/// Which staging holds the edit a pending write-back publishes.
///
/// The two doors stage in different memory and publish with different
/// copies; the armed state carries which, so one protocol settles both.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StagedWriteBackSource {
    /// A writable device capsule went out over the device-local export
    /// staging.
    DeviceExportStaging,
    /// A writable numpy view went out over the host-visible readback
    /// staging the CPU door mapped.
    CpuReadbackStaging,
}

impl StagedWriteBackSource {
    /// How a refusal names this door to the author who opened it.
    pub(super) fn door_in_a_refusal(self) -> &'static str {
        match self {
            Self::DeviceExportStaging => "the device tensor",
            Self::CpuReadbackStaging => "the CPU array",
        }
    }

    /// Whether this door has read the frame into its staging by the time it
    /// stages an edit: the device door refills before it arms, the CPU door
    /// arms before its read-in.
    pub(super) fn reads_the_frame_in_before_it_stages(self) -> bool {
        match self {
            Self::DeviceExportStaging => true,
            Self::CpuReadbackStaging => false,
        }
    }
}

/// The armed-until-settled state a writable staged export leaves behind:
/// a consumer took a writable view, and the edit it may have made is
/// published at the scope's normal exit — or discarded on its exception
/// path. One protocol for every scope that hands out a staged view, so
/// the swap-once semantics (the first settler wins; unlock-then-close
/// publishes once) live in one place.
pub(crate) struct PendingStagedWriteBackToSurface {
    armed_over: Mutex<Option<ArmedStagedWriteBack>>,
}

/// The door an armed edit publishes through, and the hold that keeps every
/// other handle's read-in off its pool slot's staging until it settles.
struct ArmedStagedWriteBack {
    source: StagedWriteBackSource,
    _pool_slot_held: Option<PoolSlotHeldForAStagedEdit>,
}

impl PendingStagedWriteBackToSurface {
    pub(crate) fn new_unarmed() -> Self {
        Self {
            armed_over: Mutex::new(None),
        }
    }

    /// A writable view went out; the next normal scope exit publishes it
    /// back through `source`'s own copy.
    ///
    /// Re-arming the same source is the idempotent case — several capsules
    /// over one staging are one pending edit. A *second* source is refused
    /// by name: the two stagings were each filled from the surface before
    /// the other door's edit existed, so neither holds both edits, and
    /// there is no order in which publishing both does not overwrite one
    /// of them with pixels that predate it. Silently settling one is the
    /// answer this must never give — the lost edit surfaces as corruption
    /// nowhere near the write.
    ///
    /// `hold_the_pool_slot` runs once, when this arms from unarmed: what it
    /// answers holds the surface's pool slot until the edit settles, and its
    /// refusal leaves this unarmed.
    pub(crate) fn arm(
        &self,
        source: StagedWriteBackSource,
        hold_the_pool_slot: impl FnOnce() -> PyResult<Option<PoolSlotHeldForAStagedEdit>>,
    ) -> PyResult<()> {
        let mut armed_over = self.armed_over.lock();
        match armed_over.as_ref().map(|armed| armed.source) {
            Some(already_armed) if already_armed != source => {
                Err(PyRuntimeError::new_err(format!(
                    "this lock scope already staged an edit through {}; {} would stage a second \
                     edit in different memory, and publishing both would overwrite one with \
                     pixels taken before it. Close this scope and open the other door in its own",
                    already_armed.door_in_a_refusal(),
                    source.door_in_a_refusal(),
                )))
            }
            Some(_) => Ok(()),
            None => {
                *armed_over = Some(ArmedStagedWriteBack {
                    source,
                    _pool_slot_held: hold_the_pool_slot()?,
                });
                Ok(())
            }
        }
    }

    /// Drop the pending write instead of publishing it — the exception
    /// path's arm. A raise means the write did not finish, and
    /// publishing a half-written view hands downstream a torn frame that
    /// surfaces as corruption far from the raise.
    pub(crate) fn discard(&self) {
        *self.armed_over.lock() = None;
    }

    /// Publish the staged edit back into the surface if one is armed.
    /// Take-once: the first caller wins, so a scope that settles twice
    /// (unlock, then close) publishes once.
    pub(crate) fn publish_if_armed(
        &self,
        python: Python<'_>,
        owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> PyResult<()> {
        debug_assert!(std::ptr::eq(self, &owned_memory.pending_staged_write_back));
        // Taken before the publish, not held across it: the copy crosses
        // to the parent, and a lock held over that hop is the
        // mutex-across-the-GIL hazard.
        // The pool slot stays held until the publish returns, when the
        // taken arm drops.
        let Some(armed) = self.armed_over.lock().take() else {
            return Ok(());
        };
        match armed.source {
            StagedWriteBackSource::DeviceExportStaging => {
                publish_device_write_back_to_surface(python, owned_memory)
            }
            StagedWriteBackSource::CpuReadbackStaging => {
                publish_cpu_staged_write_back_to_surface(python, owned_memory)
            }
        }
    }
}
