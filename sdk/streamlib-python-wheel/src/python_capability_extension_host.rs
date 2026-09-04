// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The door a capability extension's `load(host)` hook is handed.
//!
//! One host per entry point, so a refusal can name the distribution that
//! registered a capability and the one that tried to register it again. The
//! host holds the process's registry, never the engine: an extension that
//! keeps its `host` past `load()` must not be able to keep the engine alive
//! past teardown.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::runtime::{LoadedCapabilityExtension, LoadedCapabilityExtensionRegistry};

/// Which role the process running a hook has taken.
#[derive(Debug, Clone, Copy)]
enum EngineProcessRole {
    AppProcess,
    HelperProcess,
}

impl EngineProcessRole {
    /// What `host.role` reads as. Pinned by the stub's `Literal["app", "helper"]`.
    fn as_python_role_name(self) -> &'static str {
        match self {
            Self::AppProcess => "app",
            Self::HelperProcess => "helper",
        }
    }
}

/// What a capability extension's `load(host)` hook is handed.
#[pyclass(name = "CapabilityExtensionHost", module = "streamlib", frozen)]
pub(crate) struct PythonCapabilityExtensionHost {
    role: EngineProcessRole,
    distribution: String,
    loaded_capability_extension_registry: &'static LoadedCapabilityExtensionRegistry,
}

impl PythonCapabilityExtensionHost {
    fn for_this_process(role: EngineProcessRole, distribution: String) -> Self {
        Self {
            role,
            distribution,
            loaded_capability_extension_registry:
                LoadedCapabilityExtensionRegistry::of_this_process(),
        }
    }
}

#[pymethods]
impl PythonCapabilityExtensionHost {
    /// Which role this process takes — `"app"` or `"helper"`.
    #[getter]
    fn role(&self) -> &'static str {
        self.role.as_python_role_name()
    }

    /// Declare a capability this wheel brought up, under a name no other
    /// installed distribution may take.
    fn register_capability(&self, name: String, version: String) -> PyResult<()> {
        self.loaded_capability_extension_registry
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
            self.role.as_python_role_name(),
            self.distribution
        )
    }
}

/// Mint the host for `distribution`'s hook in the app process.
#[pyfunction]
pub(crate) fn capability_extension_host_for_the_app_process(
    distribution: String,
) -> PythonCapabilityExtensionHost {
    PythonCapabilityExtensionHost::for_this_process(EngineProcessRole::AppProcess, distribution)
}

/// Mint the host for `distribution`'s hook in a helper process.
#[pyfunction]
pub(crate) fn capability_extension_host_for_the_helper_process(
    distribution: String,
) -> PythonCapabilityExtensionHost {
    PythonCapabilityExtensionHost::for_this_process(EngineProcessRole::HelperProcess, distribution)
}
