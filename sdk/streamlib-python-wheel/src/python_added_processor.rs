// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What `Runtime.add` hands back, and what `Runtime.connect` takes.
//!
//! Ports are named through the processor they belong to
//! (`camera.output("frames_to_downstream")`) so a link always reads as an
//! endpoint of something, never as a bare string pair.

use pyo3::prelude::*;
use streamlib::sdk::graph::{MeshPortAddress, OutputLinkPortRef};

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

/// Either end a link may carry from: a port on this runtime, or one on
/// another runtime over the mesh.
///
/// `connect` takes this rather than two overloads because the engine's own
/// `OutputLinkPortRef` is the same two shapes — the Python surface mirrors the
/// engine's type rather than inventing a parallel one.
#[derive(FromPyObject)]
pub(crate) enum PythonLinkSourcePortReference<'py> {
    OnThisRuntime(PyRef<'py, PythonProcessorOutputPortReference>),
    OnAnotherRuntime(PyRef<'py, PythonRemoteProcessorOutputPortReference>),
}

impl PythonLinkSourcePortReference<'_> {
    /// The engine's own reference for whichever end this names.
    pub(crate) fn as_an_output_link_port_ref(&self) -> OutputLinkPortRef {
        match self {
            Self::OnThisRuntime(local) => {
                OutputLinkPortRef::new(local.processor_id.clone(), local.port_name.clone())
            }
            Self::OnAnotherRuntime(remote) => {
                OutputLinkPortRef::on_another_runtime(remote.address.clone())
            }
        }
    }
}
