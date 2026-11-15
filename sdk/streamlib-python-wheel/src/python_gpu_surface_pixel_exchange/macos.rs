// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The macOS arms of the pixel exchange: the IOSurface lock and the Metal
//! export of the surface's own pages.

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib_adapter_cuda::dlpack::{self, Device, DeviceType};

use crate::python_helper_process_pixel_exchange::HelperCheckedOutSurface;

use super::gpu_surface_owned_memory::GpuSurfaceOwnedMemory;
use super::pixel_exchange_tensor_layout::PixelExchangeTensorLayout;
use super::{DlpackExchangeShape, HostVisiblePixelPlaneView, dlpack_capsule_over};

impl GpuSurfaceOwnedMemory {
    /// Bracket CPU access with the surface's IOSurface lock, read-only or
    /// read-write, unless it is already held — what keeps the host view
    /// coherent on a discrete-GPU Mac, and ~0.6 µs on unified memory.
    pub(crate) fn lock_the_iosurface_for_cpu_access_once(&self, read_only: bool) -> PyResult<()> {
        match &self.checked_out_surface {
            HelperCheckedOutSurface::PixelBuffer(pixel_surface) => {
                pixel_surface.lock_the_iosurface_for_cpu_access_once(read_only)
            }
            HelperCheckedOutSurface::Texture(texture_surface) => {
                texture_surface.lock_the_iosurface_for_cpu_access_once(read_only)
            }
            HelperCheckedOutSurface::StorageBuffer(_) => {
                Err(crate::python_helper_process_pixel_exchange::a_tensor_surface_is_not_a_pixel_surface())
            }
        }
    }

    /// Release the IOSurface lock CPU access took, if it holds one.
    pub(crate) fn unlock_the_iosurface_after_cpu_access(&self) -> PyResult<()> {
        match &self.checked_out_surface {
            HelperCheckedOutSurface::PixelBuffer(pixel_surface) => {
                pixel_surface.unlock_the_iosurface_after_cpu_access()
            }
            HelperCheckedOutSurface::Texture(texture_surface) => {
                texture_surface.unlock_the_iosurface_after_cpu_access()
            }
            HelperCheckedOutSurface::StorageBuffer(_) => Ok(()),
        }
    }

    /// A fresh send right to this surface's IOSurface plus its
    /// allocation-stable shape — the macOS raw handle, minted from the
    /// surface this helper already holds.
    pub(crate) fn export_iosurface(
        &self,
    ) -> PyResult<crate::python_helper_process_pixel_exchange::IOSurfaceMachPortExportDescription>
    {
        self.checked_out_surface.export_iosurface()
    }

    /// On macOS every surface's view is its IOSurface's own pages — a pixel
    /// buffer's through its import's mapping, a texture's through the
    /// surface's host mapping.
    pub(crate) fn host_visible_pixel_plane(&self) -> PyResult<HostVisiblePixelPlaneView> {
        match &self.checked_out_surface {
            HelperCheckedOutSurface::PixelBuffer(pixel_surface) => {
                Ok(pixel_surface.host_visible_pixel_plane_view())
            }
            HelperCheckedOutSurface::Texture(texture_surface) => {
                texture_surface.host_visible_pixel_plane_view()
            }
            HelperCheckedOutSurface::StorageBuffer(_) => {
                Err(crate::python_helper_process_pixel_exchange::a_tensor_surface_is_not_a_pixel_surface())
            }
        }
    }

    /// A no-copy `MTLBuffer` over the surface's IOSurface pages — the same
    /// bytes [`Self::host_visible_pixel_plane`] maps, as the GPU addresses
    /// them.
    pub(crate) fn metal_buffer_over_the_iosurface_pages(
        &self,
    ) -> PyResult<objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLBuffer>>>
    {
        match &self.checked_out_surface {
            HelperCheckedOutSurface::PixelBuffer(pixel_surface) => {
                pixel_surface.metal_buffer_over_the_iosurface_pages()
            }
            HelperCheckedOutSurface::Texture(texture_surface) => {
                texture_surface.metal_buffer_over_the_iosurface_pages()
            }
            HelperCheckedOutSurface::StorageBuffer(storage_buffer) => {
                storage_buffer.metal_buffer_over_the_iosurface_pages()
            }
        }
    }
}

/// The DLPack device a Metal capsule reports: `kDLMetal`, device 0 — the one
/// GPU an Apple Silicon Mac has.
pub(crate) const METAL_DLPACK_DEVICE: Device = Device {
    device_type: DeviceType::Metal,
    device_id: 0,
};

/// A retained `MTLBuffer` a capsule's owner keeps alive until its consumer
/// lets the tensor go.
struct MetalBufferHeldByACapsule(
    #[expect(
        dead_code,
        reason = "held for its retain; the capsule's data pointer is this buffer"
    )]
    objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLBuffer>>,
);

// SAFETY: an `MTLBuffer` is a thread-safe Metal resource, and the capsule
// owner only retains and releases it — which Objective-C's retain count
// makes safe from any thread.
unsafe impl Send for MetalBufferHeldByACapsule {}

/// Build a `kDLMetal` capsule over the surface's IOSurface — torch-MPS's and
/// MLX's zero-copy door.
///
/// DLPack's Metal `data` is the `id<MTLBuffer>` itself, never a host address;
/// the buffer is a no-copy view of the pages the host mapping reaches, so the
/// strides are the surface's own row pitch, padding included. The capsule
/// owns the surface share and a retain on the buffer, and the buffer is
/// MoltenVK's, so no consumer's allocator is ever handed memory to free.
pub(crate) fn metal_dlpack_capsule<'py>(
    python: Python<'py>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    exchange_shape: DlpackExchangeShape,
    read_only: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let plane_view = owned_memory.host_visible_pixel_plane()?;
    let layout = PixelExchangeTensorLayout::for_pixel_format(
        plane_view.format,
        plane_view.width,
        plane_view.height,
        plane_view.bytes_per_row,
    )?;
    metal_dlpack_capsule_over_the_iosurface_base(
        python,
        owned_memory,
        plane_view.base_address,
        layout,
        exchange_shape,
        read_only,
    )
}

/// Build a `kDLMetal` capsule straight over a tensor storage buffer's
/// IOSurface pages, in its declared shape — torch-MPS's and MLX's zero-copy
/// door to the same memory a kernel binding the tensor addresses. The capsule
/// holds the surface, and with it the checkout lease, so the slot is not
/// reused while the tensor lives.
pub(crate) fn tensor_storage_buffer_dlpack_capsule<'py>(
    python: Python<'py>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    exchange_shape: DlpackExchangeShape,
    read_only: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let storage_buffer = owned_memory
        .tensor_storage_buffer()
        .ok_or_else(|| PyRuntimeError::new_err("this surface is not a tensor storage buffer"))?;
    let layout =
        PixelExchangeTensorLayout::for_tensor_storage_buffer(&storage_buffer.tensor_layout)?;
    metal_dlpack_capsule_over_the_iosurface_base(
        python,
        owned_memory,
        storage_buffer.iosurface().base_address().as_ptr().cast(),
        layout,
        exchange_shape,
        read_only,
    )
}

/// Wrap the surface's no-copy `MTLBuffer` in a `kDLMetal` capsule of
/// `layout`, whose strides count from `iosurface_base_address`.
fn metal_dlpack_capsule_over_the_iosurface_base<'py>(
    python: Python<'py>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    iosurface_base_address: *const u8,
    layout: PixelExchangeTensorLayout,
    exchange_shape: DlpackExchangeShape,
    read_only: bool,
) -> PyResult<Bound<'py, PyAny>> {
    use objc2_metal::MTLBuffer as _;

    let metal_buffer = owned_memory.metal_buffer_over_the_iosurface_pages()?;
    // The layout's strides are counted from the IOSurface's base, so the
    // buffer must start exactly there; anything else would shift every element.
    if metal_buffer.contents().as_ptr().cast::<u8>().cast_const() != iosurface_base_address {
        return Err(PyRuntimeError::new_err(
            "the surface's Metal buffer does not start at its IOSurface's base address, so a \
             Metal tensor over it would not address the surface's memory",
        ));
    }
    let metal_buffer_address = objc2::rc::Retained::as_ptr(&metal_buffer) as u64;
    // The buffer aliases the pages the surface share keeps, so it drops first.
    let owner: dlpack::CapsuleOwner = Box::new((
        MetalBufferHeldByACapsule(metal_buffer),
        Arc::clone(owned_memory),
    ));
    dlpack_capsule_over(
        python,
        metal_buffer_address,
        layout,
        METAL_DLPACK_DEVICE,
        exchange_shape,
        read_only,
        owner,
    )
}

/// Order every Metal write to a tensor storage buffer ahead of any other
/// holder's read: torch-MPS's queue drain, since the capsule protocol carries
/// no stream to order against. An MLX write is retired by the `mx.eval` the
/// write contract names.
pub(crate) fn publish_tensor_storage_buffer_device_writes(
    python: Python<'_>,
    _owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<()> {
    crate::python_metal_framework_queue_synchronization::drain_torch_mps_queue_if_imported(python)
}
