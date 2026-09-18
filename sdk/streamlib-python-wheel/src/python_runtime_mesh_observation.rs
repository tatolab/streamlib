// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The door `streamlib nodes` reads the runtime mesh through.
//!
//! Internal, and `_`-prefixed to say so: it exists for the CLI's own table, not
//! as authoring surface. An app that wants its mesh peers reads `graph`, which
//! answers from the session the runtime already holds.
//!
//! Nothing here joins a mesh. The engine opens a session that announces
//! nothing, asks, and closes — so listing a mesh costs no runtime, takes no
//! name, and is invisible to every runtime on it.

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use streamlib::sdk::error::Error;
use streamlib::sdk::runtime::{RuntimeMeshConfiguration, observe_a_runtime_mesh};

/// One runtime seen on a mesh.
#[pyclass(name = "ObservedRuntimeMeshPeer", module = "streamlib", frozen)]
pub(crate) struct PythonObservedRuntimeMeshPeer {
    runtime_name: String,
    runtime_id: Option<String>,
    host_name: Option<String>,
    engine_version: Option<String>,
    control_plane_urls: Option<Vec<String>>,
}

#[pymethods]
impl PythonObservedRuntimeMeshPeer {
    /// The name this runtime is addressed by on the mesh. Always known: it is
    /// on the announcement itself.
    #[getter]
    fn runtime_name(&self) -> &str {
        &self.runtime_name
    }

    /// The runtime's per-run id, or `None` until it answers what it is.
    #[getter]
    fn runtime_id(&self) -> Option<&str> {
        self.runtime_id.as_deref()
    }

    /// What the runtime's host calls itself, or `None` until it answers.
    #[getter]
    fn host_name(&self) -> Option<&str> {
        self.host_name.as_deref()
    }

    /// The engine version the runtime runs, or `None` until it answers.
    #[getter]
    fn engine_version(&self) -> Option<&str> {
        self.engine_version.as_deref()
    }

    /// Where another machine could reach the runtime's control plane. Empty
    /// when it hosts none — on the mesh, and not drivable. `None` until it
    /// answers at all, which is a different thing from hosting none.
    #[getter]
    fn control_plane_urls(&self) -> Option<Vec<String>> {
        self.control_plane_urls.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "ObservedRuntimeMeshPeer(runtime_name={:?}, host_name={:?})",
            self.runtime_name, self.host_name
        )
    }
}

/// One look at one mesh.
#[pyclass(name = "ObservedRuntimeMesh", module = "streamlib", frozen)]
pub(crate) struct PythonObservedRuntimeMesh {
    mesh_name: String,
    peers: Vec<Py<PythonObservedRuntimeMeshPeer>>,
}

#[pymethods]
impl PythonObservedRuntimeMesh {
    /// The mesh that was looked at, resolved — so a caller that named none can
    /// still say which one it read.
    #[getter]
    fn mesh_name(&self) -> &str {
        &self.mesh_name
    }

    /// Every runtime announced on it, sorted by name.
    #[getter]
    fn peers(&self, python: Python<'_>) -> Vec<Py<PythonObservedRuntimeMeshPeer>> {
        self.peers
            .iter()
            .map(|peer| peer.clone_ref(python))
            .collect()
    }

    fn __repr__(&self) -> String {
        format!(
            "ObservedRuntimeMesh(mesh_name={:?}, peers={})",
            self.mesh_name,
            self.peers.len()
        )
    }
}

/// Look at a runtime mesh without joining it.
///
/// The blocking wait runs with the GIL detached: it opens a session, waits out
/// discovery and asks every runtime it finds what it is, which is about a
/// second of network and no Python at all.
#[pyfunction]
#[pyo3(signature = (
    *,
    mesh_name = None,
    mesh_peer_endpoints = None,
    mesh_multicast_discovery = None,
))]
pub(crate) fn _observe_the_runtime_mesh(
    python: Python<'_>,
    mesh_name: Option<String>,
    mesh_peer_endpoints: Option<Vec<String>>,
    mesh_multicast_discovery: Option<bool>,
) -> PyResult<PythonObservedRuntimeMesh> {
    let observed = python
        .detach(|| {
            observe_a_runtime_mesh(RuntimeMeshConfiguration {
                mesh_name,
                mesh_peer_endpoints,
                mesh_multicast_discovery,
                ..Default::default()
            })
        })
        .map_err(|mesh_failure| match mesh_failure {
            // A configuration the caller got wrong — a `quic/` endpoint, a mesh
            // name outside the grammar — is a usage error the CLI reports as
            // one, where a mesh that would not answer leaves the registry table
            // standing.
            refusal @ Error::Configuration(_) => PyValueError::new_err(refusal.to_string()),
            unreachable => PyRuntimeError::new_err(unreachable.to_string()),
        })?;

    Ok(PythonObservedRuntimeMesh {
        mesh_name: observed.mesh_name,
        peers: observed
            .peers
            .into_iter()
            .map(|peer| {
                Py::new(
                    python,
                    PythonObservedRuntimeMeshPeer {
                        runtime_name: peer.runtime_name,
                        runtime_id: peer.runtime_id,
                        host_name: peer.host_name,
                        engine_version: peer.engine_version,
                        control_plane_urls: peer.control_plane_urls,
                    },
                )
            })
            .collect::<PyResult<Vec<_>>>()?,
    })
}
