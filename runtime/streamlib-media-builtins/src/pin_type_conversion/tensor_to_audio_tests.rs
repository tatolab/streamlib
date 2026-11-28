// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::iceoryx2::{
    ArrayFrameAttributeResidency, ArrayFrameAttributeRow, ArrayFrameCpuAttributeToPack,
    ArrayFrameDescriptorBody, ChannelsBlockDescriptorBody, ChannelsBlockSampleType,
    DataChannelPayloadDescriptor, GpuPoolSlotFrameDescriptorBody,
    array_frame_payload_of_cpu_attributes,
};

use super::tensor_to_audio::*;
use crate::pin_type_conversion::audio_to_tensor::array_frame_of_the_channels_block;

fn stereo_at_48_khz() -> TensorToAudioInputs {
    TensorToAudioInputs {
        channel_names: vec!["left".into(), "right".into()],
        sample_rate: 48_000,
        attribute: "samples".into(),
    }
}

fn array_frame_of(
    attributes: &[ArrayFrameCpuAttributeToPack<'_>],
) -> (DataChannelPayloadDescriptor, Vec<u8>) {
    let (body, payload) = array_frame_payload_of_cpu_attributes(attributes).unwrap();
    (DataChannelPayloadDescriptor::array_frame(body), payload)
}

/// The node catalog lists `TensorToAudio`'s value inputs, as it registers, after its `array`
/// port in the order it declares them, each an input taking no wire read each run
/// with its default: `attribute` a String, `channel_names` a List of String and `sample_rate`
/// a 32-bit unsigned Integer. It takes no config.
///
/// Mental-revert: rendering only the declared inputs leaves `array` alone in `inputs`.
#[test]
fn the_catalog_lists_its_value_inputs_as_inputs_taking_no_wire_after_its_port() {
    crate::register_media_builtin_node_types();
    let descriptor = streamlib::sdk::nodes::NODE_REGISTRY
        .descriptor(&TensorToAudio::node_class_import_path())
        .expect("the built-in registers");
    let rendered = serde_json::to_value(streamlib::sdk::json_schema::NodeDescriptorOutput::from(
        &descriptor,
    ))
    .unwrap();
    let inputs = rendered["inputs"].as_array().unwrap();
    assert_eq!(
        inputs
            .iter()
            .map(|input| input["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["array", "attribute", "channel_names", "sample_rate"],
        "{rendered}"
    );
    let setting = |name: &str| inputs.iter().find(|input| input["name"] == name).unwrap();
    for name in ["channel_names", "sample_rate", "attribute"] {
        assert_eq!(
            setting(name)["input_mode"],
            serde_json::json!({"mode": "setting_only"}),
            "{name}"
        );
    }
    assert_eq!(
        (
            &setting("sample_rate")["pin_type"]["pin_type"],
            &setting("sample_rate")["default_value"]
        ),
        (
            &serde_json::json!({"type": "integer", "exact_size": "uint32"}),
            &serde_json::json!(48_000)
        )
    );
    assert_eq!(
        (
            &setting("attribute")["pin_type"]["pin_type"],
            &setting("attribute")["default_value"]
        ),
        (
            &serde_json::json!({"type": "string"}),
            &serde_json::json!("samples")
        )
    );
    assert_eq!(
        setting("channel_names")["pin_type"]["pin_type"],
        serde_json::json!({"type": "list_of", "element_type": {"type": "string"}}),
        "{rendered}"
    );
    assert_eq!(
        setting("channel_names")["default_value"],
        serde_json::json!(["value"])
    );
    assert!(
        rendered.get("exposable_config_fields").is_none(),
        "a natively compiled type exposes no config field: {rendered}"
    );
    assert!(
        descriptor
            .config_schema
            .as_ref()
            .and_then(|config_schema| config_schema.get("properties"))
            .is_none(),
        "it takes no config: {rendered}"
    );
}

#[test]
fn a_channels_block_converted_to_an_array_and_back_keeps_its_samples() {
    let samples: Vec<u8> = [0.25f64, 0.5, -0.25, -0.5]
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    let block_descriptor =
        DataChannelPayloadDescriptor::channels_block(ChannelsBlockDescriptorBody {
            start_sample: 0,
            sample_rate: 48_000,
            channel_count: 2,
            samples_per_channel: 2,
            sample_type: ChannelsBlockSampleType::F64 as u32,
        });
    let (array_descriptor, array_payload) =
        array_frame_of_the_channels_block(block_descriptor, &samples).unwrap();

    let (block_descriptor_back, samples_back) = channels_block_of_the_array_frame(
        &stereo_at_48_khz(),
        480,
        array_descriptor,
        &array_payload,
    )
    .unwrap();

    assert_eq!(samples_back, samples);
    assert_eq!(
        ChannelsBlockDescriptorBody::from_descriptor_body(&block_descriptor_back.body),
        ChannelsBlockDescriptorBody {
            start_sample: 480,
            sample_rate: 48_000,
            channel_count: 2,
            samples_per_channel: 2,
            sample_type: ChannelsBlockSampleType::F64 as u32,
        }
    );
}

#[test]
fn the_attribute_its_input_names_is_the_one_read() {
    let gains = [1.0f32, 2.0, 3.0, 4.0];
    let gain_bytes: Vec<u8> = gains.iter().flat_map(|gain| gain.to_le_bytes()).collect();
    let (descriptor, payload) = array_frame_of(&[
        ArrayFrameCpuAttributeToPack {
            name: "labels",
            dtype: "uint8",
            shape: &[2],
            data: &[1, 2],
        },
        ArrayFrameCpuAttributeToPack {
            name: "gains",
            dtype: "float32",
            shape: &[2, 2],
            data: &gain_bytes,
        },
    ]);
    let held = TensorToAudioInputs {
        attribute: "gains".into(),
        ..stereo_at_48_khz()
    };

    let (_, samples) = channels_block_of_the_array_frame(&held, 0, descriptor, &payload).unwrap();

    assert_eq!(samples, gain_bytes);
}

#[test]
fn a_frame_without_the_attribute_is_refused_naming_the_attributes_it_has() {
    let (descriptor, payload) = array_frame_of(&[ArrayFrameCpuAttributeToPack {
        name: "boxes",
        dtype: "float32",
        shape: &[2, 1],
        data: &[0; 8],
    }]);

    let refusal = channels_block_of_the_array_frame(&stereo_at_48_khz(), 0, descriptor, &payload)
        .unwrap_err()
        .to_string();

    assert!(refusal.contains("\"samples\""), "{refusal}");
    assert!(refusal.contains("\"boxes\""), "{refusal}");
}

#[test]
fn an_attribute_of_another_dtype_or_channel_count_is_refused() {
    let (int_descriptor, int_payload) = array_frame_of(&[ArrayFrameCpuAttributeToPack {
        name: "samples",
        dtype: "int32",
        shape: &[2, 1],
        data: &[0; 8],
    }]);
    let (mono_descriptor, mono_payload) = array_frame_of(&[ArrayFrameCpuAttributeToPack {
        name: "samples",
        dtype: "float32",
        shape: &[1, 2],
        data: &[0; 8],
    }]);

    let dtype_refusal =
        channels_block_of_the_array_frame(&stereo_at_48_khz(), 0, int_descriptor, &int_payload)
            .unwrap_err()
            .to_string();
    let channel_count_refusal =
        channels_block_of_the_array_frame(&stereo_at_48_khz(), 0, mono_descriptor, &mono_payload)
            .unwrap_err()
            .to_string();

    assert!(dtype_refusal.contains("int32"), "{dtype_refusal}");
    assert!(
        channel_count_refusal.contains("holds 1 channels"),
        "{channel_count_refusal}"
    );
}

#[test]
fn a_gpu_resident_attribute_is_refused() {
    let row = ArrayFrameAttributeRow {
        name: "samples".into(),
        dtype: "float32".into(),
        shape: vec![2, 4],
        residency: ArrayFrameAttributeResidency::GpuPoolSlot(GpuPoolSlotFrameDescriptorBody {
            pool: [7; 16],
            slot: 0,
            generation: 1,
            timeline_value: 0,
            image_layout: 0,
        }),
    };
    let payload = row.to_row_bytes().unwrap().to_vec();
    let descriptor =
        DataChannelPayloadDescriptor::array_frame(ArrayFrameDescriptorBody { attribute_count: 1 });

    let refusal = channels_block_of_the_array_frame(&stereo_at_48_khz(), 0, descriptor, &payload)
        .unwrap_err()
        .to_string();

    assert!(refusal.contains("GPU-resident"), "{refusal}");
}
