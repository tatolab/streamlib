// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reading the `@processor` grammar off a Python class.
//!
//! The `__streamlib_processor_*__` attributes the decorator attaches are the
//! contract between `_processor_declaration.py` and this module; the two move
//! together.

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use streamlib::sdk::descriptors::{
    Org, Package, PortDescriptor, ProcessorClassImportPath, ProcessorDescriptor, ProcessorRuntime,
    ProcessorScheduling, SchemaIdent, SemVer, TypeName,
};
use streamlib::sdk::execution::{ExecutionConfig, ProcessExecution, ThreadPriority};

use crate::python_processor_import_path::processor_class_import_path;

/// The version the vestigial `ProcessorDescriptor::name` carries. Nothing keys
/// on that field any more — a processor is named by its class's import path —
/// so this is inert filler for the one struct that still has it.
const VERSION_FREE_SENTINEL: SemVer = SemVer::new(0, 0, 0);

/// Everything the engine needs to register and instantiate one Python
/// processor class.
pub(crate) struct PythonProcessorDeclaration {
    pub(crate) descriptor: ProcessorDescriptor,
    pub(crate) execution_config: ExecutionConfig,
}

impl PythonProcessorDeclaration {
    /// Read the decorator's metadata off `processor_class`.
    pub(crate) fn read_from_class(processor_class: &Bound<'_, PyAny>) -> PyResult<Self> {
        let vestigial_name = read_declared_schema_ident(processor_class)?;
        let execution_config = read_execution_config(processor_class)?;

        // Identity and `entrypoint` are separate contracts, but one
        // derivation: a second call is a second chance for them to disagree.
        let class_import_path = processor_class_import_path(processor_class)?;

        let mut descriptor = ProcessorDescriptor::new(
            vestigial_name,
            ProcessorClassImportPath::new(class_import_path.clone())
                .map_err(|blank| PyValueError::new_err(blank.to_string()))?,
            read_string_attribute(processor_class, "__streamlib_processor_description__")?,
        )
        .with_runtime(ProcessorRuntime::Python)
        .with_entrypoint(class_import_path)
        .with_scheduling(ProcessorScheduling {
            priority: read_thread_priority(processor_class)?,
        });

        descriptor.inputs = read_port_descriptors(processor_class, PortDirection::Input)?;
        descriptor.outputs = read_port_descriptors(processor_class, PortDirection::Output)?;

        Ok(Self {
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

/// The `@org/package/Type` the decorator still records, read only to fill the
/// descriptor's vestigial `name`. Nothing keys on it; the grammar and the field
/// die together.
fn read_declared_schema_ident(processor_class: &Bound<'_, PyAny>) -> PyResult<SchemaIdent> {
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
    Ok(SchemaIdent::new(
        org,
        package,
        type_name,
        VERSION_FREE_SENTINEL,
    ))
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
    use crate::python_class_from_source_for_tests::class_from_source;

    /// A class carrying what `@streamlib.processor` attaches.
    const DECLARED_CLASS_SOURCE: &str = "\
__name__ = 'my_app.filters'


class BlurProcessor:
    __streamlib_processor_type_reference__ = {
        'org': 'app', 'package': 'local', 'type': 'BlurProcessor'
    }
    __streamlib_processor_description__ = 'blurs'
    __streamlib_processor_execution__ = {'mode': 'reactive'}
    __streamlib_processor_scheduling_priority__ = None
    __streamlib_processor_input_ports__ = []
    __streamlib_processor_output_ports__ = []
";

    /// A class that drifted between the two fields would be a processor
    /// registered under a name its own helper process cannot import.
    #[test]
    fn the_identity_and_the_entrypoint_are_the_same_derived_string() {
        Python::initialize();
        Python::attach(|python| {
            let declared_class = class_from_source(python, DECLARED_CLASS_SOURCE, "BlurProcessor");
            let declaration = PythonProcessorDeclaration::read_from_class(&declared_class).unwrap();

            assert_eq!(
                declaration.descriptor.processor_class_import_path.as_str(),
                "my_app.filters:BlurProcessor"
            );
            assert_eq!(
                Some(
                    declaration
                        .descriptor
                        .processor_class_import_path
                        .as_str()
                        .to_string()
                ),
                declaration.descriptor.entrypoint,
            );
        });
    }
}
