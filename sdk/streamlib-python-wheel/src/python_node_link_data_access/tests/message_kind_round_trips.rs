// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// The whole point of the wiring surface: a helper process opens its own
/// publisher and its own subscriber on the services the parent named, and a
/// message written on one shows up on the other, its payload the same bytes.
/// Both planes live in this test's thread because iceoryx2's ports are
/// `!Send`.
#[test]
fn a_message_written_on_a_wired_output_arrives_on_a_wired_input_as_the_same_bytes() {
    Python::initialize();
    Python::attach(|python| {
        let (source, destination) = one_wired_link(python, "roundtrip");
        let payload_that_is_not_msgpack: &[u8] = b"\xc1 raw bytes, never decoded";

        source
            .write_to_output_port(
                python,
                "frames_to_downstream",
                PyBytes::new(python, payload_that_is_not_msgpack).as_any(),
                None,
                true,
            )
            .unwrap();

        let received = destination
            .read_from_input_port(python, "frames_from_upstream")
            .unwrap()
            .expect("the wired input received nothing");
        assert_eq!(
            PythonPortPayloadBorrow::bytes_the_read_borrowed(&received),
            payload_that_is_not_msgpack
        );
    });
}

/// A `[channel][sample]` array written as a Channels block reads back as a
/// `Audio` stating its shape, rate and first sample through a port
/// attribute, and as the raw borrow by name.
#[test]
fn a_channels_block_written_from_a_planar_array_reads_back_as_audio() {
    Python::initialize();
    Python::attach(|python| {
        let (source, destination) = one_wired_link(python, "channels-block");
        let samples = python
            .eval(
                c"memoryview(__import__('array').array('f', [0.0, 0.5, 1.0, -1.0, -0.5, 0.25])).cast('B').cast('f', [2, 3])",
                None,
                None,
            )
            .unwrap();
        for _ in 0..2 {
            source
                .write_channels_block_to_output_port(
                    python,
                    "frames_to_downstream",
                    &samples,
                    96_000,
                    48_000,
                    ChannelsOutputPortDeclaredShape::default(),
                    None,
                    true,
                )
                .unwrap();
        }

        let received =
            read_as_the_type_of_its_kind(python, &destination, MessageKindOfTheRead::ChannelsBlock);
        assert!(received.cast::<PythonAudio>().is_ok());
        let attribute = |name: &str| received.getattr(name).unwrap();
        assert_eq!(attribute("channel_count").extract::<u32>().unwrap(), 2);
        assert_eq!(
            attribute("samples_per_channel").extract::<u32>().unwrap(),
            3
        );
        assert_eq!(attribute("sample_rate").extract::<u32>().unwrap(), 48_000);
        assert_eq!(attribute("start_sample").extract::<u64>().unwrap(), 96_000);
        assert_eq!(attribute("dtype").extract::<String>().unwrap(), "f32");

        let by_name = destination
            .read_from_input_port(python, "frames_from_upstream")
            .unwrap()
            .expect("the second block arrived");
        assert_eq!(
            PythonPortPayloadBorrow::bytes_the_read_borrowed(&by_name).len(),
            2 * 3 * 4,
            "a read by name hands back the raw borrow"
        );

        let one_dimensional = python
            .eval(c"__import__('array').array('f', [0.0, 1.0])", None, None)
            .unwrap();
        let refusal = source
            .write_channels_block_to_output_port(
                python,
                "frames_to_downstream",
                &one_dimensional,
                0,
                48_000,
                ChannelsOutputPortDeclaredShape::default(),
                None,
                true,
            )
            .expect_err("a 1-D array is not [channel][sample]");
        assert!(
            refusal.to_string().contains("got 1 dimensions"),
            "{refusal}"
        );
    });
}

/// A dict written as a Records message reads back as the same dict, and a
/// dict missing a declared key is refused naming it.
#[test]
fn a_records_message_round_trips_a_dict_and_refuses_a_missing_declared_key() {
    Python::initialize();
    Python::attach(|python| {
        let (source, destination) = one_wired_link(python, "records-message");
        let record = python
            .eval(
                c"{'kind': 'tap', 'at': 3, 'extra': [1.5, None]}",
                None,
                None,
            )
            .unwrap();
        let declared_names = vec!["kind".to_string(), "at".to_string()];
        let encoded =
            records_message_payload_of("frames_to_downstream", &record, &declared_names).unwrap();
        source
            .write_described_by(
                python,
                "frames_to_downstream",
                PyBytes::new(python, &encoded).as_any(),
                None,
                true,
                |_| Ok(DataChannelPayloadDescriptor::records_message()),
            )
            .unwrap();

        let received = read_as_the_type_of_its_kind(
            python,
            &destination,
            MessageKindOfTheRead::RecordsMessage,
        );
        assert!(received.eq(&record).unwrap(), "got {received}");

        let missing_at = python.eval(c"{'kind': 'tap'}", None, None).unwrap();
        let refusal =
            records_message_payload_of("frames_to_downstream", &missing_at, &declared_names)
                .expect_err("`at` is declared");
        assert!(refusal.to_string().contains("\"at\""), "{refusal}");
    });
}

/// A read through a Packets input that also takes Records hands back a
/// Records message as its dict; an input taking Packets alone refuses it.
#[test]
fn a_read_through_an_input_taking_several_kinds_builds_the_arriving_kinds_type() {
    Python::initialize();
    Python::attach(|python| {
        let (source, destination) = one_wired_link(python, "records-into-packets-or-records");
        let record = python
            .eval(c"{'kind': 'tap', 'at': 3}", None, None)
            .unwrap();
        let encoded = records_message_payload_of(
            "frames_to_downstream",
            &record,
            &["kind".to_string(), "at".to_string()],
        )
        .unwrap();
        let write_the_record = || {
            source
                .write_described_by(
                    python,
                    "frames_to_downstream",
                    PyBytes::new(python, &encoded).as_any(),
                    None,
                    true,
                    |_| Ok(DataChannelPayloadDescriptor::records_message()),
                )
                .unwrap();
        };
        let read_through = |kinds: MessageKindsOfTheRead| {
            destination.read_from_input_port_offering_gpu_access(
                python,
                "frames_from_upstream",
                PortPayloadReadShape::OfItsKind(kinds),
                None,
            )
        };

        write_the_record();
        let received = read_through(MessageKindsOfTheRead::of_the_declared_kind_and_also(
            MessageKindOfTheRead::PacketsUnit,
            [MessageKindOfTheRead::RecordsMessage],
        ))
        .unwrap()
        .expect("the wired input received the record");
        assert!(received.eq(&record).unwrap(), "got {received}");

        write_the_record();
        let refusal = read_through(MessageKindsOfTheRead::of_one(
            MessageKindOfTheRead::PacketsUnit,
        ))
        .map(|_| ())
        .expect_err("a Packets input alone does not take a Records message");
        assert!(refusal.is_instance_of::<PyTypeError>(python), "{refusal}");
    });
}

/// A read through a port attribute refuses a message whose descriptor
/// kind is not one it takes, naming both.
#[test]
fn a_read_of_a_kind_refuses_a_message_described_as_another_kind() {
    Python::initialize();
    Python::attach(|python| {
        let (source, destination) = one_wired_link(python, "kind-mismatch");
        source
            .write_described_by(
                python,
                "frames_to_downstream",
                PyBytes::new(python, b"written by name").as_any(),
                None,
                true,
                |_| Ok(DataChannelPayloadDescriptor::bytes()),
            )
            .unwrap();
        let refusal = destination
            .read_from_input_port_offering_gpu_access(
                python,
                "frames_from_upstream",
                PortPayloadReadShape::OfItsKind(MessageKindsOfTheRead::of_one(
                    MessageKindOfTheRead::RecordsMessage,
                )),
                None,
            )
            .expect_err("a Bytes message is no Records message");
        assert!(refusal.to_string().contains("Records"), "{refusal}");
        assert!(refusal.to_string().contains("Bytes"), "{refusal}");
    });
}

/// A write is held to what its port declares: an Array attribute with a
/// zero extent, and a Channels block of the wrong sample type or channel
/// count, are refused before anything is published.
#[test]
fn a_write_refuses_an_array_attribute_with_no_elements_and_a_channels_block_off_its_declaration() {
    Python::initialize();
    Python::attach(|python| {
        let (source, _destination) = one_wired_link(python, "declared-shape-refusals");
        let no_gpu = PythonGpuContextLimitedAccess::new_for_helper_process(None);
        let empty_boxes = python
            .eval(c"{'boxes': memoryview(bytes(0)).cast('f')}", None, None)
            .unwrap();
        let refusal = source
            .write_array_frame_to_output_port(
                python,
                "frames_to_downstream",
                &empty_boxes,
                &[],
                None,
                &no_gpu,
                None,
                true,
            )
            .expect_err("a zero extent describes no memory");
        assert!(refusal.to_string().contains("zero dimension"), "{refusal}");

        let stereo_f32 = python
            .eval(c"memoryview(bytes(16)).cast('f', [2, 2])", None, None)
            .unwrap();
        for (declared_shape, expected) in [
            (
                ChannelsOutputPortDeclaredShape {
                    sample_type: Some(ChannelsBlockSampleType::F64),
                    channel_count: None,
                },
                "F64",
            ),
            (
                ChannelsOutputPortDeclaredShape {
                    sample_type: None,
                    channel_count: Some(1),
                },
                "1 names",
            ),
        ] {
            let refusal = source
                .write_channels_block_to_output_port(
                    python,
                    "frames_to_downstream",
                    &stereo_f32,
                    0,
                    48_000,
                    declared_shape,
                    None,
                    true,
                )
                .expect_err("the block is off the port's declaration");
            assert!(refusal.to_string().contains(expected), "{refusal}");
        }
    });
}

/// A Packets unit reads back with its bytes, its stamp, whether a decoder
/// may start at it and its codec; a plain byte stream names no codec.
#[test]
fn a_packets_unit_reads_back_with_its_stamp_group_flag_and_codec() {
    Python::initialize();
    Python::attach(|python| {
        let (source, destination) = one_wired_link(python, "packets-unit");
        for (codec, starts_group) in [("h264", false), ("", true)] {
            let descriptor =
                DataChannelPayloadDescriptor::packets_unit(&PacketsUnitDescriptorBody {
                    codec: codec.to_string(),
                })
                .unwrap();
            source
                .write_described_by(
                    python,
                    "frames_to_downstream",
                    PyBytes::new(python, b"\x00\x00\x01").as_any(),
                    Some(42),
                    starts_group,
                    |_| Ok(descriptor),
                )
                .unwrap();
        }
        for (codec, starts_group) in [("h264", false), ("", true)] {
            let unit = read_as_the_type_of_its_kind(
                python,
                &destination,
                MessageKindOfTheRead::PacketsUnit,
            );
            assert!(unit.cast::<PythonEncodedMedia>().is_ok());
            assert_eq!(
                unit.getattr("codec").unwrap().extract::<String>().unwrap(),
                codec
            );
            assert_eq!(unit.getattr("pts").unwrap().extract::<i64>().unwrap(), 42);
            assert_eq!(
                unit.getattr("starts_group")
                    .unwrap()
                    .extract::<bool>()
                    .unwrap(),
                starts_group
            );
            assert_eq!(
                PythonPortPayloadBorrow::bytes_the_read_borrowed(&unit.getattr("payload").unwrap()),
                b"\x00\x00\x01"
            );
        }
    });
}

/// `starts_group` lands in the frame header's group flag, which a Rust
/// reader on the far side of the link observes on the message it reads,
/// beside the stamp the write gave it.
#[test]
fn the_group_flag_and_the_stamp_a_write_names_reach_a_rust_reader() {
    Python::initialize();
    Python::attach(|python| {
        let (source, destination) = one_wired_link(python, "groupflag");
        for (timestamp_ns, starts_group) in [(11i64, false), (12i64, true)] {
            source
                .write_to_output_port(
                    python,
                    "frames_to_downstream",
                    PyBytes::new(python, b"access unit").as_any(),
                    Some(timestamp_ns),
                    starts_group,
                )
                .unwrap();
        }

        let input_mailboxes = destination
            .input_mailboxes
            .get()
            .expect("a wired input plane holds its mailboxes");
        for (timestamp_ns, group_boundary) in [
            (11i64, MessageGroupBoundary::ContinuesGroup),
            (12i64, MessageGroupBoundary::StartsGroup),
        ] {
            let message = input_mailboxes
                .read_raw("frames_from_upstream")
                .unwrap()
                .expect("the wired input received nothing");
            assert_eq!(message.payload, b"access unit");
            assert_eq!(message.timestamp_ns, timestamp_ns);
            assert_eq!(message.group_boundary, group_boundary);
        }
    });
}

/// A payload that exports no buffer is refused at the write, naming the
/// buffer it wants and the type it got — and refused on a declared port no
/// link has reached, too, so an author learns before the first link
/// arrives.
#[test]
fn a_write_refuses_a_payload_that_is_not_a_buffer_by_name() {
    Python::initialize();
    Python::attach(|python| {
        let (source, destination) = one_wired_link(python, "refusal");
        source.declare_ports(vec![], vec!["unlinked_output".to_string()]);
        let record = PyDict::new(python);
        record.set_item("frame_index", 7i64).unwrap();
        let not_bytes = [
            ("dict", record.into_any()),
            ("str", PyString::new(python, "frame").into_any()),
            ("int", 7i64.into_pyobject(python).unwrap().into_any()),
        ];
        for port_name in ["frames_to_downstream", "unlinked_output"] {
            for (type_name, payload) in &not_bytes {
                let refusal = source
                    .write_to_output_port(python, port_name, payload, None, true)
                    .expect_err("a payload that is not a buffer must be refused");
                assert!(
                    refusal.is_instance_of::<PyTypeError>(python),
                    "got: {refusal}"
                );
                let refusal_text = refusal.to_string();
                assert!(
                    refusal_text.contains("contiguous buffer")
                        && refusal_text.contains(&format!("got a {type_name}")),
                    "the refusal names the buffer and what it got: {refusal_text}"
                );
            }
        }
        assert!(
            destination
                .read_from_input_port(python, "frames_from_upstream")
                .unwrap()
                .is_none(),
            "a refused write publishes nothing"
        );
    });
}
