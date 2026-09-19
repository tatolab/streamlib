// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What `Runtime.add` hands back, and what `Runtime.connect` takes.
//!
//! Ports are named through the processor they belong to
//! (`camera.output("frames_to_downstream")`) so a link always reads as an
//! endpoint of something, never as a bare string pair.

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use streamlib::sdk::graph::{InputLinkPortRef, MeshPortAddress, OutputLinkPortRef};

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
    fn output(&self, port_name: &str) -> PythonProcessorOutputPortReference {
        PythonProcessorOutputPortReference {
            processor_id: self.processor_id.clone(),
            port_name: port_name.to_string(),
        }
    }

    /// Name one of this processor's input ports, to connect it upstream.
    fn input(&self, port_name: &str) -> PythonProcessorInputPortReference {
        PythonProcessorInputPortReference {
            processor_id: self.processor_id.clone(),
            port_name: port_name.to_string(),
        }
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

/// The producing end of a link, on another runtime.
///
/// Holds the address the mesh checked at the mint, so `connect` never has to
/// re-check it and an illegal chunk is refused where the author typed it.
#[pyclass(
    name = "RemoteProcessorOutputPortReference",
    module = "streamlib",
    frozen
)]
pub(crate) struct PythonRemoteProcessorOutputPortReference {
    pub(crate) address: MeshPortAddress,
}

#[pymethods]
impl PythonRemoteProcessorOutputPortReference {
    fn __repr__(&self) -> String {
        format!("RemoteProcessorOutputPortReference({})", self.address)
    }
}

/// The consuming end of a link, on another runtime.
///
/// Holds the address the mesh checked at the mint, so `connect` never has to
/// re-check it and an illegal chunk is refused where the author typed it.
#[pyclass(
    name = "RemoteProcessorInputPortReference",
    module = "streamlib",
    frozen
)]
pub(crate) struct PythonRemoteProcessorInputPortReference {
    pub(crate) address: MeshPortAddress,
}

#[pymethods]
impl PythonRemoteProcessorInputPortReference {
    fn __repr__(&self) -> String {
        format!("RemoteProcessorInputPortReference({})", self.address)
    }
}

/// The engine's own reference for whichever end `connect`'s source names: a
/// port on this runtime, or one on another runtime over the mesh.
///
/// Reads straight into `OutputLinkPortRef`, which is already those two shapes,
/// rather than through a Python-side enum that would shadow it. Hand-written
/// rather than `#[derive(FromPyObject)]` for the refusal: the derive's names
/// the Rust variants it tried, which a Python author has no way to act on,
/// where this names the two spellings that would have worked.
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
    if let Ok(on_another_runtime) = source.cast::<PythonRemoteProcessorOutputPortReference>() {
        return Ok(OutputLinkPortRef::on_another_runtime(
            on_another_runtime.borrow().address.clone(),
        ));
    }
    Err(PyTypeError::new_err(format!(
        "connect's source must name an output port: `processor.output(port_name)` for a \
         port on this runtime, or `runtime.remote_processor_output(runtime_name, \
         display_name, port_name)` for one on another runtime. Got {}.",
        source.get_type()
    )))
}

/// The engine's own reference for whichever end `connect`'s destination names.
///
/// The mirror of [`the_output_link_port_ref_this_source_names`], hand-written
/// for the same reason: a derive's refusal names the Rust variants it tried,
/// which a Python author has no way to act on.
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
    if let Ok(on_another_runtime) = destination.cast::<PythonRemoteProcessorInputPortReference>() {
        return Ok(InputLinkPortRef::on_another_runtime(
            on_another_runtime.borrow().address.clone(),
        ));
    }
    Err(PyTypeError::new_err(format!(
        "connect's destination must name an input port: `processor.input(port_name)` for a \
         port on this runtime, or `runtime.remote_processor_input(runtime_name, \
         display_name, port_name)` for one on another runtime. Got {}.",
        destination.get_type()
    )))
}
