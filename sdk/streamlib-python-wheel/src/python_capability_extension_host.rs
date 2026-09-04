// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The door a capability extension's `load(host)` hook is handed.
//!
//! One host per entry point, so a refusal can name the distribution that
//! registered a capability and the one that tried to register it again. The
//! host holds the registry rather than the engine: an extension that keeps its
//! `host` past `load()` must not be able to keep the engine alive past
//! teardown.

use std::sync::{Arc, OnceLock};

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::runtime::{LoadedCapabilityExtension, LoadedCapabilityExtensionRegistry};

use crate::python_runtime_lifecycle::PythonRuntimeHandle;

/// What every hook in this process registers into.
///
/// Process-wide rather than per-runtime because the hooks run once per process
/// and a capability they brought up stays up: a runtime constructed later has
/// it too, and says so. A process takes one role, so this never mixes them.
fn process_wide_registry() -> &'static Arc<LoadedCapabilityExtensionRegistry> {
    static REGISTRY: OnceLock<Arc<LoadedCapabilityExtensionRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Arc::new(LoadedCapabilityExtensionRegistry::default()))
}

/// What a capability extension's `load(host)` hook is handed.
#[pyclass(name = "CapabilityExtensionHost", module = "streamlib", frozen)]
pub(crate) struct PythonCapabilityExtensionHost {
    role: &'static str,
    distribution: String,
    registry: Arc<LoadedCapabilityExtensionRegistry>,
}

impl PythonCapabilityExtensionHost {
    fn for_this_process(role: &'static str, distribution: String) -> Self {
        Self {
            role,
            distribution,
            registry: Arc::clone(process_wide_registry()),
        }
    }
}

#[pymethods]
impl PythonCapabilityExtensionHost {
    /// Which role this process takes — `"app"` or `"helper"`.
    #[getter]
    fn role(&self) -> &'static str {
        self.role
    }

    /// Declare a capability this wheel brought up, under a name no other
    /// installed distribution may take.
    fn register_capability(&self, name: String, version: String) -> PyResult<()> {
        self.registry
            .register(LoadedCapabilityExtension {
                name,
                version,
                distribution: self.distribution.clone(),
            })
            .map_err(|refusal| PyRuntimeError::new_err(refusal.to_string()))
    }

    fn __repr__(&self) -> String {
        format!(
            "CapabilityExtensionHost(role={:?}, distribution={:?})",
            self.role, self.distribution
        )
    }
}

/// Mint the host for `distribution`'s hook in the app process.
#[pyfunction]
pub(crate) fn capability_extension_host_for_the_app_process(
    distribution: String,
) -> PythonCapabilityExtensionHost {
    PythonCapabilityExtensionHost::for_this_process("app", distribution)
}

/// Mint the host for `distribution`'s hook in a helper process.
#[pyfunction]
pub(crate) fn capability_extension_host_for_the_helper_process(
    distribution: String,
) -> PythonCapabilityExtensionHost {
    PythonCapabilityExtensionHost::for_this_process("helper", distribution)
}

/// Give `runtime` what this process's hooks registered, so `graph` renders it.
///
/// Called once per runtime, after the loop: the hooks run once per process,
/// so a runtime constructed after them would otherwise report nothing loaded.
#[pyfunction]
pub(crate) fn hand_loaded_capability_extensions_to_the_runtime(
    runtime: &PythonRuntimeHandle,
) -> PyResult<()> {
    runtime
        .loaded_capability_extensions()?
        .adopt(process_wide_registry().registered())
        .map_err(|refusal| PyRuntimeError::new_err(refusal.to_string()))
}
