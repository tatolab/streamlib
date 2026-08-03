// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Making a Python class a processor type the engine can instantiate.
//!
//! Registration is per process and idempotent per identity: `rt.add(Blur)`
//! called twice registers `Blur` once and adds two processors to the graph,
//! each with its own configuration and its own instance of the class.

use std::sync::Mutex;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use streamlib::sdk::descriptors::SchemaIdent;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorTypeReference};

use crate::python_processor_declaration::PythonProcessorDeclaration;
use crate::python_processor_host::PythonProcessorHost;

/// Which Python class each registered identity was registered from.
///
/// The engine's registry is keyed by identity and rejects a second
/// registration, so without this a module reloaded under a different class
/// object would fail with the registry's generic duplicate error rather than
/// the reason.
static REGISTERED_PROCESSOR_CLASSES: Mutex<Vec<(SchemaIdent, Py<PyAny>)>> = Mutex::new(Vec::new());

/// Register `processor_class` if this identity is not already registered.
///
/// Returns the type reference `Runtime.add` names the processor by.
pub(crate) fn register_processor_class(
    python: Python<'_>,
    processor_class: &Bound<'_, PyAny>,
) -> PyResult<ProcessorTypeReference> {
    let declaration = PythonProcessorDeclaration::read_from_class(processor_class)?;
    let identity = declaration.descriptor.name.clone();

    let mut registered = REGISTERED_PROCESSOR_CLASSES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some((_, already_registered)) = registered
        .iter()
        .find(|(registered_identity, _)| *registered_identity == identity)
    {
        return if already_registered.bind(python).is(processor_class) {
            Ok(declaration.type_reference)
        } else {
            Err(PyValueError::new_err(format!(
                "two different classes both claim the processor identity \
                 `@{}/{}/{}`: {} and {}. Give one of them an explicit identity — \
                 `@processor(\"@org/package/Type\")` — so each names itself.",
                declaration.type_reference.org().as_str(),
                declaration.type_reference.package().as_str(),
                declaration.type_reference.r#type().as_str(),
                class_qualified_name(already_registered.bind(python)),
                class_qualified_name(processor_class),
            )))
        };
    }

    let type_reference = declaration.type_reference.clone();
    let descriptor = declaration.descriptor.clone();
    let held_processor_class = processor_class.clone().unbind();
    let constructor_class = held_processor_class.clone_ref(python);

    PROCESSOR_REGISTRY
        .register_dynamic(
            descriptor,
            Box::new(move |node| {
                PythonProcessorHost::construct(&declaration, &constructor_class, node)
                    .map(|host| Box::new(host) as Box<dyn streamlib::sdk::processors::DynGeneratedProcessor + Send>)
            }),
        )
        .map_err(|registration_failure| {
            PyValueError::new_err(registration_failure.to_string())
        })?;

    registered.push((identity, held_processor_class));
    Ok(type_reference)
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
