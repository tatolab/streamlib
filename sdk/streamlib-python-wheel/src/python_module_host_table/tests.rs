// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The table this wheel lends, read through `tatolab-module-sdk` as a module
//! reads it, over a real link.

use std::time::Duration;

use pyo3::types::{PyBytes, PyDict, PyList};
use streamlib::sdk::iceoryx2::Iceoryx2Node;
use tatolab_module_sdk::ModuleHost;

use super::*;
use crate::python_processor_context::{
    PythonRuntimeContextFullAccess, note_execution_mode_from_parent_on_runtime_context,
    note_stop_request_from_parent_on_runtime_context,
    open_runtime_context_full_access_for_helper_process,
};
use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;

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

#[test]
fn a_native_thread_writes_a_lent_output_with_no_python_attached_and_python_reads_it_in_order() {
    Python::initialize();
    const MESSAGES_WRITTEN_NATIVELY: usize = 50;
    // The source's context outlives the writes: a publisher that goes away
    // takes its undelivered messages with it.
    let (module_host, destination, _source_context) = Python::attach(|python| {
        let (source, destination) = one_wired_link(python, "native_write");
        let (context, _) = full_access_context_recording_escalates(python, source, "source");
        let capsule = open_module_host_table(&context, &[SOURCE_OUTPUT_PORT], &[]).unwrap();
        (module_host_of(&capsule), destination, context.unbind())
    });

    // A thread Python never saw, so nothing it does can hold the GIL.
    let writer = std::thread::spawn(move || {
        let output_port = module_host.open_output_port(SOURCE_OUTPUT_PORT).unwrap();
        for message in 0..MESSAGES_WRITTEN_NATIVELY {
            output_port
                .write(
                    format!("native {message}").as_bytes(),
                    Some(1_000 + message as i64),
                    message % 10 == 0,
                )
                .unwrap();
        }
    });
    writer.join().unwrap();

    Python::attach(|python| {
        for message in 0..MESSAGES_WRITTEN_NATIVELY {
            let (payload, timestamp_ns) = destination
                .read_from_input_port_with_timestamp(python, DESTINATION_INPUT_PORT)
                .unwrap();
            let payload: Vec<u8> = payload
                .unwrap_or_else(|| panic!("message {message} arrived"))
                .extract()
                .unwrap();
            assert_eq!(payload, format!("native {message}").into_bytes());
            assert_eq!(timestamp_ns, Some(1_000 + message as i64));
        }
    });
}

#[test]
fn a_native_thread_reads_a_lent_input_after_its_wait_says_one_is_waiting() {
    Python::initialize();
    let (module_host, source) = Python::attach(|python| {
        let (source, destination) = one_wired_link(python, "native_read");
        let (context, _) =
            full_access_context_recording_escalates(python, destination, "destination");
        let capsule = open_module_host_table(&context, &[], &[DESTINATION_INPUT_PORT]).unwrap();
        (module_host_of(&capsule), source)
    });
    let input_port = module_host.open_input_port(DESTINATION_INPUT_PORT).unwrap();
    assert!(input_port.read().unwrap().is_none());
    assert!(
        !module_host
            .wait_for_any_input_port(Duration::from_millis(10))
            .unwrap()
    );

    Python::attach(|python| {
        for (payload, timestamp_ns, starts_group) in
            [(&b"keyframe"[..], 77, true), (&b"delta"[..], 78, false)]
        {
            source
                .write_to_output_port(
                    python,
                    SOURCE_OUTPUT_PORT,
                    PyBytes::new(python, payload).as_any(),
                    Some(timestamp_ns),
                    starts_group,
                )
                .unwrap();
        }
    });

    let reader = std::thread::spawn(move || {
        assert!(
            module_host
                .wait_for_any_input_port(Duration::from_secs(5))
                .unwrap()
        );
        let mut read = Vec::new();
        while let Some(message) = input_port.read().unwrap() {
            read.push((
                message.payload().to_vec(),
                message.timestamp_ns(),
                message.starts_group(),
            ));
        }
        read
    });
    assert_eq!(
        reader.join().unwrap(),
        vec![
            (b"keyframe".to_vec(), 77, true),
            (b"delta".to_vec(), 78, false)
        ]
    );
}

#[test]
fn python_writes_to_a_lent_output_are_refused_naming_the_port() {
    Python::initialize();
    Python::attach(|python| {
        let (source, _destination) = one_wired_link(python, "python_refused");
        let (context, _) = full_access_context_recording_escalates(python, source, "source");
        let _capsule = open_module_host_table(&context, &[SOURCE_OUTPUT_PORT], &[]).unwrap();

        let refusal = context
            .getattr("outputs")
            .unwrap()
            .call_method1(
                "write",
                (SOURCE_OUTPUT_PORT, PyBytes::new(python, b"from python")),
            )
            .unwrap_err()
            .to_string();

        assert!(refusal.contains(SOURCE_OUTPUT_PORT), "{refusal}");
        assert!(
            refusal.contains("lent to this node's native code"),
            "{refusal}"
        );
    });
}

#[test]
fn lending_an_undeclared_port_or_one_already_lent_is_refused_by_name() {
    Python::initialize();
    Python::attach(|python| {
        let (source, _destination) = one_wired_link(python, "lend_refusals");
        let (context, _) = full_access_context_recording_escalates(python, source, "source");

        let undeclared = open_module_host_table(&context, &["never_declared"], &[])
            .unwrap_err()
            .to_string();
        assert!(undeclared.contains("\"never_declared\""), "{undeclared}");
        assert!(undeclared.contains("declares no such port"), "{undeclared}");

        let _capsule = open_module_host_table(&context, &[SOURCE_OUTPUT_PORT], &[]).unwrap();
        let twice = open_module_host_table(&context, &[SOURCE_OUTPUT_PORT], &[])
            .unwrap_err()
            .to_string();
        assert!(twice.contains("already lent"), "{twice}");
    });
}

#[test]
fn a_port_python_did_not_lend_is_refused_when_native_code_opens_it() {
    Python::initialize();
    let module_host = Python::attach(|python| {
        let (source, _destination) = one_wired_link(python, "open_refused");
        let (context, _) = full_access_context_recording_escalates(python, source, "source");
        module_host_of(&open_module_host_table(&context, &[], &[]).unwrap())
    });

    let refusal = module_host
        .open_output_port(SOURCE_OUTPUT_PORT)
        .unwrap_err()
        .to_string();

    assert!(refusal.contains("was not lent to native code"), "{refusal}");
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
fn a_catalog_entry_native_code_states_reaches_the_parent_as_the_op_python_sends() {
    Python::initialize();
    let (module_host, escalates_sent) = Python::attach(|python| {
        let (source, _destination) = one_wired_link(python, "catalog_entry");
        let (context, escalates_sent) =
            full_access_context_recording_escalates(python, source, "source");
        let capsule = open_module_host_table(&context, &[SOURCE_OUTPUT_PORT], &[]).unwrap();
        (module_host_of(&capsule), escalates_sent.unbind())
    });

    std::thread::spawn(move || {
        module_host
            .open_output_port(SOURCE_OUTPUT_PORT)
            .unwrap()
            .set_catalog_entry("video", r#"{"codec":"avc1.42e01f","codedWidth":320}"#)
            .unwrap();
    })
    .join()
    .unwrap();

    Python::attach(|python| {
        let escalates_sent = escalates_sent.bind(python);
        assert_eq!(escalates_sent.len(), 1);
        let op = escalates_sent.get_item(0).unwrap();
        let op_text = op.str().unwrap().to_string();
        assert_eq!(
            op_text,
            "{'op': 'set_output_port_catalog_entry', 'port': 'frames_to_downstream', \
             'section': 'video', 'entry': {'codec': 'avc1.42e01f', 'codedWidth': 320}}"
        );
    });
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
        serde_json::json!({"major": 1, "minor": 1})
    );
    assert_eq!(capabilities["node_id"], "source-node");
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

#[test]
fn an_input_is_lent_only_to_a_node_the_runtime_runs_manually() {
    Python::initialize();
    Python::attach(|python| {
        let (_source, reactive_destination) = one_wired_link(python, "lend_reactive");
        let (reactive, _) =
            full_access_context_recording_escalates(python, reactive_destination, "reactive");
        note_execution_mode_from_parent_on_runtime_context(python, &reactive, "reactive").unwrap();
        let refusal = open_module_host_table(&reactive, &[], &[DESTINATION_INPUT_PORT])
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("execution=\"manual\""), "{refusal}");

        let (_source, early_destination) = one_wired_link(python, "lend_before_run");
        let (lent_before_its_run, _) =
            full_access_context_recording_escalates(python, early_destination, "lent_early");
        let _capsule =
            open_module_host_table(&lent_before_its_run, &[], &[DESTINATION_INPUT_PORT]).unwrap();
        let refusal = note_execution_mode_from_parent_on_runtime_context(
            python,
            &lent_before_its_run,
            "continuous",
        )
        .unwrap_err()
        .to_string();
        assert!(refusal.contains(DESTINATION_INPUT_PORT), "{refusal}");
        note_execution_mode_from_parent_on_runtime_context(python, &lent_before_its_run, "manual")
            .unwrap();
    });
}
