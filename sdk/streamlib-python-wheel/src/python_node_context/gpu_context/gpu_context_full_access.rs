// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `GpuContextFullAccess`, the privileged GPU capability a `setup` or
//! `teardown` hook receives.

use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::prelude::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::python_helper_process_pixel_exchange::HelperCheckedOutSurface;
use crate::python_helper_process_pixel_exchange::{
    HelperProcessGpuExchangeClient, NodeOutputPoolRequest,
};
use crate::python_node_context::format_vocabulary::parse_pixel_format_name;
use crate::python_node_context::gpu_surface_handle::PythonGpuSurfaceHandle;
use crate::python_node_context::kernels::PythonKernelDispatchBatch;
use crate::python_node_context::{
    escalate_scope_cannot_cross_the_process_boundary_error,
    gpu_unreachable_from_a_helper_process_error,
};

use super::{
    acquire_storage_buffer_through_the_helper_process_exchange,
    acquire_texture_through_the_helper_process_exchange,
};

/// The privileged GPU capability a `setup` / `teardown` hook receives.
///
/// Every method here is one escalate round trip to the parent, which runs
/// the privileged work against the engine's own capability and answers
/// with a handle. That is the shape, not a degradation of one: the
/// engine's escalate gate serializes runtime-wide and waits for device
/// idle before releasing, so each op arrives back already ordered.
///
/// What does not survive the process boundary is a *scope* spanning
/// several ops — see this capability's `escalate` refusal.
#[pyclass(name = "GpuContextFullAccess", module = "tatolab.stream", frozen)]
pub(crate) struct PythonGpuContextFullAccess {
    /// `None` means this helper was started without its GPU channels, and
    /// every method refuses by name.
    pub(in crate::python_node_context) helper_process_exchange_client:
        Option<Arc<HelperProcessGpuExchangeClient>>,
}

#[pymethods]
impl PythonGpuContextFullAccess {
    /// Acquire a pixel buffer through the privileged path.
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

    /// Acquire a pooled device texture through the privileged path.
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
    #[allow(
        clippy::too_many_arguments,
        reason = "pyo3 maps each Python keyword argument to its own parameter"
    )]
    #[pyo3(name = "acquire_texture_from_node_output_pool")]
    fn acquire_texture_from_node_output_pool(
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
            Some(NodeOutputPoolRequest {
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
    fn acquire_storage_buffer_from_node_output_pool(
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
            Some(NodeOutputPoolRequest {
                pool_key,
                rotation_depth,
            }),
        )
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

    /// Open a scope that records several dispatches and runs them as one.
    ///
    /// The Python equivalent of the engine's command-recorder flow, and the
    /// reason dispatch has two entry points in both languages: `kernel.dispatch()`
    /// for a single pass, this for several. Multi-pass work costs one round
    /// trip, one submission and one stall instead of N of each; leaving the
    /// scope returns with every write visible, same as a single dispatch.
    fn kernel_dispatch_batch(&self) -> PythonKernelDispatchBatch {
        PythonKernelDispatchBatch {
            helper_process_exchange_client: self
                .helper_process_exchange_client
                .as_ref()
                .map(Arc::clone),
            recording: Mutex::default(),
        }
    }

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
            "ctx.gpu_limited_access",
        ))
    }

    /// Block until the GPU device is idle.
    fn wait_device_idle(&self, python: Python<'_>) -> PyResult<()> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            return exchange_client.wait_device_idle(python);
        }
        let _ = python;
        Err(gpu_unreachable_from_a_helper_process_error())
    }
}
