// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Ports a node lends to native code: reads, writes, refusals, stream facts,
//! described writes and given-up groups, over a real link.

use std::time::Duration;

use pyo3::types::PyBytes;
use streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor;

use super::*;
use crate::python_bag_conversion::python_object_to_json_value;
use crate::python_port_payload::PythonPortPayloadBorrow;
use crate::python_processor_context::note_execution_mode_from_parent_on_runtime_context;

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

unsafe extern "C" fn answers_forty_two(
    _input_values: *const f64,
    _config_values: *const f64,
    _state_values: *mut f64,
    output_values: *mut f64,
    _now_ns: i64,
) -> i32 {
    unsafe {
        *output_values = 42.0;
    }
    tatolab_module_sdk::module_host_table_abi::MODULE_HOST_TABLE_STATUS_OK
}

#[test]
fn a_native_thread_reads_an_input_a_compiled_island_feeds_while_python_holds_the_gil() {
    Python::initialize();
    Python::attach(|python| {
        let (_source, destination) = one_wired_link(python, "lend_compiled_island");
        destination.declare_ports(vec!["answer".to_string()], Vec::new());
        let evaluate_entry: tatolab_module_sdk::module_host_table_abi::ModuleHostTableFunctionNodeEvaluateEntry =
            answers_forty_two;
        let called_through = (evaluate_entry as *const () as usize)
            .into_pyobject(python)
            .unwrap()
            .into_any()
            .unbind();
        crate::python_function_node_island::feed_an_input_port_from_a_function_node_island(
            &destination,
            "answer",
            vec![(
                "answers".to_string(),
                0,
                called_through,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                1,
                Vec::new(),
            )],
            (0, 0),
            vec![Default::default()],
        )
        .unwrap();
        let (context, _) =
            full_access_context_recording_escalates(python, destination, "reads_the_answer");
        note_execution_mode_from_parent_on_runtime_context(python, &context, "manual").unwrap();
        let module_host =
            module_host_of(&open_module_host_table(&context, &[], &["answer"]).unwrap());

        // This thread holds the GIL throughout, so a read that needed it
        // would block until the timeout below.
        let (payload_sender, payload_receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let input_port = module_host.open_input_port("answer").unwrap();
            let message = input_port
                .read()
                .unwrap()
                .expect("an island answers every read");
            payload_sender.send(message.payload().to_vec()).unwrap();
        });
        let payload = payload_receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the native read evaluated the island without waiting on the GIL");
        assert_eq!(payload, 42.0f64.to_le_bytes().to_vec());
    });
}

#[test]
fn an_input_an_island_running_a_node_as_python_feeds_is_refused_naming_the_node() {
    Python::initialize();
    Python::attach(|python| {
        let (_source, destination) = one_wired_link(python, "lend_island_run_as_python");
        destination.declare_ports(vec!["count".to_string()], Vec::new());
        let namespace = PyDict::new(python);
        python
            .run(
                c"class AnswersOne:\n    def evaluate(self, input_values, now_ns):\n        return [1.0]\n",
                Some(&namespace),
                None,
            )
            .unwrap();
        let run_as_python = namespace
            .get_item("AnswersOne")
            .unwrap()
            .unwrap()
            .call0()
            .unwrap()
            .unbind();
        crate::python_function_node_island::feed_an_input_port_from_a_function_node_island(
            &destination,
            "count",
            vec![(
                "tally".to_string(),
                0,
                run_as_python,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                1,
                vec![],
            )],
            (0, 0),
            Vec::new(),
        )
        .unwrap();
        let (context, _) =
            full_access_context_recording_escalates(python, destination, "reads_the_tally");
        note_execution_mode_from_parent_on_runtime_context(python, &context, "manual").unwrap();

        let refusal = open_module_host_table(&context, &[], &["count", DESTINATION_INPUT_PORT])
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("\"count\""), "{refusal}");
        assert!(refusal.contains("function node `tally`"), "{refusal}");
        assert!(refusal.contains("runs as Python"), "{refusal}");

        let _capsule = open_module_host_table(&context, &[], &[DESTINATION_INPUT_PORT])
            .expect("a refused lend lends nothing, so the channel-fed input is still free");
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
fn stream_facts_native_code_states_reach_the_parent_as_the_op_python_sends() {
    Python::initialize();
    let (module_host, escalates_sent) = Python::attach(|python| {
        let (source, _destination) = one_wired_link(python, "stream_facts");
        let (context, escalates_sent) =
            full_access_context_recording_escalates(python, source, "source");
        let capsule = open_module_host_table(&context, &[SOURCE_OUTPUT_PORT], &[]).unwrap();
        (module_host_of(&capsule), escalates_sent.unbind())
    });

    std::thread::spawn(move || {
        module_host
            .open_output_port(SOURCE_OUTPUT_PORT)
            .unwrap()
            .set_stream_facts(
                "packets",
                r#"{"rfc6381_codec":"avc1.42e01f","coded_width":320}"#,
            )
            .unwrap();
    })
    .join()
    .unwrap();

    Python::attach(|python| {
        let escalates_sent = escalates_sent.bind(python);
        assert_eq!(escalates_sent.len(), 1);
        let op = escalates_sent.get_item(0).unwrap();
        assert_eq!(
            op.get_item("op").unwrap().extract::<String>().unwrap(),
            "set_output_port_stream_facts"
        );
        assert_eq!(
            op.get_item("port").unwrap().extract::<String>().unwrap(),
            "frames_to_downstream"
        );
        let stream_facts = python_object_to_json_value(&op.get_item("stream_facts").unwrap())
            .expect("the facts are plain JSON");
        assert_eq!(
            stream_facts,
            serde_json::json!({"family": "packets", "rfc6381_codec": "avc1.42e01f", "coded_width": 320})
        );
    });
}

#[test]
fn stream_facts_whose_fields_are_not_one_json_object_are_refused_naming_the_port() {
    Python::initialize();
    let module_host = Python::attach(|python| {
        let (source, _destination) = one_wired_link(python, "stream_facts_not_json");
        let (context, _) = full_access_context_recording_escalates(python, source, "source");
        module_host_of(&open_module_host_table(&context, &[SOURCE_OUTPUT_PORT], &[]).unwrap())
    });

    let refusal = module_host
        .open_output_port(SOURCE_OUTPUT_PORT)
        .unwrap()
        .set_stream_facts("packets", "[1, 2]")
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("frames_to_downstream") && refusal.contains("one JSON object"),
        "got {refusal}"
    );
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

#[test]
fn a_described_native_write_reaches_a_native_reader_with_its_descriptor_and_lends_no_frame() {
    Python::initialize();
    let (writer_host, reader_host, _contexts) = Python::attach(|python| {
        let (source, destination) = one_wired_link(python, "described");
        let (source_context, _) = full_access_context_recording_escalates(python, source, "source");
        let (destination_context, _) =
            full_access_context_recording_escalates(python, destination, "destination");
        let writer_capsule =
            open_module_host_table(&source_context, &[SOURCE_OUTPUT_PORT], &[]).unwrap();
        let reader_capsule =
            open_module_host_table(&destination_context, &[], &[DESTINATION_INPUT_PORT]).unwrap();
        (
            module_host_of(&writer_capsule),
            module_host_of(&reader_capsule),
            (source_context.unbind(), destination_context.unbind()),
        )
    });
    let packets_unit = DataChannelPayloadDescriptor::packets_unit(
        &streamlib::sdk::iceoryx2::PacketsUnitDescriptorBody {
            codec: "h264".to_string(),
        },
    )
    .unwrap();
    writer_host
        .open_output_port(SOURCE_OUTPUT_PORT)
        .unwrap()
        .write_described(
            b"access unit",
            Some(90),
            true,
            &ModuleHostTablePayloadDescriptor {
                kind: packets_unit.kind,
                body: packets_unit.body,
            },
        )
        .unwrap();

    let message = reader_host
        .open_input_port(DESTINATION_INPUT_PORT)
        .unwrap()
        .read()
        .unwrap()
        .expect("the described write arrived");
    assert_eq!(message.payload(), b"access unit");
    assert_eq!(message.timestamp_ns(), 90);
    let read_descriptor = message.payload_descriptor().unwrap();
    assert_eq!(
        (read_descriptor.kind, read_descriptor.body),
        (packets_unit.kind, packets_unit.body)
    );
    let refusal = message.lend_pooled_gpu_frame().unwrap_err().to_string();
    assert!(
        refusal.contains("kind 4") && refusal.contains("Image frame"),
        "{refusal}"
    );
}

#[test]
fn groups_native_code_declares_given_up_reach_the_next_reader_as_lost_since_its_last_read() {
    Python::initialize();
    let (writer_host, reader_host, _contexts) = Python::attach(|python| {
        let (source, destination) = one_wired_link(python, "given_up");
        let (source_context, _) = full_access_context_recording_escalates(python, source, "source");
        let (destination_context, _) =
            full_access_context_recording_escalates(python, destination, "destination");
        let writer_capsule =
            open_module_host_table(&source_context, &[SOURCE_OUTPUT_PORT], &[]).unwrap();
        let reader_capsule =
            open_module_host_table(&destination_context, &[], &[DESTINATION_INPUT_PORT]).unwrap();
        (
            module_host_of(&writer_capsule),
            module_host_of(&reader_capsule),
            (source_context.unbind(), destination_context.unbind()),
        )
    });
    let output_port = writer_host.open_output_port(SOURCE_OUTPUT_PORT).unwrap();
    output_port.write(b"group 1", Some(1), true).unwrap();
    output_port
        .declare_groups_given_up_before_the_next_write(2)
        .unwrap();
    output_port.write(b"group 4", Some(4), true).unwrap();

    let input_port = reader_host.open_input_port(DESTINATION_INPUT_PORT).unwrap();
    let first = input_port.read().unwrap().expect("group 1 arrived");
    assert_eq!(
        (first.payload(), first.lost_since_last_read()),
        (&b"group 1"[..], 0)
    );
    let after_the_gap = input_port.read().unwrap().expect("group 4 arrived");
    assert_eq!(
        (
            after_the_gap.payload(),
            after_the_gap.lost_since_last_read()
        ),
        (&b"group 4"[..], 2)
    );
}

/// The module SDK writes the module-facing half of the descriptor wire layout
/// the engine's own types write; the two agree byte for byte.
#[test]
fn the_module_sdks_descriptors_lay_out_as_the_engines() {
    use streamlib::sdk::iceoryx2::{
        ChannelsBlockDescriptorBody, ChannelsBlockSampleType, DataChannelPayloadDescriptorKind,
        GpuPoolSlotFrameDescriptorBody, ImageFrameDescriptorBody, ImageFrameH273Colour,
        PacketsUnitDescriptorBody,
    };
    use tatolab_module_sdk::module_host_table_abi::{
        MODULE_HOST_TABLE_PAYLOAD_KIND_ARRAY_FRAME, MODULE_HOST_TABLE_PAYLOAD_KIND_BYTES,
        MODULE_HOST_TABLE_PAYLOAD_KIND_CHANNELS_BLOCK,
        MODULE_HOST_TABLE_PAYLOAD_KIND_GPU_POOL_SLOT_FRAME,
        MODULE_HOST_TABLE_PAYLOAD_KIND_IMAGE_FRAME, MODULE_HOST_TABLE_PAYLOAD_KIND_PACKETS_UNIT,
        MODULE_HOST_TABLE_PAYLOAD_KIND_RECORDS_MESSAGE,
    };
    use tatolab_module_sdk::{
        ModuleHostChannelsBlock, ModuleHostChannelsBlockSampleType, ModuleHostImageFrame,
        ModuleHostImageFrameH273Colour, packets_unit_payload_descriptor,
        records_message_payload_descriptor,
    };

    for (module_kind, engine_kind) in [
        (
            MODULE_HOST_TABLE_PAYLOAD_KIND_BYTES,
            DataChannelPayloadDescriptorKind::Bytes,
        ),
        (
            MODULE_HOST_TABLE_PAYLOAD_KIND_GPU_POOL_SLOT_FRAME,
            DataChannelPayloadDescriptorKind::GpuPoolSlotFrame,
        ),
        (
            MODULE_HOST_TABLE_PAYLOAD_KIND_IMAGE_FRAME,
            DataChannelPayloadDescriptorKind::ImageFrame,
        ),
        (
            MODULE_HOST_TABLE_PAYLOAD_KIND_CHANNELS_BLOCK,
            DataChannelPayloadDescriptorKind::ChannelsBlock,
        ),
        (
            MODULE_HOST_TABLE_PAYLOAD_KIND_PACKETS_UNIT,
            DataChannelPayloadDescriptorKind::PacketsUnit,
        ),
        (
            MODULE_HOST_TABLE_PAYLOAD_KIND_RECORDS_MESSAGE,
            DataChannelPayloadDescriptorKind::RecordsMessage,
        ),
        (
            MODULE_HOST_TABLE_PAYLOAD_KIND_ARRAY_FRAME,
            DataChannelPayloadDescriptorKind::ArrayFrame,
        ),
    ] {
        assert_eq!(module_kind, engine_kind as u32, "{engine_kind:?}");
    }

    let slot = GpuPoolSlotFrameDescriptorBody {
        pool: [9; 16],
        slot: 3,
        generation: 11,
        timeline_value: 13,
        image_layout: 5,
    };
    let colour = ImageFrameH273Colour {
        primaries: 1,
        transfer: 13,
        matrix: 6,
        range: 0,
    };
    let engine_image_frame = DataChannelPayloadDescriptor::image_frame(ImageFrameDescriptorBody {
        slot,
        width: 640,
        height: 360,
        format: 7,
        colour,
    });
    let engine_slot = DataChannelPayloadDescriptor::gpu_pool_slot_frame(slot);
    let module_image_frame = ModuleHostImageFrame {
        width: 640,
        height: 360,
        texture_format: 7,
        colour: ModuleHostImageFrameH273Colour {
            primaries: 1,
            transfer: 13,
            matrix: 6,
            range: 0,
        },
    }
    .payload_descriptor_in_the_slot(&ModuleHostTablePayloadDescriptor {
        kind: engine_slot.kind,
        body: engine_slot.body,
    })
    .unwrap();
    assert_eq!(
        (module_image_frame.kind, module_image_frame.body),
        (engine_image_frame.kind, engine_image_frame.body)
    );

    let engine_packets_unit =
        DataChannelPayloadDescriptor::packets_unit(&PacketsUnitDescriptorBody {
            codec: "h264".to_string(),
        })
        .unwrap();
    let module_packets_unit = packets_unit_payload_descriptor("h264").unwrap();
    assert_eq!(
        (module_packets_unit.kind, module_packets_unit.body),
        (engine_packets_unit.kind, engine_packets_unit.body)
    );

    for (module_sample_type, engine_sample_type) in [
        (
            ModuleHostChannelsBlockSampleType::F32,
            ChannelsBlockSampleType::F32,
        ),
        (
            ModuleHostChannelsBlockSampleType::F64,
            ChannelsBlockSampleType::F64,
        ),
    ] {
        let engine_channels_block =
            DataChannelPayloadDescriptor::channels_block(ChannelsBlockDescriptorBody {
                start_sample: 0x0102_0304_0506_0708,
                sample_rate: 44_100,
                channel_count: 6,
                samples_per_channel: 1024,
                sample_type: engine_sample_type as u32,
            });
        let module_channels_block = ModuleHostChannelsBlock {
            start_sample: 0x0102_0304_0506_0708,
            sample_rate: 44_100,
            channel_count: 6,
            samples_per_channel: 1024,
            sample_type: module_sample_type,
        }
        .payload_descriptor();
        assert_eq!(
            (module_channels_block.kind, module_channels_block.body),
            (engine_channels_block.kind, engine_channels_block.body)
        );
    }

    let engine_records_message = DataChannelPayloadDescriptor::records_message();
    let module_records_message = records_message_payload_descriptor();
    assert_eq!(
        (module_records_message.kind, module_records_message.body),
        (engine_records_message.kind, engine_records_message.body)
    );
}
