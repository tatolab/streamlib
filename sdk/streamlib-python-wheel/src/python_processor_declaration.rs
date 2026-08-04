// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reading the `@processor` grammar off a Python class.
//!
//! The `__streamlib_processor_*__` attributes the decorator attaches are the
//! contract between `_processor_declaration.py` and this module; the two move
//! together.

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use streamlib::sdk::descriptors::{
    Org, Package, PortDescriptor, PortSchemaSpec, ProcessorDescriptor, ProcessorRuntime,
    ProcessorScheduling, SchemaIdent, SemVer, TypeName,
};
use streamlib::sdk::execution::{ExecutionConfig, ProcessExecution, ThreadPriority};
use streamlib::sdk::processors::ProcessorTypeReference;

/// The version every code-declared identity carries. A processor reference is
/// version-free — the engine resolves types version-blind — so this is inert
/// filler for the one struct that still has the field.
const VERSION_FREE_SENTINEL: SemVer = SemVer::new(0, 0, 0);

/// Everything the engine needs to register and instantiate one Python
/// processor class.
pub(crate) struct PythonProcessorDeclaration {
    pub(crate) type_reference: ProcessorTypeReference,
    pub(crate) descriptor: ProcessorDescriptor,
    pub(crate) execution_config: ExecutionConfig,
}

impl PythonProcessorDeclaration {
    /// Read the decorator's metadata off `processor_class`.
    pub(crate) fn read_from_class(processor_class: &Bound<'_, PyAny>) -> PyResult<Self> {
        let type_reference = read_type_reference(processor_class)?;
        let execution_config = read_execution_config(processor_class)?;

        let mut descriptor = ProcessorDescriptor::new(
            SchemaIdent::new(
                type_reference.org().clone(),
                type_reference.package().clone(),
                type_reference.r#type().clone(),
                VERSION_FREE_SENTINEL,
            ),
            read_string_attribute(processor_class, "__streamlib_processor_description__")?,
        )
        // Python here means "authored in Python and hosted in this
        // interpreter", not the retired subprocess placement.
        .with_runtime(ProcessorRuntime::Python)
        .with_scheduling(ProcessorScheduling {
            priority: read_thread_priority(processor_class)?,
        });

        descriptor.inputs = read_port_descriptors(processor_class, PortDirection::Input)?;
        descriptor.outputs = read_port_descriptors(processor_class, PortDirection::Output)?;

        Ok(Self {
            type_reference,
            descriptor,
            execution_config,
        })
    }
}

/// Whether a Python class carries the decorator's metadata at all.
pub(crate) fn is_declared_processor_class(candidate: &Bound<'_, PyAny>) -> bool {
    candidate.is_instance_of::<pyo3::types::PyType>()
        && candidate
            .hasattr("__streamlib_processor_type_reference__")
            .unwrap_or(false)
}

fn read_type_reference(processor_class: &Bound<'_, PyAny>) -> PyResult<ProcessorTypeReference> {
    let reference = processor_class
        .getattr("__streamlib_processor_type_reference__")?
        .cast_into::<PyDict>()
        .map_err(|_| {
            PyTypeError::new_err(
                "__streamlib_processor_type_reference__ must be a dict — the class was not \
                 declared by @streamlib.processor",
            )
        })?;

    let org = Org::new(read_dict_string(&reference, "org")?).map_err(ident_error)?;
    let package = Package::new(read_dict_string(&reference, "package")?).map_err(ident_error)?;
    let type_name = TypeName::new(read_dict_string(&reference, "type")?).map_err(ident_error)?;
    Ok(ProcessorTypeReference::new(org, package, type_name))
}

fn read_execution_config(processor_class: &Bound<'_, PyAny>) -> PyResult<ExecutionConfig> {
    let execution = processor_class
        .getattr("__streamlib_processor_execution__")?
        .cast_into::<PyDict>()
        .map_err(|_| PyTypeError::new_err("__streamlib_processor_execution__ must be a dict"))?;

    let mode = read_dict_string(&execution, "mode")?;
    let execution = match mode.as_str() {
        "reactive" => ProcessExecution::Reactive,
        "manual" => ProcessExecution::Manual,
        "continuous" => ProcessExecution::Continuous {
            interval_ms: execution.get_item("interval_ms")?.map_or(Ok(0), |value| {
                value.extract::<u32>().map_err(|_| {
                    PyTypeError::new_err(
                        "__streamlib_processor_execution__.interval_ms must be an int",
                    )
                })
            })?,
        },
        unknown => {
            return Err(PyTypeError::new_err(format!(
                "unknown execution mode {unknown:?} — the decorator validates this, so a class \
                 reaching here was built by hand rather than by @streamlib.processor"
            )));
        }
    };
    Ok(ExecutionConfig::new(execution))
}

fn read_thread_priority(processor_class: &Bound<'_, PyAny>) -> PyResult<ThreadPriority> {
    let priority = processor_class.getattr("__streamlib_processor_scheduling_priority__")?;
    if priority.is_none() {
        return Ok(ThreadPriority::Normal);
    }
    match priority.extract::<String>()?.as_str() {
        "realtime" => Ok(ThreadPriority::RealTime),
        "high" => Ok(ThreadPriority::High),
        "normal" => Ok(ThreadPriority::Normal),
        unknown => Err(PyTypeError::new_err(format!(
            "unknown scheduling priority {unknown:?}"
        ))),
    }
}

#[derive(Clone, Copy)]
enum PortDirection {
    Input,
    Output,
}

impl PortDirection {
    fn class_attribute(self) -> &'static str {
        match self {
            Self::Input => "__streamlib_processor_input_ports__",
            Self::Output => "__streamlib_processor_output_ports__",
        }
    }
}

fn read_port_descriptors(
    processor_class: &Bound<'_, PyAny>,
    direction: PortDirection,
) -> PyResult<Vec<PortDescriptor>> {
    let attribute = direction.class_attribute();
    let declared = processor_class
        .getattr(attribute)?
        .cast_into::<PyList>()
        .map_err(|_| PyTypeError::new_err(format!("{attribute} must be a list")))?;

    let mut ports = Vec::with_capacity(declared.len());
    for declaration in declared.iter() {
        let declaration = declaration
            .cast_into::<PyDict>()
            .map_err(|_| PyTypeError::new_err(format!("{attribute} must hold dicts")))?;

        let mut port = PortDescriptor::iceoryx2(
            read_dict_string(&declaration, "name")?,
            read_dict_string(&declaration, "description")?,
            port_schema_spec_from_declaration(&declaration)?,
        );
        if let Some(delivery_profile) = declaration
            .get_item("delivery_profile")?
            .filter(|declared| !declared.is_none())
        {
            port = port.with_delivery_profile(delivery_profile.extract::<String>()?);
        }
        ports.push(port);
    }
    Ok(ports)
}

/// The port's declared schema: absent or `None` means a wildcard — the wire
/// is self-describing and consuming is a cast at read time — while a typed
/// declaration names the schema for the engine to agree on.
fn port_schema_spec_from_declaration(declaration: &Bound<'_, PyDict>) -> PyResult<PortSchemaSpec> {
    let Some(schema) = declaration
        .get_item("schema")?
        .filter(|declared| !declared.is_none())
    else {
        return Ok(PortSchemaSpec::Any);
    };
    let schema = schema.cast_into::<PyDict>().map_err(|_| {
        PyTypeError::new_err(
            "a port's \"schema\" must be a dict with org, package, type and version keys",
        )
    })?;
    let org = Org::new(read_dict_string(&schema, "org")?).map_err(ident_error)?;
    let package = Package::new(read_dict_string(&schema, "package")?).map_err(ident_error)?;
    let type_name = TypeName::new(read_dict_string(&schema, "type")?).map_err(ident_error)?;
    let version = read_dict_string(&schema, "version")?
        .parse::<SemVer>()
        .map_err(ident_error)?;
    Ok(PortSchemaSpec::Specific(SchemaIdent::new(
        org, package, type_name, version,
    )))
}

fn read_string_attribute(object: &Bound<'_, PyAny>, attribute: &str) -> PyResult<String> {
    object.getattr(attribute)?.extract::<String>()
}

fn read_dict_string(dictionary: &Bound<'_, PyDict>, key: &str) -> PyResult<String> {
    dictionary
        .get_item(key)?
        .ok_or_else(|| PyTypeError::new_err(format!("missing key {key:?}")))?
        .extract::<String>()
}

fn ident_error(failure: impl std::fmt::Display) -> PyErr {
    PyTypeError::new_err(failure.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tolerant reading: no `"schema"` key and an explicit `None` both
    /// mean wildcard, so pre-schema Python declarations keep working.
    #[test]
    fn a_port_without_a_schema_declaration_is_a_wildcard() {
        Python::initialize();
        Python::attach(|python| {
            let declaration = PyDict::new(python);
            assert!(matches!(
                port_schema_spec_from_declaration(&declaration).unwrap(),
                PortSchemaSpec::Any
            ));

            declaration.set_item("schema", python.None()).unwrap();
            assert!(matches!(
                port_schema_spec_from_declaration(&declaration).unwrap(),
                PortSchemaSpec::Any
            ));
        });
    }

    #[test]
    fn a_four_key_schema_dict_becomes_a_specific_ident() {
        Python::initialize();
        Python::attach(|python| {
            let schema = PyDict::new(python);
            schema.set_item("org", "tatolab").unwrap();
            schema.set_item("package", "video").unwrap();
            schema.set_item("type", "VideoFrame").unwrap();
            schema.set_item("version", "1.2.3").unwrap();
            let declaration = PyDict::new(python);
            declaration.set_item("schema", schema).unwrap();

            let spec = port_schema_spec_from_declaration(&declaration).unwrap();
            let PortSchemaSpec::Specific(ident) = spec else {
                panic!("expected Specific, got {spec:?}");
            };
            assert_eq!(
                ident,
                SchemaIdent::new(
                    Org::new("tatolab").unwrap(),
                    Package::new("video").unwrap(),
                    TypeName::new("VideoFrame").unwrap(),
                    SemVer::new(1, 2, 3),
                )
            );
        });
    }

    #[test]
    fn a_malformed_schema_declaration_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let missing_version = PyDict::new(python);
            missing_version.set_item("org", "tatolab").unwrap();
            missing_version.set_item("package", "video").unwrap();
            missing_version.set_item("type", "VideoFrame").unwrap();
            let declaration = PyDict::new(python);
            declaration.set_item("schema", &missing_version).unwrap();
            assert!(port_schema_spec_from_declaration(&declaration).is_err());

            missing_version
                .set_item("version", "not-a-version")
                .unwrap();
            assert!(port_schema_spec_from_declaration(&declaration).is_err());

            let declaration = PyDict::new(python);
            declaration.set_item("schema", "VideoFrame").unwrap();
            assert!(port_schema_spec_from_declaration(&declaration).is_err());
        });
    }
}
