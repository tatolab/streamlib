// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The surface exports and imports `GpuContextFullAccess` refuses, by name, on
//! a platform that cannot carry them.

use pyo3::prelude::*;

#[cfg(not(target_os = "linux"))]
use crate::python_processor_context::fd_shaped_raw_handle_is_linux_only_error;
#[cfg(not(target_os = "macos"))]
use crate::python_processor_context::gpu_surface_check_out_lease::PythonIOSurfaceMachPortExport;
#[cfg(not(target_os = "linux"))]
use crate::python_processor_context::gpu_surface_check_out_lease::PythonOpaqueFdAllocationExport;
use crate::python_processor_context::gpu_surface_handle::PythonGpuSurfaceHandle;
#[cfg(not(target_os = "macos"))]
use crate::python_processor_context::iosurface_raw_handle_is_macos_only_error;

use super::PythonGpuContextFullAccess;

#[pymethods]
impl PythonGpuContextFullAccess {
    /// Refuses by name: DMA-BUF is a Linux handle.
    #[cfg(not(target_os = "linux"))]
    #[expect(
        clippy::unused_self,
        reason = "the refusal is this capability's whole answer off Linux"
    )]
    #[expect(
        unused_variables,
        reason = "the Python-visible parameter name is the API; stubtest compares it"
    )]
    fn export_dma_buf(&self, surface: &PythonGpuSurfaceHandle) -> PyResult<(i32, u64)> {
        Err(fd_shaped_raw_handle_is_linux_only_error("export_dma_buf"))
    }

    /// Refuses by name: OPAQUE_FD is a Linux handle.
    #[cfg(not(target_os = "linux"))]
    #[expect(
        clippy::unused_self,
        reason = "the refusal is this capability's whole answer off Linux"
    )]
    #[expect(
        unused_variables,
        reason = "the Python-visible parameter name is the API; stubtest compares it"
    )]
    fn export_opaque_fd(
        &self,
        surface: &PythonGpuSurfaceHandle,
    ) -> PyResult<PythonOpaqueFdAllocationExport> {
        Err(fd_shaped_raw_handle_is_linux_only_error("export_opaque_fd"))
    }

    /// Refuses by name: an IOSurface is a macOS handle.
    #[cfg(not(target_os = "macos"))]
    #[expect(
        clippy::unused_self,
        reason = "the refusal is this capability's whole answer off macOS"
    )]
    #[expect(
        unused_variables,
        reason = "the Python-visible parameter name is the API; stubtest compares it"
    )]
    fn export_iosurface(
        &self,
        surface: &PythonGpuSurfaceHandle,
    ) -> PyResult<PythonIOSurfaceMachPortExport> {
        Err(iosurface_raw_handle_is_macos_only_error())
    }

    /// Refuses by name: DMA-BUF is a Linux handle.
    #[cfg(not(target_os = "linux"))]
    #[expect(
        clippy::unused_self,
        reason = "the refusal is this capability's whole answer off Linux"
    )]
    #[expect(
        unused_variables,
        reason = "the Python-visible parameter names are the API; stubtest compares them"
    )]
    #[expect(
        clippy::too_many_arguments,
        reason = "the Python-visible parameters are the API; stubtest compares them"
    )]
    #[pyo3(signature = (
        fd,
        width,
        height,
        format = "bgra",
        byte_size = None,
        plane_offsets = None,
        plane_strides = None,
        drm_format_modifier = 0,
    ))]
    fn import_dma_buf(
        &self,
        fd: i32,
        width: u32,
        height: u32,
        format: &str,
        byte_size: Option<u64>,
        plane_offsets: Option<Vec<u64>>,
        plane_strides: Option<Vec<u64>>,
        drm_format_modifier: u64,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        Err(fd_shaped_raw_handle_is_linux_only_error("import_dma_buf"))
    }
}
