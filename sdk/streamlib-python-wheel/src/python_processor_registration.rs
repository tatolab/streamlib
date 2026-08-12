// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Making a Python class a processor type the engine can instantiate.
//!
//! Registration is per process and idempotent per identity: `rt.add(Blur)`
//! called twice registers `Blur` once and adds two processors to the graph,
//! each with its own configuration and its own instance of the class.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib::sdk::processors::PROCESSOR_REGISTRY;

use crate::python_helper_process_spawn_host::spawn_host_for_processor_node;
use crate::python_processor_declaration::PythonProcessorDeclaration;

/// Which Python class each registered import path was registered from.
///
/// A cache of *which class*, never the authority on *whether* a type is
/// registered — that stays the engine's registry, consulted below, so this can
/// never suppress a re-registration the engine actually needs.
fn registered_processor_classes()
-> &'static Mutex<HashMap<ProcessorClassImportPath, Py<PyAny>>> {
    static REGISTERED_PROCESSOR_CLASSES: OnceLock<
        Mutex<HashMap<ProcessorClassImportPath, Py<PyAny>>>,
    > = OnceLock::new();
    REGISTERED_PROCESSOR_CLASSES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register `processor_class` if its import path is not already registered.
///
/// Returns the class import path `Runtime.add` names the processor by.
pub(crate) fn register_processor_class(
    python: Python<'_>,
    processor_class: &Bound<'_, PyAny>,
) -> PyResult<ProcessorClassImportPath> {
    let declaration = PythonProcessorDeclaration::read_from_class(processor_class)?;
    let identity = declaration.descriptor.processor_class_import_path.clone();

    // Held across the check and the registration, so two threads adding the
    // same class cannot both get past the engine registry's non-atomic
    // read-then-write.
    let mut registered = registered_processor_classes()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some(already_registered) = registered
        .get(&identity)
        .filter(|_| PROCESSOR_REGISTRY.is_registered(&identity))
    {
        return if already_registered.bind(python).is(processor_class) {
            Ok(identity)
        } else {
            // An import path is derived from `__module__` + `__qualname__`, so
            // two classes reach the same one only when the module reached
            // `sys.modules` twice under different names — a re-import, not a
            // naming collision. There is nothing to declare that would tell
            // them apart; the fix is upstream, at the import.
            Err(PyValueError::new_err(format!(
                "two different classes both identify as `{identity}`: {} and {}. One import \
                 path names one class, so this means the module was imported twice under \
                 different names and `sys.modules` holds two copies of it — import it by one \
                 name (a package-relative import and a path-based one reach the same file as \
                 two modules).",
                class_qualified_name(already_registered.bind(python)),
                class_qualified_name(processor_class),
            )))
        };
    }

    let descriptor = declaration.descriptor.clone();
    let held_processor_class = processor_class.clone().unbind();

    // The closure captures the class's import path, never the class object:
    // the object lives in this interpreter, and the processor does not. Every
    // instance the engine constructs is a child that imports the class for
    // itself, which is the same string `rt.add` already refused an
    // unimportable class by.
    let processor_class_import_path = declaration
        .descriptor
        .entrypoint
        .clone()
        .ok_or_else(|| PyValueError::new_err("a Python processor must carry an import path"))?;
    let child_execution_config = declaration.execution_config;
    let descriptor_for_constructor = declaration.descriptor.clone();

    PROCESSOR_REGISTRY
        .register_dynamic(
            descriptor,
            Box::new(move |node| {
                spawn_host_for_processor_node(
                    &processor_class_import_path,
                    &descriptor_for_constructor,
                    child_execution_config,
                    node,
                )
                .map(|spawn_host| {
                    Box::new(spawn_host)
                        as Box<dyn streamlib::sdk::processors::DynGeneratedProcessor + Send>
                })
            }),
        )
        .map_err(|registration_failure| PyValueError::new_err(registration_failure.to_string()))?;

    registered.insert(identity.clone(), held_processor_class);
    Ok(identity)
}

fn class_qualified_name(processor_class: &Bound<'_, PyAny>) -> String {
    let module = processor_class
        .getattr("__module__")
        .and_then(|module| module.extract::<String>())
        .unwrap_or_else(|_| "<unknown module>".to_string());
    let name = processor_class
        .getattr("__qualname__")
        .and_then(|name| name.extract::<String>())
        .unwrap_or_else(|_| "<unknown class>".to_string());
    format!("{module}.{name}")
}
