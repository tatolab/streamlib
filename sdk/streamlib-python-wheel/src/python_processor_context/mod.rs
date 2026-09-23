// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The capability-typed runtime contexts handed to Python lifecycle hooks.
//!
//! Built in the helper process the processor runs in: everything a hook
//! reads is local, was passed down by the parent, or crosses to it — the
//! GPU surface through the exchange client, whose escalate wait releases
//! the GIL so a slow parent parks one thread and never the interpreter.
//! GIL discipline: a call that crosses to the parent is made attached —
//! there is no reaching the bridge otherwise — and releases the GIL inside
//! its own wait. Everything else that can block runs inside a
//! `python.detach(..)` closure.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

mod format_vocabulary;
mod gpu_context;
mod gpu_surface_check_out_lease;
mod gpu_surface_device_tensor_scope;
mod gpu_surface_handle;
#[cfg(target_os = "linux")]
mod kernel_wire_encoding;
mod kernels;
mod link_data_access;
mod runtime_context;

pub(crate) use format_vocabulary::parse_pixel_format_name;
pub(crate) use gpu_context::{
    PythonGpuContextFullAccess, PythonGpuContextLimitedAccess, gpu_operation_error,
};
#[cfg(target_os = "linux")]
pub(crate) use gpu_surface_check_out_lease::{
    ExportedVkImageCreationRecipe, OpaqueFdExportContract,
};
pub(crate) use gpu_surface_check_out_lease::{
    PythonGpuSurfaceCheckOutLease, PythonOpaqueFdTextureExport,
};
pub(crate) use gpu_surface_device_tensor_scope::PythonGpuSurfaceDeviceTensorScope;
pub(crate) use gpu_surface_handle::PythonGpuSurfaceHandle;
#[cfg(target_os = "linux")]
pub(crate) use kernels::ReflectedKernelBinding;
pub(crate) use kernels::{
    PythonAccelerationStructureHandle, PythonComputeKernel, PythonGraphicsKernel,
    PythonKernelDispatchBatch, PythonRayTracingKernel,
};
pub(crate) use link_data_access::{PythonLinkInputDataReader, PythonLinkOutputDataWriter};
pub(crate) use runtime_context::{
    PythonRuntimeContextFullAccess, PythonRuntimeContextLimitedAccess,
};

/// The refusal `escalate` gives on either capability.
///
/// `sibling_capability_attribute_name` is the other capability on the same
/// context, so the message points at the whole surface the callback's
/// operations moved to rather than half of it.
fn escalate_scope_cannot_cross_the_process_boundary_error(
    sibling_capability_attribute_name: &str,
) -> PyErr {
    PyRuntimeError::new_err(format!(
        "escalate() gives its callback one atomic privileged scope, which cannot span a process \
         boundary. The operations it wrapped are methods on this capability and on \
         `{sibling_capability_attribute_name}` — call them directly; each is privileged on its own"
    ))
}

/// Whether a context manager's `__exit__` was reached by a propagating
/// exception. pyo3 maps Python's `None` to the `None` variant for an
/// `Option` parameter, so `is_some()` is the whole test — spelled once,
/// because a reader should not have to learn pyo3's argument mapping at
/// three call sites.
fn left_by_a_propagating_exception(exception_type: Option<&Bound<'_, PyAny>>) -> bool {
    exception_type.is_some()
}

/// The refusal an fd-shaped raw-handle method gives on a platform whose
/// surfaces are IOSurfaces, not file descriptors.
#[cfg(not(target_os = "linux"))]
fn fd_shaped_raw_handle_is_linux_only_error(method_name: &str) -> PyErr {
    PyRuntimeError::new_err(format!(
        "{method_name} is Linux-only: DMA-BUF and OPAQUE_FD are Linux file-descriptor handles, \
         and a surface on this platform is an IOSurface: its raw handle is `export_iosurface`"
    ))
}

/// The variable the parent names its surface-share channel to a helper in:
/// the Unix socket's path on Linux, the Mach service's name on macOS.
#[cfg(not(target_os = "macos"))]
pub(crate) const SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_SURFACE_SOCKET";
#[cfg(target_os = "macos")]
pub(crate) const SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE: &str =
    streamlib_surface_client::SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE;

/// The refusal a GPU call gets when this process has neither an engine view
/// nor a channel to a parent that has one.
///
/// In a helper process the pixel exchange normally crosses to the parent;
/// reaching this refusal means the helper was started without its
/// surface-share channel — a platform without one, or a parent too old to
/// pass it.
fn gpu_unreachable_from_a_helper_process_error() -> PyErr {
    PyRuntimeError::new_err(
        "the GPU is not reachable from this Python processor: its helper process was started \
         without a surface-share channel to the engine. Frames still flow — `ctx.inputs` and \
         `ctx.outputs` carry bags — but acquiring or mapping a surface cannot.",
    )
}
