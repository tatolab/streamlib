// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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
    HelperProcessGpuExchangeClient, HelperSurfaceCheckOutLeaseDebt, HelperSurfaceReleaseDebt,
};
use super::parse_device_uuid;
use crate::python_cuda_pixel_exchange::{CudaImportedSurface, import_opaque_fd_into_cuda};

/// A checked-out tensor storage buffer. Fields drop in declaration order, so
/// CUDA lets go of the memory before the checkout lease frees the slot.
pub(crate) struct HelperCheckedOutStorageBuffer {
    pub(crate) surface_id: String,
    pub(crate) tensor_layout: TensorStorageBufferLayout,
    /// Whether this process may write the tensor: the acquirer may, a
    /// resolver may not.
    pub(crate) writable: bool,
    exporting_device_uuid: [u8; 16],
    cuda_import: OnceLock<Arc<CudaImportedSurface>>,
    /// The OPAQUE_FD the checkout delivered, kept until CUDA adopts a dup of
    /// it, so a failed import can be retried.
    opaque_memory_fd_until_cuda_imports_it: Mutex<Option<OwnedFd>>,
    /// Present only on an acquired one-off — a pooled tensor belongs to its
    /// pool, and a resolved one to its acquirer.
    pub(crate) release_to_parent: Option<HelperSurfaceReleaseDebt>,
    #[expect(
        dead_code,
        reason = "settled by its own Drop; nothing reads it, and that is the point"
    )]
    release_check_out_to_surface_share: HelperSurfaceCheckOutLeaseDebt,
    pub(crate) exchange_client: Arc<HelperProcessGpuExchangeClient>,
}

impl HelperCheckedOutStorageBuffer {
    /// CUDA's linear-memory import of this tensor, made on first ask. CUDA
    /// maps the tensor's exact byte size, never the allocation's rounded one.
    pub(crate) fn cuda_import(&self) -> Result<Arc<CudaImportedSurface>, String> {
        if let Some(imported) = self.cuda_import.get() {
            return Ok(Arc::clone(imported));
        }
        let mut opaque_memory_fd = self.opaque_memory_fd_until_cuda_imports_it.lock();
        if let Some(imported) = self.cuda_import.get() {
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
            self.tensor_layout.byte_size(),
            self.exporting_device_uuid,
        )?);
        *opaque_memory_fd = None;
        Ok(Arc::clone(self.cuda_import.get_or_init(|| imported)))
    }

    /// CUDA's import, if an export has already made it.
    pub(crate) fn cuda_import_already_made(&self) -> Option<Arc<CudaImportedSurface>> {
        self.cuda_import.get().cloned()
    }
}

impl HelperProcessGpuExchangeClient {
    /// The storage-buffer arm of a checkout: parse the declared tensor layout
    /// and keep the one OPAQUE_FD for CUDA's import. Read-only until the
    /// acquire path claims it.
    pub(super) fn import_checked_out_storage_buffer(
        self: &Arc<Self>,
        surface_id: &str,
        response: &serde_json::Value,
        plane_fds: Vec<OwnedFd>,
    ) -> PyResult<HelperCheckedOutStorageBuffer> {
        // Owed from the moment the service granted the checkout, so a refusal
        // below releases the claim instead of pinning the slot until this
        // helper's connection closes.
        let release_check_out_to_surface_share = HelperSurfaceCheckOutLeaseDebt {
            exchange_client: Arc::clone(self),
            surface_id: surface_id.to_string(),
        };
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
        Ok(HelperCheckedOutStorageBuffer {
            surface_id: surface_id.to_string(),
            tensor_layout,
            writable: false,
            exporting_device_uuid,
            cuda_import: OnceLock::new(),
            opaque_memory_fd_until_cuda_imports_it: Mutex::new(Some(opaque_memory_fd)),
            release_to_parent: None,
            release_check_out_to_surface_share,
            exchange_client: Arc::clone(self),
        })
    }
}
