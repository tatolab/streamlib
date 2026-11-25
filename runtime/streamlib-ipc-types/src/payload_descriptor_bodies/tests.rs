// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::{DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind};

#[test]
fn a_packed_cpu_array_payload_reads_back_through_its_attribute_table() {
    let samples: Vec<u8> = (0u8..24).collect();
    let labels = [7u8, 8, 9];
    let (body, payload) = array_frame_payload_of_cpu_attributes(&[
        ArrayFrameCpuAttributeToPack {
            name: "samples",
            dtype: "float32",
            shape: &[2, 3],
            data: &samples,
        },
        ArrayFrameCpuAttributeToPack {
            name: "labels",
            dtype: "uint8",
            shape: &[3],
            data: &labels,
        },
    ])
    .unwrap();

    assert_eq!(body.attribute_count, 2);
    let rows = array_frame_attribute_table_of(body, &payload).unwrap();
    let data_of = |row: &ArrayFrameAttributeRow| match row.residency {
        ArrayFrameAttributeResidency::Cpu {
            payload_byte_offset,
            byte_len,
        } => {
            assert_eq!(
                payload_byte_offset % ARRAY_FRAME_CPU_DATA_ALIGNMENT_BYTES as u64,
                0
            );
            &payload[payload_byte_offset as usize..(payload_byte_offset + byte_len) as usize]
        }
        ArrayFrameAttributeResidency::GpuPoolSlot(_) => panic!("a CPU attribute"),
    };
    assert_eq!(
        (
            rows[0].name.as_str(),
            rows[0].dtype.as_str(),
            rows[0].shape.as_slice()
        ),
        ("samples", "float32", &[2u64, 3][..])
    );
    assert_eq!(data_of(&rows[0]), &samples[..]);
    assert_eq!(
        (
            rows[1].name.as_str(),
            rows[1].dtype.as_str(),
            rows[1].shape.as_slice()
        ),
        ("labels", "uint8", &[3u64][..])
    );
    assert_eq!(data_of(&rows[1]), &labels[..]);
}

#[test]
fn the_image_frame_body_packs_the_slot_then_size_format_and_colour_at_their_offsets() {
    assert_eq!(
        [
            IMAGE_FRAME_BODY_WIDTH_OFFSET,
            IMAGE_FRAME_BODY_HEIGHT_OFFSET,
            IMAGE_FRAME_BODY_FORMAT_OFFSET,
            IMAGE_FRAME_BODY_COLOUR_OFFSET,
            IMAGE_FRAME_BODY_USED_BYTES,
        ],
        [40, 44, 48, 52, 56]
    );
    let slot = GpuPoolSlotFrameDescriptorBody {
        pool: *b"0123456789abcdef",
        slot: 3,
        generation: 9,
        timeline_value: 11,
        image_layout: 7,
    };
    let frame = ImageFrameDescriptorBody {
        slot,
        width: 1920,
        height: 1080,
        format: 4,
        colour: ImageFrameH273Colour {
            primaries: 9,
            transfer: 16,
            matrix: 9,
            range: 0,
        },
    };
    let descriptor = DataChannelPayloadDescriptor::image_frame(frame);
    assert_eq!(
        descriptor.known_kind(),
        Ok(DataChannelPayloadDescriptorKind::ImageFrame)
    );
    assert_eq!(&descriptor.body[..40], &slot.to_descriptor_body()[..40]);
    assert_eq!(&descriptor.body[40..44], &1920u32.to_le_bytes());
    assert_eq!(&descriptor.body[44..48], &1080u32.to_le_bytes());
    assert_eq!(&descriptor.body[48..52], &4u32.to_le_bytes());
    assert_eq!(&descriptor.body[52..56], &[9, 16, 9, 0]);
    assert!(descriptor.body[56..].iter().all(|&reserved| reserved == 0));
    assert_eq!(
        ImageFrameDescriptorBody::from_descriptor_body(&descriptor.body),
        frame
    );
}

#[test]
fn the_channels_block_body_packs_every_field_at_its_documented_offset() {
    assert_eq!(
        [
            CHANNELS_BLOCK_BODY_START_SAMPLE_OFFSET,
            CHANNELS_BLOCK_BODY_SAMPLE_RATE_OFFSET,
            CHANNELS_BLOCK_BODY_CHANNEL_COUNT_OFFSET,
            CHANNELS_BLOCK_BODY_SAMPLES_PER_CHANNEL_OFFSET,
            CHANNELS_BLOCK_BODY_SAMPLE_TYPE_OFFSET,
            CHANNELS_BLOCK_BODY_USED_BYTES,
        ],
        [0, 8, 12, 16, 20, 24]
    );
    let block = ChannelsBlockDescriptorBody {
        start_sample: 0x0807_0605_0403_0201,
        sample_rate: 48_000,
        channel_count: 2,
        samples_per_channel: 960,
        sample_type: ChannelsBlockSampleType::F64 as u32,
    };
    let descriptor = DataChannelPayloadDescriptor::channels_block(block);
    assert_eq!(
        descriptor.known_kind(),
        Ok(DataChannelPayloadDescriptorKind::ChannelsBlock)
    );
    assert_eq!(&descriptor.body[0..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(&descriptor.body[8..12], &48_000u32.to_le_bytes());
    assert_eq!(&descriptor.body[12..16], &2u32.to_le_bytes());
    assert_eq!(&descriptor.body[16..20], &960u32.to_le_bytes());
    assert_eq!(&descriptor.body[20..24], &1u32.to_le_bytes());
    assert!(descriptor.body[24..].iter().all(|&reserved| reserved == 0));
    assert_eq!(
        ChannelsBlockDescriptorBody::from_descriptor_body(&descriptor.body),
        block
    );
    assert_eq!(block.planar_payload_byte_len(), Some(2 * 960 * 8));
}

#[test]
fn the_packets_unit_body_names_its_codec_zero_padded_and_a_plain_byte_stream_names_none() {
    let h264 = PacketsUnitDescriptorBody {
        codec: "h264".into(),
    };
    let descriptor = DataChannelPayloadDescriptor::packets_unit(&h264).unwrap();
    assert_eq!(
        descriptor.known_kind(),
        Ok(DataChannelPayloadDescriptorKind::PacketsUnit)
    );
    assert_eq!(&descriptor.body[..4], b"h264");
    assert!(descriptor.body[4..].iter().all(|&reserved| reserved == 0));
    assert_eq!(
        PacketsUnitDescriptorBody::from_descriptor_body(&descriptor.body),
        h264
    );

    let plain = PacketsUnitDescriptorBody {
        codec: String::new(),
    };
    let descriptor = DataChannelPayloadDescriptor::packets_unit(&plain).unwrap();
    assert!(descriptor.body.iter().all(|&byte| byte == 0));
    assert_eq!(
        PacketsUnitDescriptorBody::from_descriptor_body(&descriptor.body),
        plain
    );

    let too_long = PacketsUnitDescriptorBody {
        codec: "x".repeat(PACKETS_UNIT_BODY_CODEC_BYTES + 1),
    };
    assert!(DataChannelPayloadDescriptor::packets_unit(&too_long).is_err());
}

#[test]
fn a_records_message_names_its_kind_with_an_all_zero_body() {
    let descriptor = DataChannelPayloadDescriptor::records_message();
    assert_eq!(
        descriptor.known_kind(),
        Ok(DataChannelPayloadDescriptorKind::RecordsMessage)
    );
    assert!(descriptor.body.iter().all(|&byte| byte == 0));
}

#[test]
fn the_array_frame_body_and_its_attribute_row_pack_every_field_at_its_documented_offset() {
    assert_eq!(
        [
            ARRAY_FRAME_ATTRIBUTE_ROW_DTYPE_OFFSET,
            ARRAY_FRAME_ATTRIBUTE_ROW_DIMENSION_COUNT_OFFSET,
            ARRAY_FRAME_ATTRIBUTE_ROW_RESIDENCY_OFFSET,
            ARRAY_FRAME_ATTRIBUTE_ROW_SHAPE_OFFSET,
            ARRAY_FRAME_ATTRIBUTE_ROW_CPU_DATA_OFFSET_OFFSET,
            ARRAY_FRAME_ATTRIBUTE_ROW_CPU_DATA_BYTE_LEN_OFFSET,
            ARRAY_FRAME_ATTRIBUTE_ROW_GPU_POOL_SLOT_OFFSET,
            ARRAY_FRAME_ATTRIBUTE_ROW_BYTES,
        ],
        [32, 48, 52, 56, 120, 128, 136, 176]
    );
    let descriptor =
        DataChannelPayloadDescriptor::array_frame(ArrayFrameDescriptorBody { attribute_count: 2 });
    assert_eq!(
        descriptor.known_kind(),
        Ok(DataChannelPayloadDescriptorKind::ArrayFrame)
    );
    assert_eq!(&descriptor.body[..4], &2u32.to_le_bytes());
    assert!(descriptor.body[4..].iter().all(|&reserved| reserved == 0));

    let cpu_row = ArrayFrameAttributeRow {
        name: "boxes".into(),
        dtype: "float32".into(),
        shape: vec![10, 4],
        residency: ArrayFrameAttributeResidency::Cpu {
            payload_byte_offset: 384,
            byte_len: 160,
        },
    };
    let cpu_row_bytes = cpu_row.to_row_bytes().unwrap();
    assert_eq!(&cpu_row_bytes[..5], b"boxes");
    assert!(cpu_row_bytes[5..32].iter().all(|&padding| padding == 0));
    assert_eq!(&cpu_row_bytes[32..39], b"float32");
    assert_eq!(&cpu_row_bytes[48..52], &2u32.to_le_bytes());
    assert_eq!(&cpu_row_bytes[52..56], &0u32.to_le_bytes());
    assert_eq!(&cpu_row_bytes[56..64], &10u64.to_le_bytes());
    assert_eq!(&cpu_row_bytes[64..72], &4u64.to_le_bytes());
    assert_eq!(&cpu_row_bytes[120..128], &384u64.to_le_bytes());
    assert_eq!(&cpu_row_bytes[128..136], &160u64.to_le_bytes());
    assert_eq!(
        ArrayFrameAttributeRow::from_row_bytes(&cpu_row_bytes),
        Ok(cpu_row)
    );

    let slot = GpuPoolSlotFrameDescriptorBody {
        pool: *b"0123456789abcdef",
        slot: 2,
        generation: 5,
        timeline_value: 8,
        image_layout: 0,
    };
    let gpu_row = ArrayFrameAttributeRow {
        name: "scores".into(),
        dtype: "float16".into(),
        shape: vec![10],
        residency: ArrayFrameAttributeResidency::GpuPoolSlot(slot),
    };
    let gpu_row_bytes = gpu_row.to_row_bytes().unwrap();
    assert_eq!(&gpu_row_bytes[52..56], &1u32.to_le_bytes());
    assert_eq!(&gpu_row_bytes[136..176], &slot.to_descriptor_body()[..40]);
    assert_eq!(
        ArrayFrameAttributeRow::from_row_bytes(&gpu_row_bytes),
        Ok(gpu_row)
    );
}

#[test]
fn an_array_payload_lays_its_table_first_and_aligns_each_cpu_attribute() {
    let (offsets, payload_byte_len) = array_frame_cpu_data_offsets(2, &[160, 3]);
    assert_eq!(offsets, vec![384, 576]);
    assert_eq!(payload_byte_len, 579);

    let rows = [("boxes", 384u64, 160u64), ("labels", 576, 3)].map(|(name, offset, len)| {
        ArrayFrameAttributeRow {
            name: name.into(),
            dtype: "uint8".into(),
            shape: vec![len],
            residency: ArrayFrameAttributeResidency::Cpu {
                payload_byte_offset: offset,
                byte_len: len,
            },
        }
    });
    let mut payload = vec![0u8; payload_byte_len as usize];
    for (row_index, row) in rows.iter().enumerate() {
        let start = row_index * ARRAY_FRAME_ATTRIBUTE_ROW_BYTES;
        payload[start..start + ARRAY_FRAME_ATTRIBUTE_ROW_BYTES]
            .copy_from_slice(&row.to_row_bytes().unwrap());
    }
    let body = ArrayFrameDescriptorBody { attribute_count: 2 };
    assert_eq!(
        array_frame_attribute_table_of(body, &payload).unwrap(),
        rows.to_vec()
    );
    assert!(array_frame_attribute_table_of(body, &payload[..578]).is_err());
    assert!(
        array_frame_attribute_table_of(ArrayFrameDescriptorBody { attribute_count: 4 }, &payload)
            .is_err()
    );

    let mut over_the_table = rows[1].clone();
    over_the_table.residency = ArrayFrameAttributeResidency::Cpu {
        payload_byte_offset: 16,
        byte_len: 3,
    };
    payload[ARRAY_FRAME_ATTRIBUTE_ROW_BYTES..2 * ARRAY_FRAME_ATTRIBUTE_ROW_BYTES]
        .copy_from_slice(&over_the_table.to_row_bytes().unwrap());
    let refusal = array_frame_attribute_table_of(body, &payload).unwrap_err();
    assert!(refusal.reason.contains("inside"), "{}", refusal.reason);
}

#[test]
fn an_attribute_row_refuses_a_name_that_does_not_fit_and_a_shape_past_eight_dimensions() {
    let row = |name: &str, shape: Vec<u64>| ArrayFrameAttributeRow {
        name: name.into(),
        dtype: "float32".into(),
        shape,
        residency: ArrayFrameAttributeResidency::Cpu {
            payload_byte_offset: 0,
            byte_len: 0,
        },
    };
    assert!(row(&"x".repeat(33), vec![1]).to_row_bytes().is_err());
    assert!(row("", vec![1]).to_row_bytes().is_err());
    assert!(row("x", vec![1; 9]).to_row_bytes().is_err());
    assert!(row("x", vec![]).to_row_bytes().is_err());
}
