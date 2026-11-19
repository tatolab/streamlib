// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Value, json};
use streamlib_ipc_types::{
    ArrayFrameAttributeResidency, ArrayFrameDescriptorBody, ChannelsBlockDescriptorBody,
    ChannelsBlockSampleType, DataChannelPayloadDescriptorKind, array_frame_attribute_table_of,
};
use streamlib_node_schema::pin_type::VectorComponentCount;

use super::*;

fn vector(component_count: VectorComponentCount) -> PinType {
    PinType::Vector { component_count }
}

/// The bytes of a control message of `channel_values`: each an f64, little-endian, in order.
fn golden_bytes(channel_values: &[f64]) -> Vec<u8> {
    channel_values
        .iter()
        .flat_map(|channel_value| channel_value.to_le_bytes())
        .collect()
}

fn the_block_of(packed: &PackedPinValue) -> ChannelsBlockDescriptorBody {
    ChannelsBlockDescriptorBody::from_descriptor_body(&packed.payload_descriptor.body)
}

#[test]
fn each_small_type_packs_as_one_f64_sample_on_each_of_its_parts_at_rate_0() {
    let packed_as = [
        (PinType::Boolean {}, json!(true), vec![1.0]),
        (PinType::Integer { exact_size: None }, json!(-3), vec![-3.0]),
        (PinType::Float { exact_size: None }, json!(1), vec![1.0]),
        (
            PinType::Color {},
            json!({"red": 1.0, "green": 0.5, "blue": 0.2, "alpha": 0.75}),
            vec![1.0, 0.5, 0.2, 0.75],
        ),
        (
            vector(VectorComponentCount::Two),
            json!({"y": 0.25, "x": 0.5}),
            vec![0.5, 0.25],
        ),
        (
            vector(VectorComponentCount::Three),
            json!({"x": 1, "y": 2, "z": 3}),
            vec![1.0, 2.0, 3.0],
        ),
        (
            vector(VectorComponentCount::Four),
            json!({"x": 1, "y": 2, "z": 3, "w": 4}),
            vec![1.0, 2.0, 3.0, 4.0],
        ),
        (
            PinType::Rectangle {},
            json!({"x": 80, "y": 800, "width": 640, "height": 360}),
            vec![80.0, 800.0, 640.0, 360.0],
        ),
        (
            PinType::Transform {},
            json!([1, 0, 0, 5, 0, 1, 0, 6, 0, 0, 1, 7, 0, 0, 0, 1]),
            vec![
                1.0, 0.0, 0.0, 5.0, 0.0, 1.0, 0.0, 6.0, 0.0, 0.0, 1.0, 7.0, 0.0, 0.0, 0.0, 1.0,
            ],
        ),
    ];
    for (pin_type, value, parts) in packed_as {
        let packed = pack_pin_value(&pin_type, &value, 12).unwrap();
        assert_eq!(packed.payload, golden_bytes(&parts), "{pin_type}");
        assert_eq!(
            the_block_of(&packed),
            ChannelsBlockDescriptorBody {
                start_sample: 12,
                sample_rate: 0,
                channel_count: parts.len() as u32,
                samples_per_channel: 1,
                sample_type: ChannelsBlockSampleType::F64 as u32,
            },
            "{pin_type}"
        );
        assert_eq!(
            message_kind_of(&pin_type),
            Some(DataChannelPayloadDescriptorKind::ChannelsBlock)
        );
        assert_eq!(
            unpack_pin_value(&pin_type, packed.as_ref()).unwrap(),
            the_stored_value_of_channel_values(&pin_type, &parts),
            "{pin_type}"
        );
    }
}

#[test]
fn each_stored_value_unpacks_to_its_own_json_form() {
    for (pin_type, value) in [
        (PinType::Boolean {}, json!(false)),
        (PinType::Integer { exact_size: None }, json!(7)),
        (PinType::Float { exact_size: None }, json!(0.25)),
        (
            PinType::Color {},
            json!({"red": 1.0, "green": 0.5, "blue": 0.2, "alpha": 1.0}),
        ),
        (
            vector(VectorComponentCount::Three),
            json!({"x": 1.0, "y": 2.0, "z": 3.0}),
        ),
        (
            PinType::Rectangle {},
            json!({"x": 0.0, "y": 0.0, "width": 1.0, "height": 0.5}),
        ),
    ] {
        let packed = pack_pin_value(&pin_type, &value, 0).unwrap();
        assert_eq!(unpack_pin_value(&pin_type, packed.as_ref()).unwrap(), value);
    }
}

#[test]
fn a_value_of_the_wrong_shape_does_not_pack_and_nothing_is_filled_in() {
    for (pin_type, value) in [
        (
            PinType::Color {},
            json!({"red": 1.0, "green": 0.5, "blue": 0.2}),
        ),
        (
            PinType::Color {},
            json!({"red": 1.0, "green": 0.5, "blue": 0.2, "alpha": 1.0, "hue": 3}),
        ),
        (
            vector(VectorComponentCount::Two),
            json!({"x": 1, "y": 2, "z": 3}),
        ),
        (vector(VectorComponentCount::Three), json!({"x": 1, "y": 2})),
        (
            PinType::Rectangle {},
            json!({"x": 0, "y": 0, "width": -1, "height": 1}),
        ),
        (PinType::Transform {}, json!([1, 0, 0, 0])),
        (PinType::Float { exact_size: None }, json!("loud")),
    ] {
        let refusal = pack_pin_value(&pin_type, &value, 0).unwrap_err();
        assert!(refusal.contains("is not"), "{pin_type}: {refusal}");
    }
}

#[test]
fn a_colour_number_outside_0_to_1_is_packed_as_sent() {
    let packed = pack_pin_value(
        &PinType::Color {},
        &json!({"red": 2.0, "green": -0.5, "blue": 0.0, "alpha": 1.0}),
        0,
    )
    .unwrap();
    assert_eq!(packed.payload, golden_bytes(&[2.0, -0.5, 0.0, 1.0]));
}

#[test]
fn a_free_conversion_pads_a_missing_component_with_0_and_a_missing_alpha_with_1() {
    let a_vector2 = pack_pin_value(
        &vector(VectorComponentCount::Two),
        &json!({"x": 0.5, "y": 0.25}),
        0,
    )
    .unwrap();
    let read_as = |pin_type: &PinType, message: &PackedPinValue| {
        the_channel_values_a_pin_type_reads(
            pin_type,
            &the_channel_values_a_control_message_carries(message.as_ref()).unwrap(),
        )
        .unwrap()
    };
    assert_eq!(
        read_as(&PinType::Color {}, &a_vector2),
        [0.5, 0.25, 0.0, 1.0]
    );
    assert_eq!(
        read_as(&vector(VectorComponentCount::Four), &a_vector2),
        [0.5, 0.25, 0.0, 0.0]
    );
    let a_vector4 = control_message_of_channel_values(&[1.0, 2.0, 3.0, 0.5], 0);
    assert_eq!(
        read_as(&PinType::Color {}, &a_vector4),
        [1.0, 2.0, 3.0, 0.5]
    );
    assert_eq!(
        read_as(&vector(VectorComponentCount::Two), &a_vector4),
        [1.0, 2.0]
    );
    let a_number = pack_pin_value(&PinType::Float { exact_size: None }, &json!(0.5), 0).unwrap();
    assert_eq!(read_as(&PinType::Color {}, &a_number), [0.5, 0.5, 0.5, 1.0]);
    assert_eq!(
        read_as(&vector(VectorComponentCount::Three), &a_number),
        [0.5, 0.5, 0.5]
    );
    assert_eq!(
        unpack_pin_value(&PinType::Color {}, a_vector2.as_ref()).unwrap(),
        json!({"red": 0.5, "green": 0.25, "blue": 0.0, "alpha": 1.0})
    );
}

#[test]
fn a_rectangle_and_a_transform_read_exactly_their_own_size() {
    let a_vector2 = control_message_of_channel_values(&[0.5, 0.25], 0);
    for pin_type in [PinType::Rectangle {}, PinType::Transform {}] {
        let refusal = unpack_pin_value(&pin_type, a_vector2.as_ref()).unwrap_err();
        assert!(refusal.contains("carries 2"), "{refusal}");
    }
}

#[test]
fn a_number_reads_the_first_sample_of_an_f32_block_as_an_audio_block_carries_it() {
    let audio = pack_pin_value(
        &PinType::Audio {},
        &json!({"samples": [[0.5, 0.25], [-1.0, 1.0]], "sample_rate": 48000}),
        0,
    )
    .unwrap();
    assert_eq!(
        the_channel_values_a_control_message_carries(audio.as_ref()).unwrap(),
        [0.5, -1.0]
    );
    assert_eq!(
        unpack_pin_value(&PinType::Float { exact_size: None }, audio.as_ref()).unwrap(),
        json!(0.5)
    );
    let records =
        the_message_a_value_written_onto_an_output_is(PortFamily::Records, &json!({"level": 1}), 0)
            .unwrap();
    assert!(
        the_channel_values_a_control_message_carries(records.as_ref())
            .unwrap_err()
            .contains("no Channels block")
    );
}

#[test]
fn an_integer_reads_a_float_rounded_half_away_from_zero() {
    let message = control_message_of_channel_values(&[-2.5], 0);
    assert_eq!(
        unpack_pin_value(&PinType::Integer { exact_size: None }, message.as_ref()).unwrap(),
        json!(-3)
    );
}

#[test]
fn each_type_names_the_kind_of_message_it_travels_as() {
    let kind_of = |pin_type: Value| message_kind_of(&serde_json::from_value(pin_type).unwrap());
    assert_eq!(
        kind_of(json!({"type": "tensor"})),
        Some(DataChannelPayloadDescriptorKind::ArrayFrame)
    );
    assert_eq!(
        kind_of(json!({"type": "list_of", "element_type": {"type": "color"}})),
        Some(DataChannelPayloadDescriptorKind::ArrayFrame)
    );
    assert_eq!(
        kind_of(json!({"type": "list_of", "element_type": {"type": "string"}})),
        Some(DataChannelPayloadDescriptorKind::RecordsMessage)
    );
    assert_eq!(
        kind_of(json!({"type": "trigger"})),
        Some(DataChannelPayloadDescriptorKind::RecordsMessage)
    );
    assert_eq!(
        kind_of(json!({"type": "texture"})),
        Some(DataChannelPayloadDescriptorKind::ImageFrame)
    );
    assert_eq!(kind_of(json!({"type": "pin_type_choice"})), None);
}

#[test]
fn a_record_written_onto_a_records_output_is_its_msgpack_map() {
    let PackedPinValue {
        payload_descriptor: descriptor,
        payload,
    } = the_message_a_value_written_onto_an_output_is(
        PortFamily::Records,
        &json!({ "cue": "drop", "beat": 33 }),
        0,
    )
    .unwrap();
    assert_eq!(
        descriptor.known_kind().ok(),
        Some(DataChannelPayloadDescriptorKind::RecordsMessage)
    );
    let read_back: Value = rmp_serde::from_slice(&payload).unwrap();
    assert_eq!(read_back, json!({ "cue": "drop", "beat": 33 }));

    let refusal = the_message_a_value_written_onto_an_output_is(PortFamily::Records, &json!(3), 0)
        .unwrap_err();
    assert!(refusal.contains("JSON object"), "{refusal}");
}

#[test]
fn a_number_or_nested_list_written_onto_an_array_output_is_its_values_attribute() {
    let PackedPinValue {
        payload_descriptor: descriptor,
        payload,
    } = the_message_a_value_written_onto_an_output_is(
        PortFamily::Array,
        &json!([[0.5, -1.0], [0.25, 1.0], [0.0, 0.0]]),
        0,
    )
    .unwrap();
    let rows = array_frame_attribute_table_of(
        ArrayFrameDescriptorBody::from_descriptor_body(&descriptor.body),
        &payload,
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME);
    assert_eq!(rows[0].dtype, "float64");
    assert_eq!(rows[0].shape, vec![3, 2]);
    let ArrayFrameAttributeResidency::Cpu {
        payload_byte_offset,
        byte_len,
    } = rows[0].residency
    else {
        panic!("a CPU attribute");
    };
    let elements: Vec<f64> = payload
        [payload_byte_offset as usize..(payload_byte_offset + byte_len) as usize]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|element| f64::from_le_bytes(*element))
        .collect();
    assert_eq!(elements, [0.5, -1.0, 0.25, 1.0, 0.0, 0.0]);

    let PackedPinValue {
        payload_descriptor: descriptor,
        payload,
    } = the_message_a_value_written_onto_an_output_is(PortFamily::Array, &json!(0.75), 0).unwrap();
    let rows = array_frame_attribute_table_of(
        ArrayFrameDescriptorBody::from_descriptor_body(&descriptor.body),
        &payload,
    )
    .unwrap();
    assert_eq!(rows[0].shape, vec![1]);

    let PackedPinValue {
        payload_descriptor: descriptor,
        payload,
    } = the_message_a_value_written_onto_an_output_is(
        PortFamily::Array,
        &json!({ "left": [1, 2], "right": 3 }),
        0,
    )
    .unwrap();
    let rows = array_frame_attribute_table_of(
        ArrayFrameDescriptorBody::from_descriptor_body(&descriptor.body),
        &payload,
    )
    .unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| (row.name.as_str(), row.shape.clone()))
            .collect::<Vec<_>>(),
        [("left", vec![2]), ("right", vec![1])]
    );

    let refusal =
        the_message_a_value_written_onto_an_output_is(PortFamily::Array, &json!([[1, 2], [3]]), 0)
            .unwrap_err();
    assert!(refusal.contains("differ in length"), "{refusal}");
}

#[test]
fn a_number_written_onto_a_channels_output_is_a_control_message_and_a_block_runs_on() {
    let PackedPinValue {
        payload_descriptor: descriptor,
        payload,
    } = the_message_a_value_written_onto_an_output_is(PortFamily::Channels, &json!(0.5), 96)
        .unwrap();
    assert_eq!(
        ChannelsBlockDescriptorBody::from_descriptor_body(&descriptor.body),
        ChannelsBlockDescriptorBody {
            start_sample: 96,
            sample_rate: 0,
            channel_count: 1,
            samples_per_channel: 1,
            sample_type: ChannelsBlockSampleType::F64 as u32,
        }
    );
    assert_eq!(payload, 0.5f64.to_le_bytes());

    let PackedPinValue {
        payload_descriptor: descriptor,
        payload,
    } = the_message_a_value_written_onto_an_output_is(
        PortFamily::Channels,
        &json!({ "samples": [[0.0, 0.5], [1.0, -1.0]], "sample_rate": 48000 }),
        0,
    )
    .unwrap();
    let body = ChannelsBlockDescriptorBody::from_descriptor_body(&descriptor.body);
    assert_eq!(
        (
            body.sample_rate,
            body.channel_count,
            body.samples_per_channel
        ),
        (48_000, 2, 2)
    );
    assert_eq!(body.planar_payload_byte_len(), Some(payload.len()));

    let refusal = the_message_a_value_written_onto_an_output_is(
        PortFamily::Channels,
        &json!({ "samples": [[0.0], [1.0, 2.0]] }),
        0,
    )
    .unwrap_err();
    assert!(refusal.contains("same number of samples"), "{refusal}");
}

#[test]
fn an_image_or_packets_value_is_never_written_from_a_panel() {
    for family in [PortFamily::Image, PortFamily::Packets] {
        let refusal =
            the_message_a_value_written_onto_an_output_is(family, &json!({}), 0).unwrap_err();
        assert!(refusal.contains("never"), "{refusal}");
    }
}

#[test]
fn a_client_writes_array_channels_and_records_values_alone() {
    assert_eq!(
        why_a_client_never_writes(PortFamily::Packets),
        "a client writes Array, Channels and Records values, never Packets"
    );
}

fn a_list_of(element_type: PinType) -> PinType {
    PinType::ListOf {
        element_type: Box::new(element_type),
    }
}

fn a_menu(option_names: &[&str]) -> PinType {
    PinType::Menu {
        option_names: option_names.iter().map(|name| (*name).to_owned()).collect(),
    }
}

/// The msgpack bytes of the Records message `{"value": value}`, written out by hand.
fn golden_records_bytes(value_bytes: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x81, 0xa5];
    bytes.extend_from_slice(b"value");
    bytes.extend_from_slice(value_bytes);
    bytes
}

#[test]
fn a_string_a_menu_and_an_asset_pack_as_a_records_message_of_their_value() {
    let asset = "0eaf9ee2-0f73-4154-a3f6-a99c261b850d";
    let mut asset_bytes = vec![0x81, 0xa5];
    asset_bytes.extend_from_slice(b"asset");
    asset_bytes.push(0xd9);
    asset_bytes.push(asset.len() as u8);
    asset_bytes.extend_from_slice(asset.as_bytes());
    let packed_as = [
        (PinType::String {}, json!("title"), {
            let mut bytes = vec![0xa5];
            bytes.extend_from_slice(b"title");
            bytes
        }),
        (PinType::String {}, json!(""), vec![0xa0]),
        (a_menu(&["sine", "square"]), json!("sine"), {
            let mut bytes = vec![0xa4];
            bytes.extend_from_slice(b"sine");
            bytes
        }),
        (
            PinType::Asset { media: vec![] },
            json!({ "asset": asset }),
            asset_bytes,
        ),
        (a_list_of(PinType::String {}), json!([]), vec![0x90]),
    ];
    for (pin_type, value, value_bytes) in packed_as {
        let packed = pack_pin_value(&pin_type, &value, 0).unwrap();
        assert_eq!(
            DataChannelPayloadDescriptorKind::from_wire(packed.payload_descriptor.kind),
            Some(DataChannelPayloadDescriptorKind::RecordsMessage),
            "{pin_type}"
        );
        assert_eq!(
            packed.payload,
            golden_records_bytes(&value_bytes),
            "{pin_type}"
        );
        assert_eq!(
            unpack_pin_value(&pin_type, packed.as_ref()),
            Ok(value),
            "{pin_type}"
        );
    }
}

#[test]
fn a_list_of_floats_packs_as_one_float64_attribute_of_shape_n() {
    let list_of_floats = a_list_of(PinType::Float { exact_size: None });
    let packed = pack_pin_value(&list_of_floats, &json!([0.1, 0.2, 3]), 0).unwrap();
    let body = ArrayFrameDescriptorBody::from_descriptor_body(&packed.payload_descriptor.body);
    let [row] = array_frame_attribute_table_of(body, &packed.payload)
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(
        (row.name.as_str(), row.dtype.as_str(), row.shape.as_slice()),
        ("values", "float64", &[3][..])
    );
    let ArrayFrameAttributeResidency::Cpu {
        payload_byte_offset,
        byte_len,
    } = row.residency
    else {
        panic!("a List is packed on the CPU");
    };
    assert_eq!(
        &packed.payload[payload_byte_offset as usize..][..byte_len as usize],
        golden_bytes(&[0.1, 0.2, 3.0]).as_slice()
    );
    assert_eq!(
        unpack_pin_value(&list_of_floats, packed.as_ref()),
        Ok(json!([0.1, 0.2, 3.0]))
    );
}

#[test]
fn a_list_of_values_of_parts_packs_as_shape_n_by_parts_and_travels_unchanged() {
    let list_of_colours = a_list_of(PinType::Color {});
    let colours = json!([
        {"red": 1.0, "green": 0.5, "blue": 0.25, "alpha": 1.0},
        {"red": 0.0, "green": 0.0, "blue": 0.0, "alpha": 0.5},
    ]);
    let packed = pack_pin_value(&list_of_colours, &colours, 0).unwrap();
    let body = ArrayFrameDescriptorBody::from_descriptor_body(&packed.payload_descriptor.body);
    let rows = array_frame_attribute_table_of(body, &packed.payload).unwrap();
    assert_eq!(rows[0].shape, vec![2, 4]);
    assert_eq!(
        unpack_pin_value(&list_of_colours, packed.as_ref()),
        Ok(colours)
    );

    let empty = pack_pin_value(&list_of_colours, &json!([]), 0).unwrap();
    assert_eq!(
        unpack_pin_value(&list_of_colours, empty.as_ref()),
        Ok(json!([]))
    );
}

#[test]
fn a_list_of_one_element_type_is_refused_on_a_list_of_another() {
    let list_of_floats = a_list_of(PinType::Float { exact_size: None });
    let refusal = pack_pin_value(&list_of_floats, &json!(["loud"]), 0).unwrap_err();
    assert!(refusal.contains("each element a number"), "{refusal}");
    let strings = pack_pin_value(&a_list_of(PinType::String {}), &json!(["loud"]), 0).unwrap();
    let refusal = unpack_pin_value(&list_of_floats, strings.as_ref()).unwrap_err();
    assert!(refusal.contains("Array frame"), "{refusal}");
    let refusal = unpack_pin_value(&a_list_of(a_menu(&["sine"])), strings.as_ref()).unwrap_err();
    assert!(refusal.contains("`sine`"), "{refusal}");
}

#[test]
fn a_menu_value_that_is_none_of_its_options_is_refused_with_the_options() {
    let waveform = a_menu(&["sine", "square"]);
    let refusal = pack_pin_value(&waveform, &json!("saw"), 0).unwrap_err();
    assert!(refusal.contains("one of `sine`, `square`"), "{refusal}");
    let saw = pack_pin_value(&PinType::String {}, &json!("saw"), 0).unwrap();
    let refusal = unpack_pin_value(&waveform, saw.as_ref()).unwrap_err();
    assert!(refusal.contains("one of `sine`, `square`"), "{refusal}");
    let sine = pack_pin_value(&waveform, &json!("sine"), 0).unwrap();
    assert_eq!(
        unpack_pin_value(&PinType::String {}, sine.as_ref()),
        Ok(json!("sine")),
        "a Menu feeds a String input as its option name"
    );
}

#[test]
fn an_asset_takes_only_the_object_form() {
    let asset = PinType::Asset { media: vec![] };
    for refused in [
        json!("0eaf9ee2-0f73-4154-a3f6-a99c261b850d"),
        json!({"asset": "0eaf9ee2-0f73-4154-a3f6-a99c261b850d", "kind": "image"}),
        json!({"asset": "not a uuid"}),
    ] {
        assert!(pack_pin_value(&asset, &refused, 0).is_err(), "{refused}");
    }
    let imported = json!({
        "asset": "0eaf9ee2-0f73-4154-a3f6-a99c261b850d",
        "from": "5f0c3b1e-27a4-4bd3-9b51-6f0d5bd0b7a2",
    });
    let packed = pack_pin_value(&asset, &imported, 0).unwrap();
    assert_eq!(unpack_pin_value(&asset, packed.as_ref()), Ok(imported));
}
