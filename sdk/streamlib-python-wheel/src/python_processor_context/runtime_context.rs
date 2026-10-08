// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::python_bag_conversion::{json_value_to_python_object, python_object_to_json_value};
use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;
use crate::python_logging::monotonic_clock_now_ns;
use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;

use super::gpu_context::{PythonGpuContextFullAccess, PythonGpuContextLimitedAccess};
use super::link_data_access::{PythonLinkInputDataReader, PythonLinkOutputDataWriter};
use streamlib::sdk::processor_interpreter::SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE;

/// Privileged runtime context passed to `setup` / `teardown` / `start` / `stop`.
///
/// Built in the helper process the processor runs in — there is no engine in
/// that process to borrow a view from, so everything a hook reads is either
/// local or was passed down by the parent.
#[pyclass(name = "RuntimeContextFullAccess", module = "tatolab.stream", frozen)]
pub(crate) struct PythonRuntimeContextFullAccess {
    runtime_id: String,
    processor_id: String,
    configuration: serde_json::Value,
    link_input_data_reader: Py<PythonLinkInputDataReader>,
    link_output_data_writer: Py<PythonLinkOutputDataWriter>,
    gpu_limited_access_context: Py<PythonGpuContextLimitedAccess>,
    gpu_full_access_context: Py<PythonGpuContextFullAccess>,
    /// This helper's own record of what its parent last announced, shared with
    /// the limited-access view derived from this one.
    pause_state_announced_by_parent: Arc<AtomicBool>,
}

/// The context a helper process hands its own node's privileged hooks.
///
/// `escalate_request_to_parent` is the bridge's blocking round trip and
/// `release_to_parent_without_waiting` its door for releases a drop owes;
/// with both and the surface-share socket the parent's env names, the GPU
/// surface works here — without any of them, GPU calls refuse by name.
#[pyfunction]
#[pyo3(signature = (configuration, link_data_access, runtime_id, node_id, escalate_request_to_parent = None, release_to_parent_without_waiting = None))]
pub(crate) fn open_runtime_context_full_access_for_helper_process(
    python: Python<'_>,
    configuration: &Bound<'_, PyAny>,
    link_data_access: &Bound<'_, PythonProcessorLinkDataAccess>,
    runtime_id: String,
    node_id: String,
    escalate_request_to_parent: Option<&Bound<'_, PyAny>>,
    release_to_parent_without_waiting: Option<&Bound<'_, PyAny>>,
) -> PyResult<PythonRuntimeContextFullAccess> {
    let link_data_access = link_data_access.clone().unbind();
    let helper_process_exchange_client = match (
        escalate_request_to_parent,
        release_to_parent_without_waiting,
        std::env::var_os(SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE),
    ) {
        (Some(requester), Some(releaser), Some(surface_share_channel_name)) => {
            Some(Arc::new(HelperProcessGpuExchangeClient::new(
                requester.clone().unbind(),
                releaser.clone().unbind(),
                surface_share_channel_name,
                // Child-scoped, never the node's own runtime id — the
                // service's crash watchdog sweeps registrations by
                // runtime id, and this child's crash must sweep only
                // this child's adoptions.
                format!("helper:{node_id}"),
            )))
        }
        _ => None,
    };
    // Built before the reader, which carries it: a typed read offers this
    // very capability to whatever it constructs.
    let gpu_limited_access_context = Py::new(
        python,
        PythonGpuContextLimitedAccess::new_for_helper_process(
            helper_process_exchange_client.clone(),
        ),
    )?;
    Ok(PythonRuntimeContextFullAccess {
        runtime_id,
        processor_id: node_id,
        configuration: python_object_to_json_value(configuration)?,
        link_input_data_reader: Py::new(
            python,
            PythonLinkInputDataReader {
                link_data_access: link_data_access.clone_ref(python),
                gpu_limited_access_context: gpu_limited_access_context.clone_ref(python),
            },
        )?,
        link_output_data_writer: Py::new(
            python,
            PythonLinkOutputDataWriter {
                link_data_access: link_data_access.clone_ref(python),
            },
        )?,
        gpu_limited_access_context,
        gpu_full_access_context: Py::new(
            python,
            PythonGpuContextFullAccess {
                helper_process_exchange_client,
            },
        )?,
        pause_state_announced_by_parent: Arc::new(AtomicBool::new(false)),
    })
}

/// The limited-access view of the same node — same configuration, same links,
/// same pause state.
///
/// A helper builds both views once and hands each hook the one its phase
/// calls for.
#[pyfunction]
pub(crate) fn limited_access_view_of_runtime_context_for_helper_process(
    python: Python<'_>,
    full_access_context: &Bound<'_, PythonRuntimeContextFullAccess>,
) -> PythonRuntimeContextLimitedAccess {
    let full_access_context = full_access_context.get();
    PythonRuntimeContextLimitedAccess {
        runtime_id: full_access_context.runtime_id.clone(),
        processor_id: full_access_context.processor_id.clone(),
        configuration: full_access_context.configuration.clone(),
        link_input_data_reader: full_access_context.link_input_data_reader.clone_ref(python),
        link_output_data_writer: full_access_context
            .link_output_data_writer
            .clone_ref(python),
        gpu_limited_access_context: full_access_context
            .gpu_limited_access_context
            .clone_ref(python),
        pause_state_announced_by_parent: Arc::clone(
            &full_access_context.pause_state_announced_by_parent,
        ),
    }
}

/// Record on a helper's context the pause state the parent just announced, so
/// `is_paused` and `should_process` can answer without an engine to ask.
#[pyfunction]
pub(crate) fn note_pause_state_from_parent_on_runtime_context(
    full_access_context: &Bound<'_, PythonRuntimeContextFullAccess>,
    paused: bool,
) {
    full_access_context
        .get()
        .pause_state_announced_by_parent
        .store(paused, Ordering::Relaxed);
}

#[pymethods]
impl PythonRuntimeContextFullAccess {
    /// The processor's configuration, as the dict it was added with.
    #[getter]
    fn config<'py>(&self, python: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        configuration_as_python_dict(python, &self.configuration)
    }

    /// Current monotonic time in nanoseconds, on the engine's `MediaClock`.
    #[getter]
    fn time(&self) -> u64 {
        monotonic_clock_now_ns()
    }

    #[getter]
    fn inputs(&self, python: Python<'_>) -> Py<PythonLinkInputDataReader> {
        self.link_input_data_reader.clone_ref(python)
    }

    #[getter]
    fn outputs(&self, python: Python<'_>) -> Py<PythonLinkOutputDataWriter> {
        self.link_output_data_writer.clone_ref(python)
    }

    #[getter]
    fn gpu_limited_access(&self, python: Python<'_>) -> Py<PythonGpuContextLimitedAccess> {
        self.gpu_limited_access_context.clone_ref(python)
    }

    #[getter]
    fn gpu_full_access(&self, python: Python<'_>) -> Py<PythonGpuContextFullAccess> {
        self.gpu_full_access_context.clone_ref(python)
    }

    #[getter]
    fn runtime_id(&self) -> String {
        self.runtime_id.clone()
    }

    #[getter(node_id)]
    fn processor_id(&self) -> String {
        self.processor_id.clone()
    }

    /// Whether this processor is currently paused.
    fn is_paused(&self) -> bool {
        self.pause_state_announced_by_parent.load(Ordering::Relaxed)
    }

    /// Whether processing should proceed (not paused).
    fn should_process(&self) -> bool {
        !self.pause_state_announced_by_parent.load(Ordering::Relaxed)
    }
}

/// Restricted runtime context passed to `process` / `on_pause` / `on_resume`.
///
/// `gpu_full_access` is deliberately absent — reaching for it raises
/// `AttributeError`, mirroring the Rust capability split.
#[pyclass(
    name = "RuntimeContextLimitedAccess",
    module = "tatolab.stream",
    frozen
)]
pub(crate) struct PythonRuntimeContextLimitedAccess {
    runtime_id: String,
    processor_id: String,
    configuration: serde_json::Value,
    link_input_data_reader: Py<PythonLinkInputDataReader>,
    link_output_data_writer: Py<PythonLinkOutputDataWriter>,
    gpu_limited_access_context: Py<PythonGpuContextLimitedAccess>,
    /// Shared with the full-access view this one was derived from.
    pause_state_announced_by_parent: Arc<AtomicBool>,
}

#[pymethods]
impl PythonRuntimeContextLimitedAccess {
    /// The processor's configuration, as the dict it was added with.
    #[getter]
    fn config<'py>(&self, python: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        configuration_as_python_dict(python, &self.configuration)
    }

    /// Current monotonic time in nanoseconds, on the engine's `MediaClock`.
    #[getter]
    fn time(&self) -> u64 {
        monotonic_clock_now_ns()
    }

    #[getter]
    fn inputs(&self, python: Python<'_>) -> Py<PythonLinkInputDataReader> {
        self.link_input_data_reader.clone_ref(python)
    }

    #[getter]
    fn outputs(&self, python: Python<'_>) -> Py<PythonLinkOutputDataWriter> {
        self.link_output_data_writer.clone_ref(python)
    }

    #[getter]
    fn gpu_limited_access(&self, python: Python<'_>) -> Py<PythonGpuContextLimitedAccess> {
        self.gpu_limited_access_context.clone_ref(python)
    }

    #[getter]
    fn runtime_id(&self) -> String {
        self.runtime_id.clone()
    }

    #[getter(node_id)]
    fn processor_id(&self) -> String {
        self.processor_id.clone()
    }

    /// Whether this processor is currently paused.
    fn is_paused(&self) -> bool {
        self.pause_state_announced_by_parent.load(Ordering::Relaxed)
    }

    /// Whether processing should proceed (not paused).
    fn should_process(&self) -> bool {
        !self.pause_state_announced_by_parent.load(Ordering::Relaxed)
    }
}

fn configuration_as_python_dict<'py>(
    python: Python<'py>,
    configuration: &serde_json::Value,
) -> PyResult<Bound<'py, PyAny>> {
    if configuration.is_null() {
        return Ok(PyDict::new(python).into_any());
    }
    json_value_to_python_object(python, configuration)
}
