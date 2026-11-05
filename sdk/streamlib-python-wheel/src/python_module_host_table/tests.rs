// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The table this wheel lends, read through `tatolab-module-sdk` as a module
//! reads it, over a real link.

use std::time::Duration;

use pyo3::types::{PyBytes, PyDict, PyList};
use streamlib::sdk::iceoryx2::Iceoryx2Node;
use tatolab_module_sdk::ModuleHost;
use tatolab_module_sdk::module_host_table_abi::ModuleHostTableVirtualCameraStreamHandle;

use super::*;
use crate::python_port_payload::PythonPortPayloadBorrow;
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
            let payload = PythonPortPayloadBorrow::bytes_the_read_borrowed(
                &payload.unwrap_or_else(|| panic!("message {message} arrived")),
            );
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
        serde_json::json!({"major": 1, "minor": 9})
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

/// A host over a node that lent nothing: the audio verbs need no port.
fn module_host_lending_no_port(label: &str) -> ModuleHost {
    Python::initialize();
    Python::attach(|python| {
        let (source, _destination) = one_wired_link(python, label);
        let (context, _) = full_access_context_recording_escalates(python, source, "microphone");
        module_host_of(&open_module_host_table(&context, &[], &[]).unwrap())
    })
}

/// Whichever arm this machine probes — a device, or the null backend's
/// silence — delivers whole, device-stamped blocks through the module's
/// callback on its own thread, and none once delivery stops.
#[test]
fn a_module_captures_blocks_through_the_engines_audio_device_backend() {
    let module_host = module_host_lending_no_port("audio_capture");
    let mut capture_stream = module_host
        .open_audio_capture_stream(
            &tatolab_module_sdk::ModuleHostAudioCaptureStreamRequest::default(),
        )
        .expect("the default device, or silence, opens");
    let stream_format = capture_stream.stream_format();
    let (block_sender, block_receiver) = std::sync::mpsc::sync_channel(64);
    capture_stream
        .start_delivering_to(Box::new(move |captured_block| {
            let _ = block_sender.try_send((
                captured_block.interleaved_sample_bytes.len(),
                captured_block.sample_count,
                captured_block.first_sample_timestamp_ns,
            ));
        }))
        .unwrap();

    let mut previous_first_sample_timestamp_ns = i64::MIN;
    for _ in 0..3 {
        let (byte_count, sample_count, first_sample_timestamp_ns) = block_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("a block arrives within five seconds");
        assert!(sample_count > 0);
        assert_eq!(
            byte_count,
            sample_count as usize
                * stream_format.channels as usize
                * stream_format.sample_format.bytes_per_sample()
        );
        assert!(first_sample_timestamp_ns > previous_first_sample_timestamp_ns);
        previous_first_sample_timestamp_ns = first_sample_timestamp_ns;
    }
    assert_eq!(
        capture_stream.failure_that_ended_the_stream().unwrap(),
        None
    );

    capture_stream.stop_delivering().unwrap();
    while block_receiver.try_recv().is_ok() {}
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        block_receiver.try_recv().is_err(),
        "no block is delivered once delivery stops"
    );
}

#[test]
fn a_device_no_backend_has_is_refused_by_name() {
    let module_host = module_host_lending_no_port("audio_unknown_device");

    let refusal = module_host
        .open_audio_capture_stream(&tatolab_module_sdk::ModuleHostAudioCaptureStreamRequest {
            device_id: Some("tatolab-no-such-audio-device"),
            ..Default::default()
        })
        .unwrap_err()
        .to_string();

    assert!(
        refusal.starts_with("audio_capture_stream_open: "),
        "{refusal}"
    );
    assert!(
        refusal.contains("tatolab-no-such-audio-device"),
        "{refusal}"
    );
}

#[test]
fn a_format_the_device_does_not_open_in_is_refused_never_converted() {
    let module_host = module_host_lending_no_port("audio_format");

    let refusal = module_host
        .open_audio_capture_stream(&tatolab_module_sdk::ModuleHostAudioCaptureStreamRequest {
            channels: Some(31),
            ..Default::default()
        })
        .unwrap_err()
        .to_string();

    assert!(
        refusal.contains("31 channel(s) were requested"),
        "{refusal}"
    );
}

#[test]
fn a_closed_capture_streams_handle_names_nothing() {
    let audio_capture_streams = audio_capture_streams::AudioCaptureStreamsOpenedForOneNode::under(
        CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
    );
    let (handle, _stream_format) = audio_capture_streams
        .open("microphone", None, Default::default())
        .unwrap();
    assert!(audio_capture_streams.opened_stream(handle).is_ok());

    audio_capture_streams.close(handle);

    let refusal = audio_capture_streams
        .opened_stream(handle)
        .err()
        .expect("a closed stream's handle is refused");
    assert!(
        refusal.contains("names no capture stream this table has open"),
        "{refusal}"
    );
    let (next_handle, _stream_format) = audio_capture_streams
        .open("microphone", None, Default::default())
        .unwrap();
    assert_ne!(next_handle, handle, "a handle is never reused");
}

/// The format the playback tests ask for: what the speaker module plays.
const PLAYBACK_REQUEST_AT_48_KHZ_STEREO_F32:
    tatolab_module_sdk::ModuleHostAudioPlaybackStreamRequest<'static> =
    tatolab_module_sdk::ModuleHostAudioPlaybackStreamRequest {
        device_id: None,
        sample_rate: Some(48_000),
        channels: Some(2),
        sample_format: Some(tatolab_module_sdk::ModuleHostAudioSampleFormat::F32),
    };

/// Whichever arm this machine probes opens the stream in the format the module
/// asked for, and asks the module's callback for whole device buffers in it on
/// its own thread, and for none once requesting stops.
#[test]
fn a_module_plays_through_the_engines_audio_device_backend_in_the_format_it_asked_for() {
    let module_host = module_host_lending_no_port("audio_playback");
    let mut playback_stream = module_host
        .open_audio_playback_stream(&PLAYBACK_REQUEST_AT_48_KHZ_STEREO_F32)
        .expect("the default device, or the null arm, opens at 48 kHz stereo f32");
    assert_eq!(
        playback_stream.stream_format(),
        tatolab_module_sdk::ModuleHostAudioStreamFormat {
            sample_rate: 48_000,
            channels: 2,
            sample_format: tatolab_module_sdk::ModuleHostAudioSampleFormat::F32,
        }
    );
    let (request_sender, request_receiver) = std::sync::mpsc::sync_channel(64);
    playback_stream
        .start_requesting_from(Box::new(move |requested_block| {
            requested_block.interleaved_sample_bytes_to_fill.fill(0);
            let _ = request_sender.try_send((
                requested_block.interleaved_sample_bytes_to_fill.len(),
                requested_block.sample_count,
            ));
        }))
        .unwrap();

    for _ in 0..3 {
        let (byte_count, sample_count) = request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the device asks for samples within five seconds");
        assert!(sample_count > 0);
        assert_eq!(byte_count, sample_count as usize * 2 * 4);
    }
    assert_eq!(
        playback_stream.failure_that_ended_the_stream().unwrap(),
        None
    );

    playback_stream.stop_requesting().unwrap();
    while request_receiver.try_recv().is_ok() {}
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        request_receiver.try_recv().is_err(),
        "no buffer is asked for once requesting stops"
    );
}

#[test]
fn a_playback_device_no_backend_has_is_refused_by_name() {
    let module_host = module_host_lending_no_port("audio_playback_unknown_device");

    let refusal = module_host
        .open_audio_playback_stream(&tatolab_module_sdk::ModuleHostAudioPlaybackStreamRequest {
            device_id: Some("tatolab-no-such-audio-device"),
            ..PLAYBACK_REQUEST_AT_48_KHZ_STEREO_F32
        })
        .unwrap_err()
        .to_string();

    assert!(
        refusal.starts_with("audio_playback_stream_open: "),
        "{refusal}"
    );
    assert!(
        refusal.contains("tatolab-no-such-audio-device"),
        "{refusal}"
    );
}

#[test]
fn a_closed_playback_streams_handle_names_nothing_and_is_never_reused() {
    let audio_playback_streams =
        audio_playback_streams::AudioPlaybackStreamsOpenedForOneNode::default();
    let (handle, _stream_format, _device_period) = audio_playback_streams
        .open("speaker", None, Default::default())
        .unwrap();
    assert!(audio_playback_streams.opened_stream(handle).is_ok());

    audio_playback_streams.close(handle);

    let refusal = audio_playback_streams
        .opened_stream(handle)
        .err()
        .expect("a closed stream's handle is refused");
    assert!(
        refusal.contains("names no playback stream this table has open"),
        "{refusal}"
    );
    let (next_handle, _stream_format, _device_period) = audio_playback_streams
        .open("speaker", None, Default::default())
        .unwrap();
    assert_ne!(next_handle, handle, "a handle is never reused");
}

/// With a PipeWire daemon the camera node registers and offers an extent;
/// without one — the CI container — the open is refused naming the door and
/// why, never a bare failure.
#[test]
fn a_virtual_camera_opens_where_pipewire_answers_and_is_refused_by_name_elsewhere() {
    let module_host = module_host_lending_no_port("virtual_camera");

    match module_host.open_virtual_camera_stream("tatolab test camera") {
        Ok(camera) => {
            camera
                .offer_extent(64, 48, None)
                .expect("a PipeWire camera takes an even extent");
            let presentation = camera
                .write_yuyv_frame(&[0x80; 64 * 2 * 48], 64 * 2, 1)
                .expect("a frame of the offered extent is taken");
            assert_ne!(
                presentation,
                tatolab_module_sdk::ModuleHostVirtualCameraFramePresentation::EveryBufferIsHeldByTheConsumer,
                "with nothing watching, no buffer is held"
            );
        }
        Err(refusal) => {
            let refusal = refusal.to_string();
            assert!(
                refusal.starts_with("virtual_camera_stream_open: "),
                "{refusal}"
            );
            assert!(refusal.contains("PipeWire camera door"), "{refusal}");
        }
    }
}

#[test]
fn a_virtual_camera_handle_no_open_returned_is_refused() {
    let virtual_camera_streams =
        virtual_camera_streams::VirtualCameraStreamsOpenedForOneNode::default();

    let refusal = virtual_camera_streams
        .opened_camera(ModuleHostTableVirtualCameraStreamHandle { value: 3 })
        .err()
        .expect("a handle no open returned is refused");

    assert!(
        refusal.contains("names no camera this table has open"),
        "{refusal}"
    );
}
