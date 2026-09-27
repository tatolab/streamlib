// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A tensor storage buffer as a macOS helper holds it: the engine's
//! byte-shaped IOSurface of a declared shape and dtype, its pages imported on
//! this helper's device, and the no-copy `MTLBuffer` over them that torch-MPS
//! and MLX read and write.

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::rhi::TensorStorageBufferLayout;
use streamlib_consumer_rhi::ConsumerVulkanBuffer;

use super::super::storage_buffer::tensor_layout_of_a_check_out;
use super::super::{
    HelperProcessGpuExchangeClient, HelperSurfaceCheckOutLeaseDebt, HelperSurfaceReleaseDebt,
    SurfaceShareTransferredHandle,
};

/// A checked-out tensor storage buffer. Fields drop in declaration order, so
/// the import lets go of the pages before the checkout lease frees the slot.
pub(crate) struct HelperCheckedOutStorageBuffer {
    pub(crate) surface_id: String,
    pub(crate) tensor_layout: TensorStorageBufferLayout,
    /// Whether this process may write the tensor: the acquirer may, a
    /// resolver may not.
    pub(crate) writable: bool,
    /// The tensor's IOSurface pages imported on this helper's consumer
    /// device; the import retains the surface.
    iosurface_pages_import: ConsumerVulkanBuffer,
    /// Present only on an acquired one-off — a pooled tensor belongs to its
    /// pool, and a resolved one to its acquirer.
    pub(crate) release_to_parent: Option<HelperSurfaceReleaseDebt>,
    #[expect(
        dead_code,
        reason = "settled by its own Drop; nothing reads it, and that is the point"
    )]
    release_check_out_to_surface_share: HelperSurfaceCheckOutLeaseDebt,
}

impl HelperCheckedOutStorageBuffer {
    /// The tensor's IOSurface.
    pub(crate) fn iosurface(&self) -> &objc2_io_surface::IOSurfaceRef {
        self.iosurface_pages_import
            .backing_iosurface()
            .expect("an import built from an IOSurface is backed by it")
    }

    /// The no-copy `MTLBuffer` MoltenVK backs the imported pages with — the
    /// memory a kernel binding this tensor reads and writes.
    pub(crate) fn metal_buffer_over_the_iosurface_pages(
        &self,
    ) -> PyResult<objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLBuffer>>>
    {
        self.iosurface_pages_import
            .exported_metal_buffer()
            .map_err(|export_failure| {
                PyRuntimeError::new_err(format!(
                    "tensor surface {:?} has no Metal buffer over its IOSurface: \
                     {export_failure}",
                    self.surface_id
                ))
            })
    }
}

impl HelperProcessGpuExchangeClient {
    /// The storage-buffer arm of a macOS checkout: parse the declared tensor
    /// layout and import the one IOSurface's pages. Read-only until the
    /// acquire path claims it.
    pub(super) fn import_checked_out_storage_buffer(
        self: &Arc<Self>,
        surface_id: &str,
        response: &serde_json::Value,
        received_ports: Vec<SurfaceShareTransferredHandle>,
        release_check_out_to_surface_share: HelperSurfaceCheckOutLeaseDebt,
    ) -> PyResult<HelperCheckedOutStorageBuffer> {
        let tensor_layout = tensor_layout_of_a_check_out(surface_id, response)?;
        let [iosurface_port]: [SurfaceShareTransferredHandle; 1] =
            received_ports.try_into().map_err(|ports: Vec<_>| {
                PyRuntimeError::new_err(format!(
                    "tensor surface {surface_id:?} arrived with {} ports; its memory travels as \
                     exactly one IOSurface",
                    ports.len()
                ))
            })?;
        let iosurface =
            objc2_io_surface::IOSurfaceRef::lookup_from_mach_port(iosurface_port.as_raw_name())
                .ok_or_else(|| {
                    PyRuntimeError::new_err(format!(
                        "check_out of tensor surface {surface_id:?} carried a port that names \
                         no IOSurface"
                    ))
                })?;
        drop(iosurface_port);
        if (iosurface.alloc_size() as u64) < tensor_layout.byte_size() {
            return Err(PyRuntimeError::new_err(format!(
                "tensor surface {surface_id:?}'s IOSurface holds {} bytes, but its shape and \
                 dtype span {}",
                iosurface.alloc_size(),
                tensor_layout.byte_size()
            )));
        }
        let vulkan_device = self.consumer_vulkan_device()?;
        let iosurface_pages_import =
            ConsumerVulkanBuffer::from_iosurface_pages(&vulkan_device, &iosurface).map_err(
                |import_failure| {
                    PyRuntimeError::new_err(format!(
                        "Vulkan could not import tensor surface {surface_id:?}'s IOSurface: \
                         {import_failure}"
                    ))
                },
            )?;
        Ok(HelperCheckedOutStorageBuffer {
            surface_id: surface_id.to_string(),
            tensor_layout,
            writable: false,
            iosurface_pages_import,
            release_to_parent: None,
            release_check_out_to_surface_share,
        })
    }
}
