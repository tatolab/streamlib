// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::prelude::*;
use pyo3::types::{PyCapsule, PyDict};

use crate::python_bag_conversion::{json_value_to_python_object, python_object_to_json_value};
use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;
use crate::python_helper_stream_clock::{PythonStreamClockReading, helper_stream_clock_now_ns};
use crate::python_module_host_table::module_host_table_capsule_lending;
use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;

use super::gpu_context::{PythonGpuContextFullAccess, PythonGpuContextLimitedAccess};
use super::link_data_access::{PythonLinkInputDataReader, PythonLinkOutputDataWriter};
use streamlib::sdk::processor_interpreter::{
    PROCESSOR_INTERPRETER_NODE_NAME_ENVIRONMENT_VARIABLE,
    PROCESSOR_INTERPRETER_STREAM_ID_ENVIRONMENT_VARIABLE,
    PROCESSOR_INTERPRETER_STREAM_PROJECT_DIRECTORY_ENVIRONMENT_VARIABLE,
    SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE,
};

/// The stream and node a helper's node is, as its parent named them at spawn;
/// each is empty in a process no parent spawned.
#[derive(Clone)]
struct StreamAndNodeTheHelperHosts {
    node_name: String,
    stream_id: String,
    stream_project_directory: String,
}

impl StreamAndNodeTheHelperHosts {
    fn named_by_the_parent_at_spawn() -> Self {
        let named = |variable: &str| std::env::var(variable).unwrap_or_default();
        Self {
            node_name: named(PROCESSOR_INTERPRETER_NODE_NAME_ENVIRONMENT_VARIABLE),
            stream_id: named(PROCESSOR_INTERPRETER_STREAM_ID_ENVIRONMENT_VARIABLE),
            stream_project_directory: named(
                PROCESSOR_INTERPRETER_STREAM_PROJECT_DIRECTORY_ENVIRONMENT_VARIABLE,
            ),
        }
    }

    fn asset_content_path(&self, asset: &str) -> PyResult<String> {
        let asset = uuid::Uuid::parse_str(asset).map_err(|error| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "`{asset}` is not an asset UUID: {error}"
            ))
        })?;
        let content_path = streamlib_project_repository::the_content_path_of_an_asset(
            std::path::Path::new(&self.stream_project_directory),
            asset,
        )
        .map_err(|refusal| pyo3::exceptions::PyLookupError::new_err(refusal.to_string()))?;
        Ok(content_path.to_string_lossy().into_owned())
    }
}

/// Privileged runtime context passed to `setup` / `teardown` / `start` / `stop`.
///
/// Built in the helper process the processor runs in — there is no engine in
/// that process to borrow a view from, so everything a hook reads is either
/// local or was passed down by the parent.
#[pyclass(name = "RuntimeContextFullAccess", module = "tatolab.stream", frozen)]
pub(crate) struct PythonRuntimeContextFullAccess {
    runtime_id: String,
    processor_id: String,
    stream_and_node: StreamAndNodeTheHelperHosts,
    configuration: serde_json::Value,
    link_input_data_reader: Py<PythonLinkInputDataReader>,
    link_output_data_writer: Py<PythonLinkOutputDataWriter>,
    gpu_limited_access_context: Py<PythonGpuContextLimitedAccess>,
    gpu_full_access_context: Py<PythonGpuContextFullAccess>,
    /// This helper's own record of what its parent last announced, shared with
    /// the limited-access view derived from this one.
    pause_state_announced_by_parent: Arc<AtomicBool>,
    link_data_access: Py<PythonProcessorLinkDataAccess>,
    escalate_request_to_parent: Option<Py<PyAny>>,
    /// Set once the parent asks the node to stop, before its `stop` runs, and
    /// read by the native code a module host table was lent to.
    stop_requested_by_parent: Arc<AtomicBool>,
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
                streamlib_consumer_rhi::runtime_vulkan_device_uuid_named_at_spawn(),
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
        stream_and_node: StreamAndNodeTheHelperHosts::named_by_the_parent_at_spawn(),
        configuration: python_object_to_json_value(configuration)?,
        link_input_data_reader: Py::new(
            python,
            PythonLinkInputDataReader {
                link_data_access: link_data_access.clone_ref(python),
                gpu_limited_access_context: gpu_limited_access_context.clone_ref(python),
                escalate_request_to_parent: escalate_request_to_parent
                    .map(|requester| requester.clone().unbind()),
            },
        )?,
        link_output_data_writer: Py::new(
            python,
            PythonLinkOutputDataWriter {
                link_data_access: link_data_access.clone_ref(python),
                gpu_limited_access_context: gpu_limited_access_context.clone_ref(python),
                escalate_request_to_parent: escalate_request_to_parent
                    .map(|requester| requester.clone().unbind()),
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
        link_data_access,
        escalate_request_to_parent: escalate_request_to_parent
            .map(|requester| requester.clone().unbind()),
        stop_requested_by_parent: Arc::new(AtomicBool::new(false)),
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
        stream_and_node: full_access_context.stream_and_node.clone(),
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

/// Record on a helper's context whether the parent has asked the node to stop,
/// so native code its module host table was lent to winds down.
#[pyfunction]
pub(crate) fn note_stop_request_from_parent_on_runtime_context(
    full_access_context: &Bound<'_, PythonRuntimeContextFullAccess>,
    stop_requested: bool,
) {
    full_access_context
        .get()
        .stop_requested_by_parent
        .store(stop_requested, Ordering::Release);
}

/// Record on a helper's context the execution mode the parent runs the node
/// in, refusing a mode that drives `process()` from inputs lent to native code.
#[pyfunction]
pub(crate) fn note_execution_mode_from_parent_on_runtime_context(
    python: Python<'_>,
    full_access_context: &Bound<'_, PythonRuntimeContextFullAccess>,
    execution_mode: &str,
) -> PyResult<()> {
    full_access_context
        .get()
        .link_data_access
        .bind(python)
        .get()
        .note_whether_the_runtime_drives_process_from_inputs(execution_mode != "manual")
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

    /// Stream time now in nanoseconds, on the clock the node's stream runs on.
    #[getter]
    fn time(&self) -> u64 {
        helper_stream_clock_now_ns().max(0) as u64
    }

    /// Which clock the node's stream runs on and where it stands.
    #[getter]
    fn stream_clock(&self) -> PythonStreamClockReading {
        PythonStreamClockReading::now()
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

    #[getter]
    fn node_name(&self) -> String {
        self.stream_and_node.node_name.clone()
    }

    #[getter]
    fn stream_id(&self) -> String {
        self.stream_and_node.stream_id.clone()
    }

    #[getter]
    fn stream_project_directory(&self) -> String {
        self.stream_and_node.stream_project_directory.clone()
    }

    /// Where the bytes of the project asset `asset` sit, to open and read but never write.
    fn asset_content_path(&self, asset: &str) -> PyResult<String> {
        self.stream_and_node.asset_content_path(asset)
    }

    /// Whether this processor is currently paused.
    fn is_paused(&self) -> bool {
        self.pause_state_announced_by_parent.load(Ordering::Relaxed)
    }

    /// Whether processing should proceed (not paused).
    fn should_process(&self) -> bool {
        !self.pause_state_announced_by_parent.load(Ordering::Relaxed)
    }

    /// Lend `output_ports` and `input_ports` to the node's native code, and
    /// hand back the module host table it reaches them through, as a capsule
    /// `tatolab-module-sdk` reads.
    ///
    /// Python's own writes to a lent output port, and reads of a lent input
    /// port, are refused by name from here on.
    #[pyo3(signature = (*, output_ports = Vec::new(), input_ports = Vec::new()))]
    fn open_module_host_table<'py>(
        &self,
        python: Python<'py>,
        output_ports: Vec<String>,
        input_ports: Vec<String>,
    ) -> PyResult<Bound<'py, PyCapsule>> {
        let data_plane = self
            .link_data_access
            .get()
            .lend_ports_to_native_code(&input_ports, &output_ports)?;
        module_host_table_capsule_lending(
            python,
            data_plane,
            &self.processor_id,
            &self.runtime_id,
            self.escalate_request_to_parent
                .as_ref()
                .map(|requester| requester.clone_ref(python)),
            Arc::clone(&self.stop_requested_by_parent),
            self.gpu_full_access_context
                .get()
                .helper_process_exchange_client
                .clone(),
        )
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
    stream_and_node: StreamAndNodeTheHelperHosts,
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

    /// Stream time now in nanoseconds, on the clock the node's stream runs on.
    #[getter]
    fn time(&self) -> u64 {
        helper_stream_clock_now_ns().max(0) as u64
    }

    /// Which clock the node's stream runs on and where it stands.
    #[getter]
    fn stream_clock(&self) -> PythonStreamClockReading {
        PythonStreamClockReading::now()
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

    #[getter]
    fn node_name(&self) -> String {
        self.stream_and_node.node_name.clone()
    }

    #[getter]
    fn stream_id(&self) -> String {
        self.stream_and_node.stream_id.clone()
    }

    #[getter]
    fn stream_project_directory(&self) -> String {
        self.stream_and_node.stream_project_directory.clone()
    }

    /// Where the bytes of the project asset `asset` sit, to open and read but never write.
    fn asset_content_path(&self, asset: &str) -> PyResult<String> {
        self.stream_and_node.asset_content_path(asset)
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
