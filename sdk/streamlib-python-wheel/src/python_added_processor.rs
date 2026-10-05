// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What `Runtime.add` hands back, and what `Runtime.connect` takes.
//!
//! Ports are named through the processor they belong to
//! (`camera.output("frames_to_downstream")`) so a link always reads as an
//! endpoint of something, never as a bare string pair.

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};

use crate::python_processor_link_data_access::declared_port_name_the_spelling_names;

/// A processor in the graph.
#[pyclass(name = "AddedProcessor", module = "streamlib", frozen)]
pub(crate) struct PythonAddedProcessor {
    processor_id: String,
    display_name: String,
}

impl PythonAddedProcessor {
    pub(crate) fn new(processor_id: String, display_name: String) -> Self {
        Self {
            processor_id,
            display_name,
        }
    }
}

#[pymethods]
impl PythonAddedProcessor {
    /// The engine's id for this processor — what `streamlib graph` shows.
    #[getter]
    fn processor_id(&self) -> &str {
        &self.processor_id
    }

    /// The processor's display name in the graph.
    #[getter]
    fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Name one of this processor's output ports, to connect it downstream.
    fn output(&self, port_name: &str) -> PyResult<PythonProcessorOutputPortReference> {
        Ok(PythonProcessorOutputPortReference {
            processor_id: self.processor_id.clone(),
            port_name: declared_port_name_the_spelling_names(port_name)?.into_owned(),
        })
    }

    /// Name one of this processor's input ports, to connect it upstream.
    fn input(&self, port_name: &str) -> PyResult<PythonProcessorInputPortReference> {
        Ok(PythonProcessorInputPortReference {
            processor_id: self.processor_id.clone(),
            port_name: declared_port_name_the_spelling_names(port_name)?.into_owned(),
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "AddedProcessor(display_name={:?}, processor_id={:?})",
            self.display_name, self.processor_id
        )
    }
}

/// The producing end of a link.
#[pyclass(name = "ProcessorOutputPortReference", module = "streamlib", frozen)]
pub(crate) struct PythonProcessorOutputPortReference {
    pub(crate) processor_id: String,
    pub(crate) port_name: String,
}

#[pymethods]
impl PythonProcessorOutputPortReference {
    fn __repr__(&self) -> String {
        format!(
            "ProcessorOutputPortReference({}.{})",
            self.processor_id, self.port_name
        )
    }
}

/// The consuming end of a link.
#[pyclass(name = "ProcessorInputPortReference", module = "streamlib", frozen)]
pub(crate) struct PythonProcessorInputPortReference {
    pub(crate) processor_id: String,
    pub(crate) port_name: String,
}

#[pymethods]
impl PythonProcessorInputPortReference {
    fn __repr__(&self) -> String {
        format!(
            "ProcessorInputPortReference({}.{})",
            self.processor_id, self.port_name
        )
    }
}

/// The engine's own reference for the output port `connect`'s source names,
/// which is always on this runtime.
pub(crate) fn the_output_link_port_ref_this_source_names(
    source: &Bound<'_, PyAny>,
) -> PyResult<OutputLinkPortRef> {
    if let Ok(on_this_runtime) = source.cast::<PythonProcessorOutputPortReference>() {
        let on_this_runtime = on_this_runtime.borrow();
        return Ok(OutputLinkPortRef::new(
            on_this_runtime.processor_id.clone(),
            on_this_runtime.port_name.clone(),
        ));
    }
    Err(PyTypeError::new_err(format!(
        "connect's source must name an output port on this runtime: \
         `processor.output(port_name)`. Got {}.",
        source.get_type()
    )))
}

/// The engine's own reference for the input port `connect`'s destination
/// names, which is always on this runtime.
pub(crate) fn the_input_link_port_ref_this_destination_names(
    destination: &Bound<'_, PyAny>,
) -> PyResult<InputLinkPortRef> {
    if let Ok(on_this_runtime) = destination.cast::<PythonProcessorInputPortReference>() {
        let on_this_runtime = on_this_runtime.borrow();
        return Ok(InputLinkPortRef::new(
            on_this_runtime.processor_id.clone(),
            on_this_runtime.port_name.clone(),
        ));
    }
    Err(PyTypeError::new_err(format!(
        "connect's destination must name an input port on this runtime: \
         `processor.input(port_name)`. Got {}.",
        destination.get_type()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_reference_carries_the_cast_port_name() {
        let added_processor =
            PythonAddedProcessor::new("processor-id".to_string(), "Camera".to_string());

        assert_eq!(added_processor.output("Video").unwrap().port_name, "video");
        assert_eq!(
            added_processor
                .input("Frames From Upstream")
                .unwrap()
                .port_name,
            "frames-from-upstream"
        );
        assert!(added_processor.output("..").is_err());
    }
}
