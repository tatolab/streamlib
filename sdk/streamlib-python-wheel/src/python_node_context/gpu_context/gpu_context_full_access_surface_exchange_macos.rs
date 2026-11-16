// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The surface `GpuContextFullAccess` exports to another process or API on
//! macOS: an IOSurface.

use pyo3::prelude::*;

use crate::python_node_context::gpu_surface_check_out_lease::PythonIOSurfaceMachPortExport;
use crate::python_node_context::gpu_surface_handle::PythonGpuSurfaceHandle;

use super::PythonGpuContextFullAccess;

#[pymethods]
impl PythonGpuContextFullAccess {
    /// Export a Mach send right to `surface`'s IOSurface, for native code
    /// that looks the surface up itself — Metal, CoreVideo, a Vulkan
    /// IOSurface import.
    ///
    /// Returns a [`PythonIOSurfaceMachPortExport`]. **The caller owns the
    /// send right** and deallocates it; while it is held the surface reads
    /// as in use, which pins its pool slot as a held fd does on Linux.
    ///
    /// Answered without leaving this process: the helper already holds the
    /// IOSurface from the surface's checkout and mints the right itself.
    #[expect(
        clippy::unused_self,
        reason = "the surface carries the IOSurface; the capability is the door"
    )]
    fn export_iosurface(
        &self,
        python: Python<'_>,
        surface: &PythonGpuSurfaceHandle,
    ) -> PyResult<PythonIOSurfaceMachPortExport> {
        let owned_memory = surface.owned_memory()?;
        Ok(python.detach(|| owned_memory.export_iosurface())?.into())
    }
}
