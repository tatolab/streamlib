// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The table this wheel lends, read through `tatolab-module-sdk` as a module
//! reads it, over a real link.

use pyo3::types::{PyDict, PyList};
use streamlib::sdk::iceoryx2::Iceoryx2Node;
use tatolab_module_sdk::ModuleHost;

use super::*;
use crate::python_processor_context::{
    PythonRuntimeContextFullAccess, open_runtime_context_full_access_for_helper_process,
};
use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;

mod audio_and_virtual_camera_stream_tests;
mod lent_port_tests;

const SOURCE_OUTPUT_PORT: &str = "frames_to_downstream";
const DESTINATION_INPUT_PORT: &str = "frames_from_upstream";

fn helper_plane() -> PythonProcessorLinkDataAccess {
    PythonProcessorLinkDataAccess::over_helper_process_iceoryx2_node(
        Iceoryx2Node::for_this_test_process(),
    )
}

/// A source whose output feeds a destination's input, both declared.
fn one_wired_link(
    python: Python<'_>,
    label: &str,
) -> (PythonProcessorLinkDataAccess, PythonProcessorLinkDataAccess) {
    let run = std::process::id();
    let channel = format!("modulehost{run}_{label}/{SOURCE_OUTPUT_PORT}");
    let notify = format!("modulehost{run}_{label}_dest/notify");
    let source = helper_plane();
    let destination = helper_plane();
    source.declare_ports(Vec::new(), vec![SOURCE_OUTPUT_PORT.to_string()]);
    destination.declare_ports(vec![DESTINATION_INPUT_PORT.to_string()], Vec::new());
    destination
        .wire_input_link(
            python,
            DESTINATION_INPUT_PORT,
            &channel,
            &channel,
            &notify,
            "read_next_in_order",
            64,
            64,
            2,
            1,
            "link-1",
            None,
            None,
            None,
            None,
        )
        .unwrap();
    source
        .wire_output_link(
            python,
            SOURCE_OUTPUT_PORT,
            &channel,
            &notify,
            1024,
            1 << 20,
            64,
            2,
            1,
            "link-1",
            None,
        )
        .unwrap();
    (source, destination)
}

/// A context over `plane` whose parent records each escalate op it is sent.
fn full_access_context_recording_escalates<'py>(
    python: Python<'py>,
    plane: PythonProcessorLinkDataAccess,
    node_id: &str,
) -> (
    Bound<'py, PythonRuntimeContextFullAccess>,
    Bound<'py, PyList>,
) {
    let escalates_sent = PyList::empty(python);
    let namespace = PyDict::new(python);
    namespace
        .set_item("escalates_sent", &escalates_sent)
        .unwrap();
    python
        .run(
            c"def escalate(op):\n    escalates_sent.append(dict(op))\n    return {}\n",
            Some(&namespace),
            None,
        )
        .unwrap();
    let escalate = namespace.get_item("escalate").unwrap().unwrap();
    let plane = Bound::new(python, plane).unwrap();
    let context = open_runtime_context_full_access_for_helper_process(
        python,
        PyDict::new(python).as_any(),
        &plane,
        "runtime-under-test".to_string(),
        node_id.to_string(),
        Some(&escalate),
        Some(&escalate),
    )
    .unwrap();
    (Bound::new(python, context).unwrap(), escalates_sent)
}

fn open_module_host_table<'py>(
    context: &Bound<'py, PythonRuntimeContextFullAccess>,
    output_ports: &[&str],
    input_ports: &[&str],
) -> PyResult<Bound<'py, PyAny>> {
    let keywords = PyDict::new(context.py());
    keywords.set_item("output_ports", output_ports.to_vec())?;
    keywords.set_item("input_ports", input_ports.to_vec())?;
    context.call_method("open_module_host_table", (), Some(&keywords))
}

fn module_host_of(capsule: &Bound<'_, PyAny>) -> ModuleHost {
    ModuleHost::from_python_module_host_table_capsule(capsule).unwrap()
}
