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
    /// No surface exchange exists here, so the host mapping is the only door
    /// and opening it does nothing.
    pub(super) fn open_the_cpu_door_over_this_frame(
        &self,
        _python: Python<'_>,
        _owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> PyResult<()> {
        Ok(())
    }

    /// No surface exchange exists here, so a lock scope leaves nothing pending.
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

    /// No surface here has a device side, so the host answers.
    pub(super) fn dlpack_device_of_the_natural_device_side(
        &self,
        _python: Python<'_>,
        _owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> PyResult<(i32, i32)> {
        Ok(dlpack_device_as_python_pair(HOST_VISIBLE_DLPACK_DEVICE))
    }

    /// No surface here has a device side, so the host's capsule answers.
    pub(super) fn dlpack_capsule_on_the_device_side<'py>(
        &self,
        python: Python<'py>,
        owned_memory: &Arc<GpuSurfaceOwnedMemory>,
        exchange_shape: DlpackExchangeShape,
        read_only: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        host_visible_dlpack_capsule(python, owned_memory, exchange_shape, read_only)
    }

    /// No staging exists here, so there is no staged edit to discard.
    pub(super) fn discard_the_staged_write_a_propagating_exception_left(&self) {}

    /// No surface exchange exists here, so a lock scope has no door to close.
    pub(super) fn close_the_previous_lock_scope_before_a_new_one(
        &self,
        _python: Python<'_>,
    ) -> PyResult<()> {
        Ok(())
    }
}
