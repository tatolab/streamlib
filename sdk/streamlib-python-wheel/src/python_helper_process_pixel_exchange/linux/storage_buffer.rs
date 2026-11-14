// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A tensor storage buffer as a helper process holds it: engine-owned
//! DEVICE_LOCAL memory of a declared shape and dtype, imported into CUDA as
//! linear memory the first time it is exported — no staging, no refill, no
//! copy-back.

use std::os::fd::OwnedFd;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::rhi::TensorStorageBufferLayout;

use super::super::storage_buffer::tensor_layout_of_a_check_out;
use super::super::{
    HelperProcessGpuExchangeClient, HelperSurfaceHeldAgainstProducerReuse, HelperSurfaceReleaseDebt,
};
use super::export_staging::device_tensor_refused_on_a_software_device;
use super::parse_device_uuid;
use crate::python_cuda_pixel_exchange::{CudaImportedSurface, import_opaque_fd_into_cuda};

/// One tensor storage buffer pool slot as this helper imported it: the
/// OPAQUE_FD the slot was delivered and, once an export asked, CUDA's import
/// of it — shared by every frame over the slot.
pub(crate) struct HelperStorageBufferPoolSlotImport {
    tensor_layout: TensorStorageBufferLayout,
    exporting_device_uuid: [u8; 16],
    cuda_import: OnceLock<Arc<CudaImportedSurface>>,
    /// The OPAQUE_FD the checkout delivered, kept until CUDA adopts a dup of
    /// it, so a failed import can be retried.
    opaque_memory_fd_until_cuda_imports_it: Mutex<Option<OwnedFd>>,
}

/// A checked-out tensor storage buffer. Fields drop in declaration order, so
/// CUDA lets go of the memory before the checkout lease frees the slot.
pub(crate) struct HelperCheckedOutStorageBuffer {
    pub(crate) surface_id: String,
    pub(crate) tensor_layout: TensorStorageBufferLayout,
    /// Whether this process may write the tensor: the acquirer may, a
    /// resolver may not.
    pub(crate) writable: bool,
    storage_buffer_pool_slot_import: Arc<HelperStorageBufferPoolSlotImport>,
    /// Present only on an acquired one-off — a pooled tensor belongs to its
    /// pool, and a resolved one to its acquirer.
    pub(crate) release_to_parent: Option<HelperSurfaceReleaseDebt>,
    #[expect(
        dead_code,
        reason = "settled by its own Drop; nothing reads it, and that is the point"
    )]
    held_against_producer_reuse: HelperSurfaceHeldAgainstProducerReuse,
    pub(crate) exchange_client: Arc<HelperProcessGpuExchangeClient>,
}

#[cfg(test)]
impl HelperCheckedOutStorageBuffer {
    /// The slot's shared import this frame reads through.
    pub(crate) fn storage_buffer_pool_slot_import(
        &self,
    ) -> &Arc<HelperStorageBufferPoolSlotImport> {
        &self.storage_buffer_pool_slot_import
    }
}

impl HelperCheckedOutStorageBuffer {
    /// CUDA's linear-memory import of this tensor, made on first ask. CUDA
    /// maps the tensor's exact byte size, never the allocation's rounded one.
    /// Refused by the import device's name on a software device.
    pub(crate) fn cuda_import(&self) -> Result<Arc<CudaImportedSurface>, String> {
        let imported_slot = &self.storage_buffer_pool_slot_import;
        if let Some(imported) = imported_slot.cuda_import.get() {
            return Ok(Arc::clone(imported));
        }
        let software_device_name = self
            .exchange_client
            .software_import_device_name()
            .map_err(|import_device_failure| import_device_failure.to_string())?;
        if let Some(software_device_name) = software_device_name {
            return Err(device_tensor_refused_on_a_software_device(
                &self.surface_id,
                &software_device_name,
            ));
        }
        let mut opaque_memory_fd = imported_slot.opaque_memory_fd_until_cuda_imports_it.lock();
        if let Some(imported) = imported_slot.cuda_import.get() {
            return Ok(Arc::clone(imported));
        }
        let fd_for_cuda = opaque_memory_fd
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "tensor surface {:?} holds no memory fd to import into CUDA",
                    self.surface_id
                )
            })?
            .try_clone()
            .map_err(|duplicate_failure| {
                format!("could not duplicate tensor surface's memory fd: {duplicate_failure}")
            })?;
        let imported = Arc::new(import_opaque_fd_into_cuda(
            fd_for_cuda,
            imported_slot.tensor_layout.byte_size(),
            imported_slot.exporting_device_uuid,
        )?);
        *opaque_memory_fd = None;
        Ok(Arc::clone(
            imported_slot.cuda_import.get_or_init(|| imported),
        ))
    }

    /// CUDA's import, if an export has already made it.
    pub(crate) fn cuda_import_already_made(&self) -> Option<Arc<CudaImportedSurface>> {
        self.storage_buffer_pool_slot_import
            .cuda_import
            .get()
            .cloned()
    }
}

impl HelperProcessGpuExchangeClient {
    /// The storage-buffer arm of a checkout: parse the declared tensor layout
    /// and keep the one OPAQUE_FD for CUDA's import.
    pub(super) fn import_checked_out_storage_buffer(
        &self,
        surface_id: &str,
        response: &serde_json::Value,
        plane_fds: Vec<OwnedFd>,
    ) -> PyResult<HelperStorageBufferPoolSlotImport> {
        let tensor_layout = tensor_layout_of_a_check_out(surface_id, response)?;
        let handle_type = response
            .get("handle_type")
            .and_then(|value| value.as_str())
            .unwrap_or("dma_buf");
        if handle_type != "opaque_fd" {
            return Err(PyRuntimeError::new_err(format!(
                "tensor surface {surface_id:?} is registered as {handle_type:?}; CUDA imports a \
                 tensor's memory only as an opaque_fd"
            )));
        }
        let [opaque_memory_fd]: [OwnedFd; 1] = plane_fds.try_into().map_err(|fds: Vec<_>| {
            PyRuntimeError::new_err(format!(
                "tensor surface {surface_id:?} arrived with {} fds; its memory travels as \
                 exactly one",
                fds.len()
            ))
        })?;
        let registered_byte_size = response
            .get("plane_sizes")
            .and_then(|sizes| sizes.get(0))
            .and_then(|size| size.as_u64());
        if registered_byte_size != Some(tensor_layout.byte_size()) {
            return Err(PyRuntimeError::new_err(format!(
                "tensor surface {surface_id:?} registered {registered_byte_size:?} bytes, but \
                 its shape and dtype span {}",
                tensor_layout.byte_size()
            )));
        }
        let exporting_device_uuid = parse_device_uuid(
            response
                .get("exporting_device_uuid")
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    PyRuntimeError::new_err(format!(
                        "tensor surface {surface_id:?} carries no exporting device UUID; CUDA \
                         must import onto the GPU that owns the memory"
                    ))
                })?,
        )?;
        Ok(HelperStorageBufferPoolSlotImport {
            tensor_layout,
            exporting_device_uuid,
            cuda_import: OnceLock::new(),
            opaque_memory_fd_until_cuda_imports_it: Mutex::new(Some(opaque_memory_fd)),
        })
    }

    /// One frame over a tensor storage buffer pool slot's import, read-only
    /// until the acquire path claims it.
    pub(super) fn storage_buffer_checked_out_over_a_pool_slot_import(
        self: &Arc<Self>,
        surface_id: &str,
        storage_buffer_pool_slot_import: Arc<HelperStorageBufferPoolSlotImport>,
        held_against_producer_reuse: HelperSurfaceHeldAgainstProducerReuse,
    ) -> HelperCheckedOutStorageBuffer {
        HelperCheckedOutStorageBuffer {
            surface_id: surface_id.to_string(),
            tensor_layout: storage_buffer_pool_slot_import.tensor_layout.clone(),
            writable: false,
            storage_buffer_pool_slot_import,
            release_to_parent: None,
            held_against_producer_reuse,
            exchange_client: Arc::clone(self),
        }
    }
}
