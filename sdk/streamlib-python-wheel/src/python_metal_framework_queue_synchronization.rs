// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Draining the Metal queues of the GPU frameworks a helper imported, so a
//! write through a Metal DLPack capsule has landed before the engine can read.
//!
//! The write lands in the surface itself, but only once the framework's
//! queue retires it, and nothing in the capsule protocol carries that
//! ordering: torch and MLX pass no `stream` when they import a `kDLMetal`
//! capsule. A write door drains every framework it can see instead.

use pyo3::prelude::*;
use pyo3::types::PyDict;

/// Drain torch-MPS's and MLX's queues, whichever this process imported.
///
/// `torch.mps.synchronize` waits on every MPS stream. MLX's `synchronize`
/// waits on its default stream and evaluates nothing, so an MLX write
/// reaches the surface only once the author evaluated it on that stream.
pub(crate) fn synchronize_the_imported_metal_frameworks(python: Python<'_>) -> PyResult<()> {
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
