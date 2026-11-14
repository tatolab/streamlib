// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! A tensor storage buffer as a macOS helper holds it: the engine's
//! byte-shaped IOSurface of a declared shape and dtype, its pages imported on
//! this helper's device, and the no-copy `MTLBuffer` over them that torch-MPS
//! and MLX read and write.

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::engine::apple_surface_share::RetainedIOSurfaceSharedAcrossThreads;
use streamlib::sdk::rhi::TensorStorageBufferLayout;
use streamlib_consumer_rhi::ConsumerVulkanBuffer;

use super::super::HelperProcessGpuExchangeClient;
use super::super::storage_buffer::tensor_layout_of_a_check_out;
use super::super::{
    HelperSurfaceHeldAgainstProducerReuse, HelperSurfaceReleaseDebt, SurfaceShareTransferredHandle,
};
use super::{HelperIOSurfaceUseCountClaim, HelperPoolSlotImport};

/// A tensor pool slot as this helper imported it, shared by every frame over
/// the slot: the tensor's IOSurface and its pages imported on this helper's
/// consumer device.
pub(crate) struct HelperIOSurfaceStorageBufferPoolSlotImport {
    tensor_layout: TensorStorageBufferLayout,
    iosurface_pages_import: ConsumerVulkanBuffer,
    iosurface: RetainedIOSurfaceSharedAcrossThreads,
}

impl HelperIOSurfaceStorageBufferPoolSlotImport {
    /// The slot's IOSurface.
    pub(super) fn iosurface(&self) -> &objc2_io_surface::IOSurfaceRef {
        &self.iosurface
    }
}

/// A checked-out tensor storage buffer. Fields drop in declaration order, so
/// the frame lets go of the slot's import before what holds the slot against
/// producer reuse frees it.
pub(crate) struct HelperCheckedOutStorageBuffer {
    pub(crate) surface_id: String,
    pub(crate) tensor_layout: TensorStorageBufferLayout,
    /// Whether this process may write the tensor: the acquirer may, a
    /// resolver may not.
    pub(crate) writable: bool,
    storage_buffer_pool_slot_import: Arc<HelperIOSurfaceStorageBufferPoolSlotImport>,
    /// Present only on an acquired one-off — a pooled tensor belongs to its
    /// pool, and a resolved one to its acquirer.
    pub(crate) release_to_parent: Option<HelperSurfaceReleaseDebt>,
    #[expect(
        dead_code,
        reason = "settled by its own Drop; nothing reads it, and that is the point"
    )]
    iosurface_use_count_claim: HelperIOSurfaceUseCountClaim,
    #[expect(
        dead_code,
        reason = "settled by its own Drop; nothing reads it, and that is the point"
    )]
    held_against_producer_reuse: HelperSurfaceHeldAgainstProducerReuse,
}

impl HelperCheckedOutStorageBuffer {
    /// The tensor's IOSurface.
    pub(crate) fn iosurface(&self) -> &objc2_io_surface::IOSurfaceRef {
        &self.storage_buffer_pool_slot_import.iosurface
    }

    /// The no-copy `MTLBuffer` MoltenVK backs the imported pages with — the
    /// memory a kernel binding this tensor reads and writes.
    pub(crate) fn metal_buffer_over_the_iosurface_pages(
        &self,
    ) -> PyResult<objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLBuffer>>>
    {
        self.storage_buffer_pool_slot_import
            .iosurface_pages_import
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
    /// A tensor pool slot's import, from the registration a checkout or a
    /// pool slot import answered: parse the declared tensor layout and import
    /// the one IOSurface's pages.
    pub(super) fn import_storage_buffer_pool_slot(
        &self,
        surface_id: &str,
        response: &serde_json::Value,
        received_ports: Vec<SurfaceShareTransferredHandle>,
    ) -> PyResult<HelperPoolSlotImport> {
        let tensor_layout = tensor_layout_of_a_check_out(surface_id, response)?;
        let [iosurface_port]: [SurfaceShareTransferredHandle; 1] =
            received_ports.try_into().map_err(|ports: Vec<_>| {
                PyRuntimeError::new_err(format!(
                    "tensor surface {surface_id:?} arrived with {} ports; its memory travels as \
                     exactly one IOSurface",
                    ports.len()
                ))
            })?;
        let iosurface = super::iosurface_named_by_the_check_out_port(
            &format!("tensor surface {surface_id:?}"),
            iosurface_port,
        )?;
        if u64::try_from(iosurface.alloc_size())
            .is_ok_and(|alloc_byte_size| alloc_byte_size < tensor_layout.byte_size())
        {
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
        Ok(HelperPoolSlotImport::StorageBuffer(Arc::new(
            HelperIOSurfaceStorageBufferPoolSlotImport {
                tensor_layout,
                iosurface_pages_import,
                iosurface: RetainedIOSurfaceSharedAcrossThreads::new(iosurface),
            },
        )))
    }
}

/// One tensor frame over its pool slot's import, read-only until the acquire
/// path claims it.
pub(super) fn storage_buffer_frame_over_a_pool_slot_import(
    surface_id: &str,
    storage_buffer_pool_slot_import: Arc<HelperIOSurfaceStorageBufferPoolSlotImport>,
    held_against_producer_reuse: HelperSurfaceHeldAgainstProducerReuse,
) -> HelperCheckedOutStorageBuffer {
    HelperCheckedOutStorageBuffer {
        surface_id: surface_id.to_string(),
        tensor_layout: storage_buffer_pool_slot_import.tensor_layout.clone(),
        writable: false,
        iosurface_use_count_claim: HelperIOSurfaceUseCountClaim::claiming(
            HelperPoolSlotImport::StorageBuffer(Arc::clone(&storage_buffer_pool_slot_import)),
        ),
        storage_buffer_pool_slot_import,
        release_to_parent: None,
        held_against_producer_reuse,
    }
}
