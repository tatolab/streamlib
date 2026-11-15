// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use pyo3::prelude::*;

use super::PythonGpuSurfaceHandle;
use crate::python_gpu_surface_pixel_exchange::{
    DlpackExchangeShape, GpuSurfaceOwnedMemory, METAL_DLPACK_DEVICE, dlpack_device_as_python_pair,
    metal_dlpack_capsule,
};
use crate::python_metal_framework_queue_synchronization::drain_torch_mps_queue_if_imported;

impl PythonGpuSurfaceHandle {
    /// Open the CPU door over this frame, once per lock scope: take the
    /// IOSurface's lock with the intent `lock()` declared — what keeps the
    /// host view coherent on a discrete-GPU Mac.
    ///
    /// Taken here rather than in `lock()`, so a scope that reaches the pixels
    /// only through a Metal capsule never holds a CPU lock over GPU work.
    pub(super) fn open_the_cpu_door_over_this_frame(
        &self,
        _python: Python<'_>,
        owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> PyResult<()> {
        owned_memory.lock_the_iosurface_for_cpu_access_once(self.cpu_access.is_read_only())
    }

    /// Settle what this lock scope left pending, before its gate opens.
    ///
    /// On macOS it retires the stores a writable Metal capsule took, then
    /// lets the IOSurface's CPU lock go; both run whatever the other
    /// answered, and the first failure is returned with the second logged.
    ///
    /// A tensor surface has no lock scope; its settle orders the torch-MPS
    /// writes a writable capsule took ahead of every other holder's read.
    pub(super) fn settle_this_lock_scopes_pending_writes(
        &self,
        python: Python<'_>,
    ) -> PyResult<()> {
        if self
            .a_writable_tensor_capsule_went_out
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return self.order_tensor_device_writes_ahead_of_a_kernel_read(python);
        }
        let metal_writes_retired = if self
            .a_writable_metal_capsule_went_out_this_lock_scope
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            drain_torch_mps_queue_if_imported(python)
        } else {
            Ok(())
        };
        // Bound before the match, so the guard drops before the unlock.
        let owned_memory = self.owned_memory.lock().clone();
        let iosurface_cpu_lock_released = match owned_memory {
            Some(owned_memory) => owned_memory.unlock_the_iosurface_after_cpu_access(),
            None => Ok(()),
        };
        match (metal_writes_retired, iosurface_cpu_lock_released) {
            (Err(retire_failure), Err(unlock_failure)) => {
                tracing::warn!(
                    "releasing surface {:?}'s IOSurface CPU lock also failed: {unlock_failure}",
                    self.minted_surface_id
                );
                Err(retire_failure)
            }
            (metal_writes_retired, iosurface_cpu_lock_released) => {
                metal_writes_retired.and(iosurface_cpu_lock_released)
            }
        }
    }

    /// Decide — once per handle — whether the no-preference DLPack side is
    /// Metal: it is wherever the helper's device exports a no-copy
    /// `MTLBuffer` over the surface's IOSurface.
    pub(super) fn natural_side_is_device(
        &self,
        _python: Python<'_>,
        owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> bool {
        *self
            .natural_dlpack_side_is_device
            .get_or_init(|| owned_memory.metal_buffer_over_the_iosurface_pages().is_ok())
    }

    /// The DLPack device of this surface's device side, once the handle decided that side is
    /// the one it naturally serves.
    pub(super) fn dlpack_device_of_the_natural_device_side(
        &self,
        python: Python<'_>,
        owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> PyResult<(i32, i32)> {
        let _ = (python, owned_memory);
        Ok(dlpack_device_as_python_pair(METAL_DLPACK_DEVICE))
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
        // A write lands in the surface itself once the framework's queue
        // retires it, so a writable capsule is what obliges the unlock
        // to drain that queue.
        if !read_only {
            self.a_writable_metal_capsule_went_out_this_lock_scope
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        metal_dlpack_capsule(python, owned_memory, exchange_shape, read_only)
    }
}

/// The DLPack device a tensor storage buffer's capsule lives on: `kDLMetal`.
pub(super) fn tensor_storage_buffer_dlpack_device(
    _python: Python<'_>,
    _storage_buffer: &crate::python_helper_process_pixel_exchange::HelperCheckedOutStorageBuffer,
) -> PyResult<(i32, i32)> {
    Ok(dlpack_device_as_python_pair(METAL_DLPACK_DEVICE))
}
