// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Making a Python class a processor type the engine can instantiate.
//!
//! Registration arrives in two halves. `@processor` registers the descriptor
//! when it runs, so the class is in the catalog an agent reads before anything
//! adds it; the first add installs the constructor onto that descriptor.
//! Registration is per process and idempotent per identity: `rt.add(Blur)`
//! called twice installs `Blur`'s constructor once and adds two processors to
//! the graph, each with its own configuration and its own instance of the class.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib::sdk::processors::PROCESSOR_REGISTRY;

use crate::python_helper_process_spawn_host::{
    HELPER_PROCESS_ENTRYPOINT_ENVIRONMENT_VARIABLE, spawn_host_for_processor_node,
};
use crate::python_processor_declaration::PythonProcessorDeclaration;
use crate::python_processor_import_path::processor_class_import_path;

/// Which Python class each import path had its constructor installed from.
///
/// A cache of *which class*, never the authority on *whether* a type can be
/// constructed — that stays the engine's registry, consulted below, so this can
/// never report an install the engine does not actually hold.
fn registered_processor_classes() -> &'static Mutex<HashMap<ProcessorClassImportPath, Py<PyAny>>> {
    static REGISTERED_PROCESSOR_CLASSES: OnceLock<
        Mutex<HashMap<ProcessorClassImportPath, Py<PyAny>>>,
    > = OnceLock::new();
    REGISTERED_PROCESSOR_CLASSES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Give the descriptor `processor_class` registered at decoration the
/// constructor that spawns its helper process, unless it already has one.
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
            // An import path is `__module__` + `__qualname__`, so two classes
            // reach the same one only by being the same declaration executed
            // twice — the module was loaded again and rebuilt its classes. Two
            // *differently named* classes can no longer collide at all, so
            // there is nothing to declare that would tell these apart; the fix
            // is upstream, at the reload.
            Err(PyValueError::new_err(format!(
                "two different class objects both identify as `{identity}`: {} and {}. One \
                 import path names one class, so these are the same declaration loaded twice \
                 — `importlib.reload` is the usual cause, and the class object you are adding \
                 is not the one already registered. Add the class from the module the \
                 interpreter currently holds, or restart the app.",
                class_qualified_name(already_registered.bind(python)),
                class_qualified_name(processor_class),
            )))
        };
    }

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
        .install_constructor_for_registered_descriptor(
            &identity,
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
        .map_err(|install_failure| PyValueError::new_err(install_failure.to_string()))?;

    registered.insert(identity.clone(), held_processor_class);
    Ok(identity)
}

/// Register the descriptor `@processor` has just stamped onto
/// `processor_class`, so the class is in the catalog before anything adds it.
///
/// The decorator's one call into the native half. Registers the descriptor
/// alone: the constructor is the first add's to supply, through
/// [`register_processor_class`].
///
/// Two classes are passed over rather than registered. One decorated inside a
/// helper process registers nothing, because a helper hosts no graph. One no
/// interpreter could import — declared in the entry file or inside a function
/// — has no identity to be registered under, and `rt.add` is where that is
/// said, with the fix named.
#[pyfunction]
pub(crate) fn register_declared_processor_class(
    processor_class: &Bound<'_, PyAny>,
) -> PyResult<()> {
    if std::env::var_os(HELPER_PROCESS_ENTRYPOINT_ENVIRONMENT_VARIABLE).is_some() {
        tracing::debug!(
            "[register_declared_processor_class] a decoration inside a helper process registers \
             nothing"
        );
        return Ok(());
    }
    // Only an unresolvable identity is passed over, and deliberately narrowly:
    // every other refusal `read_from_class` raises — a malformed port, an
    // unreadable config schema — is the author's to see at decoration, so this
    // guard asks the one question rather than swallowing the whole read.
    if let Err(no_import_path) = processor_class_import_path(processor_class) {
        tracing::debug!(
            %no_import_path,
            "[register_declared_processor_class] a class with no import path registers nothing"
        );
        return Ok(());
    }
    let declaration = PythonProcessorDeclaration::read_from_class(processor_class)?;
    PROCESSOR_REGISTRY
        .register_descriptor_only(declaration.descriptor)
        .map_err(|registration_failure| PyValueError::new_err(registration_failure.to_string()))
}

/// Every processor class import path in the calling process's catalog.
///
/// What `/api/registry` renders, reachable in a process that serves no control
/// plane — which a helper is, and is the only way to see from inside one that
/// decoration registered nothing there. Named for the catalog rather than for
/// registration: a path listed here may be one the engine's `is_registered`
/// calls false, because that asks whether a constructor has arrived.
#[pyfunction]
pub(crate) fn processor_class_import_paths_in_this_processes_catalog() -> Vec<String> {
    PROCESSOR_REGISTRY
        .registered_processor_class_import_paths()
        .into_iter()
        .map(|import_path| import_path.as_str().to_string())
        .collect()
}

/// Register the class `processor_class_import_path` names by importing it into
/// this interpreter — the registration import `rt.add` performs, done for a
/// caller that holds only the path, such as an `add_processor` over the control
/// plane. The processor itself still runs in its own helper process.
///
/// A path in another grammar — a Rust `crate::module::Type`, or one with no
/// module at all — names nothing this interpreter could import, so it is left
/// untouched for the registry to report as unknown.
pub(crate) fn register_processor_class_by_import_path(
    python: Python<'_>,
    processor_class_import_path: &ProcessorClassImportPath,
) -> PyResult<()> {
    let path = processor_class_import_path.as_str();
    let Some((module_name, qualname)) = path.split_once(':') else {
        return Ok(());
    };
    if path.contains("::") || module_name.is_empty() || qualname.is_empty() {
        return Ok(());
    }
    let importlib = python.import("importlib")?;
    // A module written to disk after this interpreter started is invisible to
    // the import system's directory caches until they are invalidated.
    importlib.call_method0("invalidate_caches")?;
    let mut processor_class = importlib.call_method1("import_module", (module_name,))?;
    for attribute in qualname.split('.') {
        processor_class = processor_class.getattr(attribute)?;
    }
    let registered = register_processor_class(python, &processor_class)?;
    if registered != *processor_class_import_path {
        return Err(PyValueError::new_err(format!(
            "`{path}` resolved to a class identifying as `{}`; add it by that path",
            registered.as_str()
        )));
    }
    Ok(())
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
