// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::python_helper_process_pixel_exchange::HelperCheckedOutSurface;
use crate::python_helper_process_pixel_exchange::{
    HelperProcessGpuExchangeClient, ProcessorOutputPoolRequest,
};

use super::format_vocabulary::parse_texture_format_name;
use super::gpu_surface_handle::PythonGpuSurfaceHandle;
use super::gpu_unreachable_from_a_helper_process_error;

mod gpu_context_full_access;
mod gpu_context_full_access_kernels;
mod gpu_context_full_access_surface_exchange;
mod gpu_context_limited_access;

pub(crate) use gpu_context_full_access::PythonGpuContextFullAccess;
pub(crate) use gpu_context_limited_access::{
    PooledGpuFrameClaimHeldByARead, PythonGpuContextLimitedAccess,
};

pub(crate) fn gpu_operation_error(failure: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(failure.to_string())
}

/// Acquire a texture through a helper's exchange with its parent — a one-off,
/// or the next frame of `processor_output_pool`.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    allow(unused_variables)
)]
fn acquire_texture_through_the_helper_process_exchange(
    helper_process_exchange_client: Option<&Arc<HelperProcessGpuExchangeClient>>,
    python: Python<'_>,
    width: u32,
    height: u32,
    format: &str,
    usage: &[String],
    processor_output_pool: Option<ProcessorOutputPoolRequest<'_>>,
) -> PyResult<PythonGpuSurfaceHandle> {
    let texture_format = parse_texture_format_name(format)?;
    #[cfg(target_os = "linux")]
    if let Some(exchange_client) = helper_process_exchange_client {
        let acquired = exchange_client.acquire_texture(
            python,
            width,
            height,
            texture_format,
            usage,
            processor_output_pool,
        )?;
        return Ok(PythonGpuSurfaceHandle::from_helper_acquired_texture(
            acquired,
        ));
    }
    #[cfg(target_os = "macos")]
    if let Some(exchange_client) = helper_process_exchange_client {
        let acquired = exchange_client.acquire_texture(
            python,
            width,
            height,
            texture_format,
            usage,
            processor_output_pool,
        )?;
        return Ok(PythonGpuSurfaceHandle::from_helper_checked_out_surface(
            HelperCheckedOutSurface::Texture(acquired),
        ));
    }
    let _ = texture_format;
    Err(gpu_unreachable_from_a_helper_process_error())
}

/// Acquire a tensor storage buffer through the helper's exchange client: a
/// one-off, or the next tensor of the processor output pool named by
/// `processor_output_pool`.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    expect(
        unused_variables,
        reason = "off Linux and macOS the request is refused before it reaches the parent"
    )
)]
fn acquire_storage_buffer_through_the_helper_process_exchange(
    helper_process_exchange_client: Option<&Arc<HelperProcessGpuExchangeClient>>,
    python: Python<'_>,
    shape: Vec<u64>,
    dtype: &str,
    processor_output_pool: Option<ProcessorOutputPoolRequest<'_>>,
) -> PyResult<PythonGpuSurfaceHandle> {
    let tensor_layout = streamlib::sdk::rhi::TensorStorageBufferLayout::from_wire(shape, dtype)
        .map_err(|refusal| pyo3::exceptions::PyValueError::new_err(refusal.to_string()))?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(exchange_client) = helper_process_exchange_client {
        let acquired = exchange_client.acquire_storage_buffer(
            python,
            &tensor_layout,
            processor_output_pool,
        )?;
        return Ok(PythonGpuSurfaceHandle::from_helper_checked_out_surface(
            HelperCheckedOutSurface::StorageBuffer(acquired),
        ));
    }
    Err(gpu_unreachable_from_a_helper_process_error())
}
