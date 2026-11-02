// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reading the module host table out of the capsule a node's context hands its
//! Python half.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyCapsule, PyCapsuleMethods};

use crate::module_host::{ModuleHost, ModuleHostError};
use crate::module_host_table_abi::{MODULE_HOST_TABLE_PYTHON_CAPSULE_NAME, ModuleHostTable};

impl ModuleHost {
    /// The node host in the capsule `ctx.open_module_host_table(...)` returned,
    /// refused by name when it is anything else or a table this module cannot
    /// read.
    pub fn from_python_module_host_table_capsule(
        capsule: &Bound<'_, PyAny>,
    ) -> Result<Self, ModuleHostError> {
        let not_the_capsule = |refusal: String| ModuleHostError::NotAModuleHostTableCapsule {
            refusal: format!(
                "a module host table is the capsule `ctx.open_module_host_table(...)` returns \
                 ({}); {refusal}",
                MODULE_HOST_TABLE_PYTHON_CAPSULE_NAME.to_string_lossy()
            ),
        };
        let capsule = capsule.cast::<PyCapsule>().map_err(|_| {
            not_the_capsule(format!(
                "got a {}",
                capsule
                    .get_type()
                    .name()
                    .map(|type_name| type_name.to_string())
                    .unwrap_or_default()
            ))
        })?;
        let module_host_table = capsule
            .pointer_checked(Some(MODULE_HOST_TABLE_PYTHON_CAPSULE_NAME))
            .map_err(|wrong_name| not_the_capsule(wrong_name.to_string()))?;
        // SAFETY: a capsule carrying this name points at a module host table
        // whose node host the capsule keeps alive while it is referenced.
        unsafe {
            ModuleHost::from_module_host_table_pointer(
                module_host_table.as_ptr().cast::<ModuleHostTable>(),
            )
        }
    }
}

impl From<ModuleHostError> for PyErr {
    fn from(module_host_error: ModuleHostError) -> Self {
        PyRuntimeError::new_err(module_host_error.to_string())
    }
}
