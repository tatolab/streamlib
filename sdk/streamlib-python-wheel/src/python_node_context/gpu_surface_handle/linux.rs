// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use super::PythonGpuSurfaceHandle;
use crate::python_gpu_surface_pixel_exchange::{
    DlpackExchangeShape, GpuSurfaceOwnedMemory, StagedWriteBackSource, device_dlpack_capsule,
    device_export_available, dlpack_device_as_python_pair, imported_device_for,
    prepare_device_export, publish_tensor_storage_buffer_device_writes,
    read_the_frame_into_its_cpu_staging,
};
use crate::python_helper_process_pixel_exchange::{
    GpuSurfaceGeometry, HelperAcquiredTexture, HelperCheckedOutSurface, PixelSurfaceGeometry,
};

impl PythonGpuSurfaceHandle {
    /// Open the staged CPU door over this frame, once per lock scope.
    ///
    /// A surface the CPU can already address does nothing — its own
    /// coherent mapping *is* the door. Everything else checks the
    /// engine's host-visible staging out, maps it, and reads this frame's
    /// pixels in; a write lock over a frame that takes an edit also arms
    /// the publish, so the block edge has something to settle.
    ///
    /// Called from the host-side accessors rather than from `lock()`
    /// itself: `lock()` also gates the device side, and reading a frame
    /// into a host staging nobody asked for would cost the device path a
    /// copy per frame.
    pub(super) fn open_the_cpu_door_over_this_frame(
        &self,
        python: Python<'_>,
        owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> PyResult<()> {
        if !owned_memory.cpu_reach_goes_through_the_export_staging() {
            return Ok(());
        }
        if self
            .cpu_staging_holds_this_locks_frame
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(());
        }
        let opened = (|| -> PyResult<()> {
            if self.cpu_access.is_read_only() {
                read_the_frame_into_its_cpu_staging(python, owned_memory)?;
                return Ok(());
            }
            // Refused before the copy, not after: a scope that already
            // staged an edit through the other door cannot take this one,
            // and finding that out is not worth a round trip and a frame
            // copy first. Nothing is discarded on this path — the arm that
            // refused belongs to the other door.
            owned_memory
                .pending_staged_write_back()
                .arm(StagedWriteBackSource::CpuReadbackStaging)?;
            // Past the arm it is this scope's to settle, so every way out
            // from here drops it: a publish over a staging this scope never
            // filled would copy some earlier frame over the surface.
            let staged = read_the_frame_into_its_cpu_staging(python, owned_memory)
                .inspect_err(|_| owned_memory.pending_staged_write_back().discard())?;
            if !staged.writable {
                // Refused rather than downgraded: nothing here can make the
                // capsule read-only — `__dlpack__` derives that from the
                // lock, which said write — so going on would hand out a
                // writable array whose stores publish nowhere and vanish
                // without an error. The plan's own answer for a texture
                // that cannot take the copy is to refuse the door by name.
                owned_memory.pending_staged_write_back().discard();
                return Err(PyRuntimeError::new_err(format!(
                    "surface {:?} cannot take a write-back, so a write lock over it would hand \
                     out an array whose edits reach no other holder: it is a pooled frame its \
                     producer still owns, or a registered texture without the transfer usage to \
                     take a copy in. Lock it read-only to read these pixels",
                    owned_memory.surface_id_for_a_refusal(),
                )));
            }
            Ok(())
        })();
        if opened.is_err() {
            // The read-in is what makes a later publish legal, so a door
            // that failed to open must not look open to the next accessor.
            self.cpu_staging_holds_this_locks_frame
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        opened
    }

    /// Settle what this lock scope left pending, before its gate opens.
    ///
    /// On Linux that publishes a staged edit through whichever staging holds
    /// it.
    ///
    /// A tensor surface has no lock scope; its settle orders the CUDA writes
    /// a writable capsule took ahead of every other holder's read.
    pub(super) fn settle_this_lock_scopes_pending_writes(
        &self,
        python: Python<'_>,
    ) -> PyResult<()> {
        if self
            .a_writable_tensor_capsule_went_out
            .load(std::sync::atomic::Ordering::SeqCst)
            && let Some(owned_memory) = self.owned_memory.lock().clone()
        {
            return publish_tensor_storage_buffer_device_writes(python, &owned_memory);
        }
        self.publish_pending_staged_write(python)
    }

    pub(in crate::python_node_context) fn from_helper_acquired_texture(
        acquired: HelperAcquiredTexture,
    ) -> Self {
        let surface_id = acquired.surface_id.clone();
        let pixel_geometry = GpuSurfaceGeometry::Pixels(PixelSurfaceGeometry {
            width: acquired.width,
            height: acquired.height,
            format_wire_name: acquired.format.wire_name(),
        });
        Self::new(
            Some(surface_id.clone()),
            pixel_geometry,
            GpuSurfaceOwnedMemory::new(
                HelperCheckedOutSurface::AcquiredDeviceTexture(acquired),
                Some(surface_id),
            ),
        )
    }

    /// Decide — once per handle — whether the no-preference DLPack side
    /// is the device. Detached work: the first call may allocate staging
    /// and import into CUDA. A handle that answered kDLCUDA here never
    /// silently downgrades later; a refill failure after this raises.
    pub(super) fn natural_side_is_device(
        &self,
        python: Python<'_>,
        owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> bool {
        *self.natural_dlpack_side_is_device.get_or_init(|| {
            device_export_available(owned_memory)
                && imported_device_for(python, owned_memory).is_ok()
        })
    }

    /// Publish a pending staged write, once, through whichever staging
    /// holds the edit. Shared by `unlock` and `close` so the
    /// context-manager spelling cannot silently drop an edit; a handle
    /// already closed has nothing to publish into and discards instead.
    fn publish_pending_staged_write(&self, python: Python<'_>) -> PyResult<()> {
        // Bound before the match, so the guard drops here: the publish
        // crosses to the parent — the same mutex-across-the-GIL hazard
        // `release_owned_engine_value` documents.
        let owned_memory = self.owned_memory.lock().clone();
        match owned_memory {
            Some(owned_memory) => owned_memory
                .pending_staged_write_back()
                .publish_if_armed(python, &owned_memory),
            // A handle already closed dropped its share of the surface, so
            // there is nothing here to publish into — and nothing to
            // discard either: the cell went with the surface.
            None => Ok(()),
        }
    }

    /// The DLPack device of this surface's device side, once the handle decided that side is
    /// the one it naturally serves.
    pub(super) fn dlpack_device_of_the_natural_device_side(
        &self,
        python: Python<'_>,
        owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> PyResult<(i32, i32)> {
        Ok(dlpack_device_as_python_pair(imported_device_for(
            python,
            owned_memory,
        )?))
    }

    /// The DLPack capsule over this surface's device side, for a consumer that did not ask for
    /// the host.
    pub(super) fn dlpack_capsule_on_the_device_side<'py>(
        &self,
        python: Python<'py>,
        owned_memory: &Arc<GpuSurfaceOwnedMemory>,
        exchange_shape: DlpackExchangeShape,
        read_only: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        // The refill is a GPU submit plus a bounded wait, and on the
        // first call the staging allocation and CUDA import too. It
        // detaches around the blocking work itself — a helper's arm
        // has to stay attached to reach the parent at all.
        let prepared = prepare_device_export(python, owned_memory)?;
        let writable_export = !read_only && prepared.writable;
        // Armed before the capsule is minted: a scope that already
        // staged an edit through the CPU door is refused here rather
        // than handed a second writable view over other memory.
        if writable_export {
            owned_memory
                .pending_staged_write_back()
                .arm(StagedWriteBackSource::DeviceExportStaging)?;
        }
        let capsule =
            device_dlpack_capsule(python, owned_memory, prepared, exchange_shape, read_only)?;
        Ok(capsule)
    }

    /// Drop the edit a scope staged when the scope ends under a propagating
    /// exception, so `close` has nothing to publish.
    pub(super) fn discard_the_staged_write_a_propagating_exception_left(&self) {
        if let Some(owned_memory) = self.owned_memory.lock().clone() {
            owned_memory.pending_staged_write_back().discard();
        }
    }

    /// A fresh scope: whatever the staging holds is a previous one's read-in,
    /// and this scope owes its own.
    pub(super) fn close_the_previous_lock_scope_before_a_new_one(
        &self,
        _python: Python<'_>,
    ) -> PyResult<()> {
        self.cpu_staging_holds_this_locks_frame
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// The DLPack device a tensor storage buffer's capsule lives on: the CUDA
/// device its memory is imported onto, making the import on first ask.
pub(super) fn tensor_storage_buffer_dlpack_device(
    python: Python<'_>,
    storage_buffer: &crate::python_helper_process_pixel_exchange::HelperCheckedOutStorageBuffer,
) -> PyResult<(i32, i32)> {
    let cuda_import = python
        .detach(|| storage_buffer.cuda_import())
        .map_err(super::super::gpu_operation_error)?;
    Ok(dlpack_device_as_python_pair(cuda_import.dlpack_device()))
}
