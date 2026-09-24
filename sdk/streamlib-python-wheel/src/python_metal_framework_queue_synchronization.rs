// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Retiring a GPU framework's writes through a Metal DLPack capsule before
//! the engine can read the surface they landed in.
//!
//! The write lands in the surface itself, but only once the framework's
//! queue retires it, and nothing in the capsule protocol carries that
//! ordering: torch and MLX pass no `stream` when they import a `kDLMetal`
//! capsule.

use pyo3::prelude::*;
use pyo3::types::PyDict;

/// Drain torch's MPS queue — every MPS stream — when this process imported
/// torch.
///
/// MLX is not drained: `mx.synchronize()` holds the GIL while it waits, and
/// an MLX completion handler freeing a DLPack-imported array needs the GIL,
/// so the wait deadlocks. An MLX write is retired by the `mx.eval` its write
/// contract puts inside the scope.
pub(crate) fn drain_torch_mps_queue_if_imported(python: Python<'_>) -> PyResult<()> {
    let imported_modules = python.import("sys")?.getattr("modules")?;
    let imported_modules = imported_modules.cast::<PyDict>()?;
    if let Some(torch) = imported_modules.get_item("torch")?
        && torch
            .getattr("backends")?
            .getattr("mps")?
            .call_method0("is_available")?
            .is_truthy()?
    {
        torch.getattr("mps")?.call_method0("synchronize")?;
    }
    Ok(())
}
