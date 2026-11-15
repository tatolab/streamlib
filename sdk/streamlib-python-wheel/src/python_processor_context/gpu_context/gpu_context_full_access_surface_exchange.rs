// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The surfaces `GpuContextFullAccess` exports to, and imports from, another
//! process or API, each with its arm for the platforms that cannot.

#[cfg(target_os = "linux")]
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::python_helper_process_pixel_exchange::HelperCheckedOutSurface;
#[cfg(target_os = "linux")]
use crate::python_helper_process_pixel_exchange::foreign_dma_buf_plane_layout;
#[cfg(not(target_os = "linux"))]
use crate::python_processor_context::fd_shaped_raw_handle_is_linux_only_error;
#[cfg(target_os = "linux")]
use crate::python_processor_context::format_vocabulary::parse_pixel_format_name;
use crate::python_processor_context::gpu_surface_check_out_lease::{
    PythonIOSurfaceMachPortExport, PythonOpaqueFdAllocationExport,
};
use crate::python_processor_context::gpu_surface_handle::PythonGpuSurfaceHandle;
#[cfg(target_os = "linux")]
use crate::python_processor_context::gpu_unreachable_from_a_helper_process_error;
#[cfg(not(target_os = "macos"))]
use crate::python_processor_context::iosurface_raw_handle_is_macos_only_error;

use super::PythonGpuContextFullAccess;

#[pymethods]
impl PythonGpuContextFullAccess {
    /// Export a DMA-BUF file descriptor for `surface`, for native code that
    /// speaks DMA-BUF — EGL, a V4L2 output device, another process.
    ///
    /// Returns `(fd, byte_size)`. **The caller owns the fd** and must close
    /// it, or hand it to something that takes ownership.
    ///
    /// Answered without leaving this process: the fds arrived here over
    /// SCM_RIGHTS when the surface was checked out, and they are the same
    /// ones a host-side export would mint.
    #[cfg(target_os = "linux")]
    #[expect(
        clippy::unused_self,
        reason = "the surface carries the fds; the capability is the door"
    )]
    fn export_dma_buf(
        &self,
        python: Python<'_>,
        surface: &PythonGpuSurfaceHandle,
    ) -> PyResult<(i32, u64)> {
        let owned_memory = surface.owned_memory()?;
        python.detach(|| owned_memory.export_dma_buf())
    }

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

    /// Export the OPAQUE_FD memory handle for `surface` — a texture or a
    /// pixel buffer — for native code that runs its own Vulkan or CUDA
    /// external-memory import against the allocation.
    ///
    /// Returns a [`PythonOpaqueFdAllocationExport`]. **The caller owns the
    /// fd** — a successful foreign import adopts it; close it after a
    /// failed one. Consume a texture's as an image: a linear mapping over
    /// OPTIMAL-tiled memory yields block-linear bytes, never pixels.
    ///
    /// Answered without leaving this process: the fd arrived here over
    /// SCM_RIGHTS when the surface was checked out.
    #[cfg(target_os = "linux")]
    #[expect(
        clippy::unused_self,
        reason = "the surface carries the fds; the capability is the door"
    )]
    fn export_opaque_fd(
        &self,
        python: Python<'_>,
        surface: &PythonGpuSurfaceHandle,
    ) -> PyResult<PythonOpaqueFdAllocationExport> {
        let owned_memory = surface.owned_memory()?;
        Ok(python.detach(|| owned_memory.export_opaque_fd())?.into())
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
    #[cfg(target_os = "macos")]
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

    /// Import a foreign DMA-BUF file descriptor as a surface this graph can
    /// resolve. The caller keeps ownership of `fd` — the kernel dups it on
    /// the SCM_RIGHTS crossing — and may close it once this returns.
    ///
    /// `plane_offsets` and `plane_strides` state where each plane of `format`
    /// sits inside the `byte_size`-byte allocation; without them one plane's
    /// pitch is `byte_size / height` and several planes sit tightly packed.
    #[cfg(target_os = "linux")]
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
    #[expect(
        clippy::too_many_arguments,
        reason = "each is one stated property of the foreign allocation"
    )]
    fn import_dma_buf(
        &self,
        python: Python<'_>,
        fd: i32,
        width: u32,
        height: u32,
        format: &str,
        byte_size: Option<u64>,
        plane_offsets: Option<Vec<u64>>,
        plane_strides: Option<Vec<u64>>,
        drm_format_modifier: u64,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        let pixel_format = parse_pixel_format_name(format)?;
        let (allocation_byte_size, planes) = foreign_dma_buf_plane_layout(
            pixel_format,
            width,
            height,
            byte_size,
            plane_offsets,
            plane_strides,
            drm_format_modifier,
        )
        .map_err(PyValueError::new_err)?;
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let checked_out = exchange_client.import_foreign_dma_buf(
                python,
                fd,
                width,
                height,
                pixel_format,
                allocation_byte_size,
                &planes,
            )?;
            return Ok(PythonGpuSurfaceHandle::from_helper_checked_out_surface(
                HelperCheckedOutSurface::PixelBuffer(checked_out),
            ));
        }
        Err(gpu_unreachable_from_a_helper_process_error())
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
