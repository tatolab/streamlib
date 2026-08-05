// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Standing the one control plane up inside the app's own interpreter.
//!
//! The boot recipe itself lives in [`streamlib_api_server::control_plane_host`];
//! this is the Python-facing half, so a wheel-hosted node registers and is
//! driven by `nodes` / `graph` / `tap` exactly as any other host's node is.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::runtime::Runner;
use streamlib_api_server::control_plane_host::{
    ApiServerControlPlaneHostConfig, register_api_server_control_plane_processor_on_runtime,
};

/// Add the control-plane processor to `engine`, with the GIL released.
///
/// Registration touches the process-global processor registry and the engine's
/// graph, neither of which needs the interpreter.
pub(crate) fn host_control_plane_on_engine(
    python: Python<'_>,
    engine: &Runner,
    bind_host: String,
    bind_port: u16,
    node_name: Option<String>,
) -> PyResult<()> {
    python
        .detach(|| {
            register_api_server_control_plane_processor_on_runtime(
                engine,
                ApiServerControlPlaneHostConfig {
                    bind_host,
                    bind_port,
                    node_name,
                },
            )
        })
        .map_err(|hosting_failure| PyRuntimeError::new_err(hosting_failure.to_string()))
}
