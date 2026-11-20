// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The table this wheel lends, read through `tatolab-module-sdk` as a module
//! reads it, over a real link: the table root's stop flag, host capabilities
//! and node-host lifetime here, the lent ports and the streams a module opens
//! in the child modules.

use pyo3::types::{PyDict, PyList};
use streamlib::sdk::iceoryx2::Iceoryx2Node;
use tatolab_module_sdk::ModuleHost;

use super::*;
use crate::python_node_context::{
    PythonRuntimeContextFullAccess, note_stop_request_from_parent_on_runtime_context,
    open_runtime_context_full_access_for_helper_process,
};
use crate::python_node_link_data_access::PythonNodeLinkDataAccess;

mod audio_and_virtual_camera_stream_tests;
mod lent_port_tests;

const SOURCE_OUTPUT_PORT: &str = "frames_to_downstream";
const DESTINATION_INPUT_PORT: &str = "frames_from_upstream";

fn helper_plane() -> PythonNodeLinkDataAccess {
    PythonNodeLinkDataAccess::over_helper_process_iceoryx2_node(
        Iceoryx2Node::for_this_test_process(),
    )
}

/// A source whose output feeds a destination's input, both declared.
fn one_wired_link(
    python: Python<'_>,
    label: &str,
) -> (PythonNodeLinkDataAccess, PythonNodeLinkDataAccess) {
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
    plane: PythonNodeLinkDataAccess,
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

#[test]
fn the_stop_flag_native_code_reads_follows_the_parents_stop_request() {
    Python::initialize();
    let (module_host, context) = Python::attach(|python| {
        let (source, _destination) = one_wired_link(python, "stop_flag");
        let (context, _) = full_access_context_recording_escalates(python, source, "source");
        let capsule = open_module_host_table(&context, &[], &[]).unwrap();
        (module_host_of(&capsule), context.unbind())
    });
    assert!(!module_host.stop_requested());

    Python::attach(|python| {
        note_stop_request_from_parent_on_runtime_context(context.bind(python), true)
    });

    assert!(module_host.stop_requested());
}

#[test]
fn the_host_capabilities_name_the_table_version_and_the_lent_ports() {
    Python::initialize();
    let module_host = Python::attach(|python| {
        let (source, _destination) = one_wired_link(python, "capabilities");
        let (context, _) = full_access_context_recording_escalates(python, source, "source-node");
        module_host_of(&open_module_host_table(&context, &[SOURCE_OUTPUT_PORT], &[]).unwrap())
    });

    let capabilities: serde_json::Value =
        serde_json::from_str(module_host.host_capabilities_json()).unwrap();

    assert_eq!(
        capabilities["module_host_table"],
        serde_json::json!({"major": 1, "minor": 12})
    );
    assert_eq!(capabilities["node_id"], "source-node");
    assert_eq!(
        capabilities["capture_device_access_prompting"], "refuse_what_was_never_asked_for",
        "a helper the engine did not spawn never prompts"
    );
    assert_eq!(
        capabilities["lent_output_ports"],
        serde_json::json!([SOURCE_OUTPUT_PORT])
    );
}

#[test]
fn the_node_host_outlives_the_capsule_for_as_long_as_native_code_holds_it() {
    Python::initialize();
    let module_host = Python::attach(|python| {
        let (source, _destination) = one_wired_link(python, "outlives_capsule");
        let (context, _) = full_access_context_recording_escalates(python, source, "source");
        let capsule = open_module_host_table(&context, &[SOURCE_OUTPUT_PORT], &[]).unwrap();
        let module_host = module_host_of(&capsule);
        drop(capsule);
        drop(context);
        python.run(c"import gc; gc.collect()", None, None).unwrap();
        module_host
    });

    let output_port = module_host.open_output_port(SOURCE_OUTPUT_PORT).unwrap();
    output_port.write(b"after the capsule", None, true).unwrap();
}
