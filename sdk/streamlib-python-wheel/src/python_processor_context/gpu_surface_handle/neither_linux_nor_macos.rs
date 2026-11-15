// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use pyo3::prelude::*;

use super::PythonGpuSurfaceHandle;
use crate::python_gpu_surface_pixel_exchange::{
    DlpackExchangeShape, GpuSurfaceOwnedMemory, HOST_VISIBLE_DLPACK_DEVICE,
    dlpack_device_as_python_pair, host_visible_dlpack_capsule,
};

impl PythonGpuSurfaceHandle {
    pub(super) fn settle_this_lock_scopes_pending_writes(
        &self,
        _python: Python<'_>,
    ) -> PyResult<()> {
        Ok(())
    }

    /// No surface exchange exists here, so every handle serves its host side.
    pub(super) fn natural_side_is_device(
        &self,
        _python: Python<'_>,
        _owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> bool {
        false
    }

    /// The DLPack device of this surface's device side, once the handle decided that side is
    /// the one it naturally serves.
    pub(super) fn dlpack_device_of_the_natural_device_side(
        &self,
        python: Python<'_>,
        owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> PyResult<(i32, i32)> {
        let _ = (python, owned_memory);
        Ok(dlpack_device_as_python_pair(HOST_VISIBLE_DLPACK_DEVICE))
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
        host_visible_dlpack_capsule(python, owned_memory, exchange_shape, read_only)
    }
}
