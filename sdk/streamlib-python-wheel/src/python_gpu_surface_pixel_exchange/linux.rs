// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The Linux arms of the pixel exchange: the device export CUDA imports, the
//! CPU readback staging, and their write-backs to the surface.

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib_adapter_cuda::dlpack::{self, Device};

use crate::python_helper_process_pixel_exchange::{
    CpuReadbackCopyDirection, HelperCheckedOutSurface, HelperCpuReadbackExport, HelperDeviceExport,
    HelperProcessGpuExchangeClient,
};

use super::gpu_surface_owned_memory::GpuSurfaceOwnedMemory;
use super::pending_staged_write_back_to_surface::PendingStagedWriteBackToSurface;
use super::pixel_exchange_tensor_layout::PixelExchangeTensorLayout;
use super::{DlpackExchangeShape, HostVisiblePixelPlaneView, dlpack_capsule_over};

impl GpuSurfaceOwnedMemory {
    /// A DMA-BUF fd for this surface's first plane, plus its byte size.
    ///
    /// A checked-out surface answers from the fds it was handed at
    /// check-out — they are the engine's own export, dup'd by the kernel
    /// on the way over, so there is nothing to ask the parent for.
    pub(crate) fn export_dma_buf(&self) -> PyResult<(i32, u64)> {
        self.checked_out_surface.export_dma_buf()
    }

    /// The OPAQUE_FD memory handle plus its allocation-stable shape —
    /// the raw-handle twin of [`Self::export_dma_buf`], answered from the
    /// same checked-out fds.
    pub(crate) fn export_opaque_fd(
        &self,
    ) -> PyResult<crate::python_helper_process_pixel_exchange::OpaqueFdAllocationExportDescription>
    {
        self.checked_out_surface.export_opaque_fd()
    }

    /// Whether the CPU reaches this surface's pixels through the engine's
    /// host-visible export staging rather than the surface's own
    /// allocation.
    ///
    /// True for both texture backings — tiled device memory has no host
    /// mapping to address — and for a pixel buffer whose allocation
    /// imported without one. What routes the CPU doors; no door names it.
    pub(crate) fn cpu_reach_goes_through_the_export_staging(&self) -> bool {
        match &self.checked_out_surface {
            HelperCheckedOutSurface::PixelBuffer(pixel_surface) => {
                pixel_surface.host_mapped_base_address().is_null()
            }
            HelperCheckedOutSurface::Texture(_)
            | HelperCheckedOutSurface::AcquiredDeviceTexture(_)
            | HelperCheckedOutSurface::StorageBuffer(_) => true,
        }
    }

    /// How a refusal names this surface — its minted id where it has one,
    /// and the checkout's own id otherwise, so no refusal says "None".
    pub(crate) fn surface_id_for_a_refusal(&self) -> &str {
        self.minted_surface_id
            .as_deref()
            .unwrap_or_else(|| self.checked_out_surface.surface_id())
    }

    /// The staged edit outstanding against this surface, if any door has
    /// one — the cell every door arms, discards and publishes through.
    pub(crate) fn pending_staged_write_back(&self) -> &PendingStagedWriteBackToSurface {
        &self.pending_staged_write_back
    }

    /// The host-mapped pixel view, or a refusal naming why this surface
    /// has none. The single answer to "can the CPU address these bytes?"
    /// — every host-side accessor routes through it.
    ///
    /// A surface the CPU cannot address directly answers from its
    /// readback staging's mapping, which the CPU door checked out and
    /// read this frame into on entry. Geometry comes from the staging
    /// alone — the object the byte span was sized for.
    pub(crate) fn host_visible_pixel_plane(&self) -> PyResult<HostVisiblePixelPlaneView> {
        if let HelperCheckedOutSurface::PixelBuffer(pixel_surface) = &self.checked_out_surface
            && !pixel_surface.host_mapped_base_address().is_null()
        {
            return Ok(pixel_surface.host_visible_pixel_plane_view());
        }
        let surface_id = self.checked_out_surface.surface_id();
        let staged = self
            .checked_out_surface
            .exchange_client()
            .cpu_readback_export_already_open(surface_id)
            .ok_or_else(|| {
                PyRuntimeError::new_err(format!(
                    "no CPU door is open on surface {surface_id:?}: its pixels reach the CPU \
                     through the engine's host-visible staging, which `lock()` plus a host-side \
                     accessor (`as_numpy`, `base_address`, `__dlpack__`) checks out and reads \
                     the frame into"
                ))
            })?;
        Ok(HostVisiblePixelPlaneView {
            base_address: staged.consumer_buffer.mapped_ptr(),
            bytes_per_row: staged.bytes_per_row,
            width: staged.width,
            height: staged.height,
            format: staged.format,
        })
    }
}

/// A surface's device export as a helper process holds it: CUDA's import
/// of the staging buffer the parent owns, the timeline its refills
/// signal, and the client to ask for the next one.
///
/// The staging is the engine's — one allocation per surface, refilled by
/// a VRAM-side copy. This side holds the consumer half.
#[derive(Clone)]
pub(crate) struct SurfaceDeviceExport {
    surface_id: String,
    helper_device_export: Arc<HelperDeviceExport>,
    exchange_client: Arc<HelperProcessGpuExchangeClient>,
}

impl SurfaceDeviceExport {
    /// The DLPack device CUDA's import of the staging lives on.
    pub(crate) fn imported_dlpack_device(&self) -> Device {
        self.helper_device_export.cuda_import.dlpack_device()
    }
}

/// This surface's device export, opening it on first ask.
///
/// The escalate round trip needs the GIL attached to make the call at all;
/// its wait releases it.
pub(crate) fn surface_device_export_for(
    python: Python<'_>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<SurfaceDeviceExport> {
    let surface_id = owned_memory.minted_surface_id.as_deref().ok_or_else(|| {
        PyRuntimeError::new_err(
            "this surface carries no id, so there is nothing to key a device export on; device \
             tensors come from graph frames (resolve_surface) or published pixel buffers",
        )
    })?;
    let exchange_client = owned_memory.checked_out_surface.exchange_client();
    Ok(SurfaceDeviceExport {
        surface_id: surface_id.to_string(),
        helper_device_export: exchange_client.open_device_export(python, surface_id)?,
        exchange_client: Arc::clone(exchange_client),
    })
}

/// Everything a device capsule needs. The costly parts run detached —
/// the refill is a GPU submit plus a wait, and the first call also
/// allocates the staging and imports it into CUDA — but the helper arm
/// crosses to the parent, which needs the GIL attached to make the call
/// and releases it inside its own wait.
#[derive(Clone)]
pub(crate) struct PreparedDeviceExport {
    pub(crate) export: SurfaceDeviceExport,
    pub(crate) layout: PixelExchangeTensorLayout,
    pub(crate) writable: bool,
}

/// Refill the staging from the surface's current pixels and derive the
/// tensor layout.
pub(crate) fn prepare_device_export(
    python: Python<'_>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<PreparedDeviceExport> {
    let export = surface_device_export_for(python, owned_memory)?;
    export.exchange_client.run_device_export_copy(
        python,
        "refill_device_export_staging",
        &export.surface_id,
        &export.helper_device_export,
    )?;
    // Geometry comes from the staging alone — the object the byte span was
    // sized for — never mixed with the handle's own pixel view. The engine
    // records the pixel shape for both source kinds (a texture source maps
    // to its own color shape, float widths included — BGRA stays BGRA).
    let layout = PixelExchangeTensorLayout::for_pixel_format(
        export.helper_device_export.format,
        export.helper_device_export.width,
        export.helper_device_export.height,
        export.helper_device_export.bytes_per_row,
    )?;
    let writable = export.helper_device_export.writable;
    Ok(PreparedDeviceExport {
        export,
        layout,
        writable,
    })
}

/// Build the capsule over a prepared export — the attached half; touches
/// no engine lock.
pub(crate) fn device_dlpack_capsule<'py>(
    python: Python<'py>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    prepared: PreparedDeviceExport,
    exchange_shape: DlpackExchangeShape,
    read_only_lock: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let read_only = read_only_lock || !prepared.writable;
    let cuda_import = Arc::clone(&prepared.export.helper_device_export.cuda_import);
    // The capsule outlives the handle, so it holds the layers the pointer
    // depends on: the surface, and the device export — which is what owns
    // CUDA's import of the staging.
    let owner: dlpack::CapsuleOwner = Box::new((Arc::clone(owned_memory), prepared.export));
    dlpack_capsule_over(
        python,
        cuda_import.device_pointer(),
        prepared.layout,
        cuda_import.dlpack_device(),
        exchange_shape,
        read_only,
        owner,
    )
}

/// Build a `kDLCUDA` capsule straight over a tensor storage buffer's memory,
/// in its declared shape — no staging, no refill, no copy-back. The capsule
/// holds the surface, and with it CUDA's import and the checkout lease, so the
/// slot is not reused while the tensor lives. Detached for the import.
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
    let cuda_import = python
        .detach(|| storage_buffer.cuda_import())
        .map_err(crate::python_processor_context::gpu_operation_error)?;
    dlpack_capsule_over(
        python,
        cuda_import.device_pointer(),
        layout,
        cuda_import.dlpack_device(),
        exchange_shape,
        read_only,
        Box::new(Arc::clone(owned_memory)),
    )
}

/// Order every CUDA write to a tensor storage buffer ahead of any other
/// holder's read: the device-wide synchronize the write-back path uses, since
/// no fence connects a consumer's CUDA stream to the engine. A tensor CUDA
/// never imported has nothing to order.
pub(crate) fn publish_tensor_storage_buffer_device_writes(
    python: Python<'_>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<()> {
    let Some(cuda_import) = owned_memory
        .tensor_storage_buffer()
        .and_then(|storage_buffer| storage_buffer.cuda_import_already_made())
    else {
        return Ok(());
    };
    python
        .detach(|| {
            crate::python_cuda_pixel_exchange::synchronize_every_stream_on_the_import_device(
                &cuda_import,
            )
        })
        .map_err(crate::python_processor_context::gpu_operation_error)
}

/// Publish a device-side write back into the surface, so every other
/// holder observes the edit. Runs when a scope holding a writable
/// device tensor is left normally — the handle's unlock/close, or the
/// device-tensor scope's exit.
///
/// The consumer's writes ride its own CUDA stream and the publish is a
/// Vulkan copy that reads the staging; the device-wide synchronize is
/// the only thing ordering the two APIs, so the copy reads finished
/// pixels rather than a torn frame. Detached — it can block for a full
/// frame's GPU work.
pub(crate) fn publish_device_write_back_to_surface(
    python: Python<'_>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<()> {
    let export = surface_device_export_for(python, owned_memory)?;
    let cuda_import = Arc::clone(&export.helper_device_export.cuda_import);
    python
        .detach(|| {
            crate::python_cuda_pixel_exchange::synchronize_every_stream_on_the_import_device(
                &cuda_import,
            )
        })
        .map_err(crate::python_processor_context::gpu_operation_error)?;
    export.exchange_client.run_device_export_copy(
        python,
        "copy_device_export_staging_back_to_surface",
        &export.surface_id,
        &export.helper_device_export,
    )
}

/// A surface's CPU-readback export as a helper process holds it: this
/// child's mapping of the host-visible staging the parent owns, and the
/// client to ask for the next copy.
pub(crate) struct SurfaceCpuReadbackExport {
    surface_id: String,
    cpu_readback_export: Arc<HelperCpuReadbackExport>,
    exchange_client: Arc<HelperProcessGpuExchangeClient>,
}

/// This surface's CPU-readback export, opening it on first ask — the
/// staged twin of [`surface_device_export_for`].
fn surface_cpu_readback_export_for(
    python: Python<'_>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<SurfaceCpuReadbackExport> {
    let surface_id = owned_memory.minted_surface_id.as_deref().ok_or_else(|| {
        PyRuntimeError::new_err(
            "this surface carries no id, so there is nothing to key a readback staging on; the \
             staged CPU door serves graph frames (resolve_surface) and surfaces this processor \
             acquired",
        )
    })?;
    let exchange_client = owned_memory.checked_out_surface.exchange_client();
    Ok(SurfaceCpuReadbackExport {
        surface_id: surface_id.to_string(),
        cpu_readback_export: exchange_client.open_cpu_readback_export(python, surface_id)?,
        exchange_client: Arc::clone(exchange_client),
    })
}

/// Make this surface's readback staging exist and be mapped, without
/// reading a frame into it.
///
/// Enough to answer a geometry question — the staging's own shape is what
/// the row pitch comes from — and deliberately not enough for pixels:
/// reading those is the door's job, and the copy belongs where the frame
/// is actually asked for.
pub(crate) fn map_the_cpu_staging_without_reading_a_frame_in(
    python: Python<'_>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<()> {
    if !owned_memory.cpu_reach_goes_through_the_export_staging() {
        return Ok(());
    }
    surface_cpu_readback_export_for(python, owned_memory)?;
    Ok(())
}

/// Open the staged CPU door over this frame: check the readback staging
/// out, map it, and read the frame's current pixels in.
///
/// Entering always reads in, a pure write included. The read is what
/// makes a later write-back legal — the engine refuses a publish into a
/// staging no frame was read into, because one staging spans every frame
/// its pool slot publishes and it inspects no bag content, so it cannot
/// tell a fully-overwritten staging from uninitialised memory.
pub(crate) fn read_the_frame_into_its_cpu_staging(
    python: Python<'_>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<Arc<HelperCpuReadbackExport>> {
    let staged = surface_cpu_readback_export_for(python, owned_memory)?;
    staged.exchange_client.run_cpu_readback_copy(
        python,
        CpuReadbackCopyDirection::SurfaceIntoStaging,
        &staged.surface_id,
        &staged.cpu_readback_export,
    )?;
    Ok(staged.cpu_readback_export)
}

/// Publish a staged CPU edit back into the surface's own allocation as
/// one engine-ordered copy — the block edge's publication point.
pub(crate) fn publish_cpu_staged_write_back_to_surface(
    python: Python<'_>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<()> {
    let staged = surface_cpu_readback_export_for(python, owned_memory)?;
    staged.exchange_client.run_cpu_readback_copy(
        python,
        CpuReadbackCopyDirection::StagingBackIntoSurface,
        &staged.surface_id,
        &staged.cpu_readback_export,
    )
}

/// The DLPack device this surface's tensors would live on, importing on
/// first ask — the driver's own classification of the mapped pointer is
/// the only honest answer.
pub(crate) fn imported_device_for(
    python: Python<'_>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<Device> {
    Ok(surface_device_export_for(python, owned_memory)?
        .helper_device_export
        .cuda_import
        .dlpack_device())
}

/// Whether this surface can even attempt a device export: it has an id
/// to key on, and a way to reach the staging — the engine capability in
/// this process, or the parent that owns it.
pub(crate) fn device_export_available(owned_memory: &Arc<GpuSurfaceOwnedMemory>) -> bool {
    owned_memory.minted_surface_id.is_some()
}
