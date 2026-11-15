// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `GpuContextLimitedAccess`, the non-allocating GPU capability a node holds
//! for its whole life.

use std::sync::Arc;

use pyo3::prelude::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::python_helper_process_pixel_exchange::HelperCheckedOutSurface;
use crate::python_helper_process_pixel_exchange::{
    HelperProcessGpuExchangeClient, ProcessorOutputPoolRequest,
};
use crate::python_processor_context::format_vocabulary::parse_pixel_format_name;
use crate::python_processor_context::gpu_surface_check_out_lease::PythonGpuSurfaceCheckOutLease;
use crate::python_processor_context::gpu_surface_handle::PythonGpuSurfaceHandle;
use crate::python_processor_context::{
    escalate_scope_cannot_cross_the_process_boundary_error,
    gpu_unreachable_from_a_helper_process_error,
};

use super::{
    acquire_storage_buffer_through_the_helper_process_exchange,
    acquire_texture_through_the_helper_process_exchange,
};

/// Non-allocating GPU capability, valid for the whole processor life.
///
/// Every call crosses to the parent through the exchange client — the
/// engine and its pools live one process away. `None` means this helper
/// has no surface-share channel, and every call refuses by name.
/// The claim a read took on the pooled GPU frame it carried, which holds the
/// frame's slot against a rehand for as long as the read's payload is held.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    allow(
        dead_code,
        reason = "only Linux and macOS read pooled frames through a claim table"
    )
)]
pub(crate) struct PooledGpuFrameClaimHeldByARead(
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[expect(
        dead_code,
        reason = "the claim is released by its own Drop; nothing reads it, and that is the point"
    )]
    Arc<crate::python_helper_process_pixel_exchange::GpuPoolSlotFrameClaimOfThisImporter>,
);

#[pyclass(name = "GpuContextLimitedAccess", module = "tatolab.stream", frozen)]
pub(crate) struct PythonGpuContextLimitedAccess {
    helper_process_exchange_client: Option<Arc<HelperProcessGpuExchangeClient>>,
}

impl PythonGpuContextLimitedAccess {
    pub(crate) fn new_for_helper_process(
        helper_process_exchange_client: Option<Arc<HelperProcessGpuExchangeClient>>,
    ) -> Self {
        Self {
            helper_process_exchange_client,
        }
    }

    /// Take in the pooled GPU frame a read carried and claim it in the pool's
    /// claim table; `None` where no claim table covers the frame.
    pub(crate) fn note_and_claim_the_pooled_gpu_frame_a_read_carries(
        &self,
        python: Python<'_>,
        pooled_frame: streamlib::sdk::context::GpuPoolSlotFrameDescriptorBody,
    ) -> PyResult<Option<PooledGpuFrameClaimHeldByARead>> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            return python
                .detach(|| {
                    exchange_client.note_and_claim_the_pooled_gpu_frame_a_read_carries(pooled_frame)
                })
                .map(|claim| claim.map(PooledGpuFrameClaimHeldByARead));
        }
        let _ = (python, pooled_frame);
        Ok(None)
    }

    /// The surface id the pooled frame a read carried is published under;
    /// `None` where this helper holds no claim table covering it.
    pub(crate) fn published_frame_id_of_the_pooled_gpu_frame(
        &self,
        pooled_frame: &streamlib::sdk::context::GpuPoolSlotFrameDescriptorBody,
    ) -> Option<String> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            return exchange_client.published_frame_id_of_the_pooled_gpu_frame(pooled_frame);
        }
        let _ = pooled_frame;
        None
    }

    /// The descriptor a write naming the pooled GPU frame `published_frame_id`
    /// carries, as this helper last read or acquired the frame.
    pub(crate) fn descriptor_of_the_pooled_gpu_frame_named(
        &self,
        published_frame_id: &str,
    ) -> Option<streamlib::sdk::context::GpuPoolSlotFrameDescriptorBody> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            return exchange_client.descriptor_of_the_pooled_gpu_frame_named(published_frame_id);
        }
        let _ = published_frame_id;
        None
    }
}

#[pymethods]
impl PythonGpuContextLimitedAccess {
    /// Acquire a pixel buffer from the pre-reserved pool.
    ///
    /// The pool lives with the engine: the parent allocates and checks the
    /// buffer into surface-share, and this process checks it out and imports
    /// the mapping — same handle, same views.
    #[pyo3(signature = (width, height, format = "bgra"))]
    fn acquire_pixel_buffer(
        &self,
        python: Python<'_>,
        width: u32,
        height: u32,
        format: &str,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        let pixel_format = parse_pixel_format_name(format)?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let checked_out = exchange_client.acquire_pixel_buffer(
                python,
                width,
                height,
                pixel_format.wire_name(),
            )?;
            return Ok(PythonGpuSurfaceHandle::from_helper_checked_out_surface(
                HelperCheckedOutSurface::PixelBuffer(checked_out),
            ));
        }
        let _ = (python, width, height, pixel_format);
        Err(gpu_unreachable_from_a_helper_process_error())
    }

    /// Acquire a pooled device texture, named by the surface id the engine
    /// minted for it.
    ///
    /// The id is the whole handle: a kernel dispatch binds it, and a
    /// downstream processor resolves it. `copy_src` and `copy_dst` ride
    /// every request, so the CPU doors reach the pixels — over the surface's
    /// host-visible staging on Linux, through its IOSurface on macOS — with
    /// no transfer usage spelled here.
    fn acquire_texture(
        &self,
        python: Python<'_>,
        width: u32,
        height: u32,
        format: &str,
        usage: Vec<String>,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        acquire_texture_through_the_helper_process_exchange(
            self.helper_process_exchange_client.as_ref(),
            python,
            width,
            height,
            format,
            &usage,
            None,
        )
    }

    /// The texture this frame publishes into, from the node output pool
    /// named `pool_key`: a fresh `<slot>#<generation>` per call, never a slot a
    /// consumer still holds.
    ///
    /// The pool rotates through `rotation_depth` slots, grows while consumers
    /// hold frames, and at its cap refuses by name — the producer drops its
    /// own frame rather than wait.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(name = "acquire_texture_from_node_output_pool")]
    fn acquire_texture_from_processor_output_pool(
        &self,
        python: Python<'_>,
        pool_key: &str,
        rotation_depth: u32,
        width: u32,
        height: u32,
        format: &str,
        usage: Vec<String>,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        acquire_texture_through_the_helper_process_exchange(
            self.helper_process_exchange_client.as_ref(),
            python,
            width,
            height,
            format,
            &usage,
            Some(ProcessorOutputPoolRequest {
                pool_key,
                rotation_depth,
            }),
        )
    }

    /// Acquire a tensor storage buffer of `shape` and `dtype` (`float32`,
    /// `float16`, `uint8`, `int32`), named by the surface id the engine minted
    /// for it. `torch.from_dlpack` reads and writes it on the GPU in place.
    fn acquire_storage_buffer(
        &self,
        python: Python<'_>,
        shape: Vec<u64>,
        dtype: &str,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        acquire_storage_buffer_through_the_helper_process_exchange(
            self.helper_process_exchange_client.as_ref(),
            python,
            shape,
            dtype,
            None,
        )
    }

    /// The tensor this frame publishes into, from the node output pool
    /// named `pool_key`: a fresh `<slot>#<generation>` per call, never a slot a
    /// consumer still holds; at the pool's cap, refused by name.
    #[pyo3(name = "acquire_storage_buffer_from_node_output_pool")]
    fn acquire_storage_buffer_from_processor_output_pool(
        &self,
        python: Python<'_>,
        pool_key: &str,
        rotation_depth: u32,
        shape: Vec<u64>,
        dtype: &str,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        acquire_storage_buffer_through_the_helper_process_exchange(
            self.helper_process_exchange_client.as_ref(),
            python,
            shape,
            dtype,
            Some(ProcessorOutputPoolRequest {
                pool_key,
                rotation_depth,
            }),
        )
    }

    /// Run `privileged_callback` with a temporary full-access GPU capability.
    ///
    /// Refused, and it is the shape rather than the reach that refuses it —
    /// the same answer `ctx.gpu_full_access.escalate` gives, because it is
    /// the same door.
    #[expect(
        clippy::unused_self,
        reason = "the refusal is this capability's whole answer for escalation"
    )]
    #[expect(
        unused_variables,
        reason = "the Python-visible parameter name is the API; stubtest compares it"
    )]
    fn escalate(&self, privileged_callback: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        Err(escalate_scope_cannot_cross_the_process_boundary_error(
            "ctx.gpu_full_access",
        ))
    }

    /// Resolve a surface id another processor published into a handle.
    fn resolve_surface(
        &self,
        python: Python<'_>,
        surface_id: &str,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let checked_out = exchange_client.resolve_surface(python, surface_id)?;
            return Ok(PythonGpuSurfaceHandle::from_helper_checked_out_surface(
                checked_out,
            ));
        }
        let _ = (python, surface_id);
        Err(gpu_unreachable_from_a_helper_process_error())
    }

    /// Claim a published surface against producer reuse until the returned
    /// lease is dropped.
    ///
    /// The cheap half of [`resolve_surface`]: it holds the frame still without
    /// importing its memory, so an object that wants only the pixels it was
    /// handed to stay put can keep the lease in a field and let its own
    /// lifetime do the releasing.
    ///
    /// [`resolve_surface`]: PythonGpuContextLimitedAccess::resolve_surface
    fn claim_surface_against_producer_reuse(
        &self,
        python: Python<'_>,
        surface_id: &str,
    ) -> PyResult<PythonGpuSurfaceCheckOutLease> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let claimed = python
                .detach(|| exchange_client.claim_surface_against_producer_reuse(surface_id))?;
            return Ok(PythonGpuSurfaceCheckOutLease {
                claimed_surface_id: surface_id.to_string(),
                held_against_producer_reuse: claimed,
            });
        }
        let _ = (python, surface_id);
        Err(gpu_unreachable_from_a_helper_process_error())
    }

    /// Whether an edit written back into this surface publishes at all —
    /// the engine's one answer for every write door: a write-back belongs
    /// to a pooled frame whose allocation is its only backing, or to a
    /// registered texture that takes a recorded copy in; a frame backed by
    /// neither answers `False`.
    /// `writable()` refuses on this answer; `cpu()` hands its array out
    /// read-only on it.
    fn surface_can_take_write_back(&self, python: Python<'_>, surface_id: &str) -> PyResult<bool> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            return exchange_client.surface_can_take_write_back(python, surface_id);
        }
        let _ = (python, surface_id);
        Err(gpu_unreachable_from_a_helper_process_error())
    }

    /// Convert `source_surface_id`'s YCbCr pixels (NV12 or YUYV) into
    /// `destination_surface`, an `rgba8_unorm` texture of the same extent
    /// allocated with `storage_binding` usage, returning once the
    /// destination's next reader would see them.
    ///
    /// `color_info` names the source's primaries, transfer, matrix and range;
    /// an axis it leaves unset takes the engine's YCbCr default. The source's
    /// transfer curve is kept.
    #[pyo3(signature = (source_surface_id, destination_surface, color_info = None))]
    fn convert_surface_into(
        &self,
        python: Python<'_>,
        source_surface_id: &str,
        destination_surface: &PythonGpuSurfaceHandle,
        color_info: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            return exchange_client.convert_surface_into(
                python,
                source_surface_id,
                &destination_surface.surface_id()?,
                color_info,
            );
        }
        let _ = (python, source_surface_id, destination_surface, color_info);
        Err(gpu_unreachable_from_a_helper_process_error())
    }

    /// Copy `source_surface_id`'s pixels into `destination_surface`, same
    /// format and extent, returning once the destination's next reader would
    /// see them.
    ///
    /// The engine picks the copy the two backings need and converts nothing:
    /// a format or extent mismatch, a retired frame, and a destination that
    /// cannot take a write-back each raise naming the reason.
    fn copy_surface_to_surface(
        &self,
        python: Python<'_>,
        source_surface_id: &str,
        destination_surface: &PythonGpuSurfaceHandle,
    ) -> PyResult<()> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            return exchange_client.copy_surface_to_surface(
                python,
                source_surface_id,
                &destination_surface.surface_id()?,
            );
        }
        let _ = (python, source_surface_id, destination_surface);
        Err(gpu_unreachable_from_a_helper_process_error())
    }
}
