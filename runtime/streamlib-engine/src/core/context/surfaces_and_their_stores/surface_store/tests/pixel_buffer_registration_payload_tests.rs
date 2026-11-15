// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const EXPORTING_DEVICE_UUID: &str = "00112233445566778899aabbccddeeff";

fn exported_planes_of_flavour(
    plane_flavour: ExportedPlaneFlavourOnTheWire,
) -> ExportedPlaneWireHandles {
    ExportedPlaneWireHandles {
        plane_fds: Vec::new(),
        plane_sizes: vec![64 * 32 * 4],
        plane_offsets: vec![0],
        plane_flavour,
    }
}

fn opaque_fd_exported_planes_at_memory_type_index(
    vk_memory_type_index: u32,
) -> ExportedPlaneWireHandles {
    exported_planes_of_flavour(ExportedPlaneFlavourOnTheWire::OpaqueFd(
        OpaqueFdExportContract {
            vk_memory_type_index,
            exporting_device_uuid: EXPORTING_DEVICE_UUID.to_string(),
        },
    ))
}

fn shape() -> PixelBufferShapeOnTheWire {
    PixelBufferShapeOnTheWire {
        width: 64,
        height: 32,
        format: PixelFormat::Rgba32,
    }
}

/// Every pixel-buffer payload the store sends, for one set of planes.
fn every_pixel_buffer_payload(
    exported_planes: &ExportedPlaneWireHandles,
) -> [(&'static str, serde_json::Value); 3] {
    [
        (
            "check_in",
            pixel_buffer_check_in_payload("runtime-under-test", shape(), exported_planes),
        ),
        (
            "register",
            pixel_buffer_registration_payload(
                "slot-under-test",
                "runtime-under-test",
                shape(),
                exported_planes,
                None,
            ),
        ),
        (
            "register with a timeline",
            pixel_buffer_registration_payload(
                "slot-under-test",
                "runtime-under-test",
                shape(),
                exported_planes,
                Some(AnnouncedTimelineEdges {
                    has_produce_done_fd: true,
                    has_consume_done_fd: false,
                }),
            ),
        ),
    ]
}

#[test]
fn an_opaque_fd_pixel_buffer_payload_states_its_memory_type_index_and_exporting_device() {
    for vk_memory_type_index in [0u32, 5] {
        for (payload_name, payload) in every_pixel_buffer_payload(
            &opaque_fd_exported_planes_at_memory_type_index(vk_memory_type_index),
        ) {
            assert_eq!(
                payload["handle_type"], SURFACE_HANDLE_TYPE_OPAQUE_FD,
                "{payload_name}"
            );
            assert_eq!(
                payload
                    .get("vk_memory_type_index")
                    .and_then(serde_json::Value::as_u64),
                Some(u64::from(vk_memory_type_index)),
                "the {payload_name} payload must state index {vk_memory_type_index}, zero \
                 included"
            );
            assert_eq!(
                payload
                    .get("exporting_device_uuid")
                    .and_then(serde_json::Value::as_str),
                Some(EXPORTING_DEVICE_UUID),
                "{payload_name}"
            );
        }
    }
}

/// A DMA-BUF payload carries neither OPAQUE_FD contract field, and its
/// keys keep their order.
#[test]
fn a_dma_buf_pixel_buffer_payload_carries_neither_contract_field() {
    let [check_in, register, register_with_a_timeline] = every_pixel_buffer_payload(
        &exported_planes_of_flavour(ExportedPlaneFlavourOnTheWire::DmaBuf),
    );
    assert_eq!(
        check_in.1.to_string(),
        serde_json::json!({
            "op": "check_in",
            "runtime_id": "runtime-under-test",
            "width": 64,
            "height": 32,
            "format": PixelFormat::Rgba32.wire_name(),
            "handle_type": "dma_buf",
            "plane_sizes": [64 * 32 * 4],
            "plane_offsets": [0],
        })
        .to_string()
    );
    assert_eq!(
        register.1.to_string(),
        serde_json::json!({
            "op": "register",
            "surface_id": "slot-under-test",
            "runtime_id": "runtime-under-test",
            "width": 64,
            "height": 32,
            "format": PixelFormat::Rgba32.wire_name(),
            "resource_type": SURFACE_RESOURCE_TYPE_PIXEL_BUFFER,
            "handle_type": "dma_buf",
            "plane_sizes": [64 * 32 * 4],
            "plane_offsets": [0],
        })
        .to_string()
    );
    assert_eq!(
        register_with_a_timeline.1.to_string(),
        serde_json::json!({
            "op": "register",
            "surface_id": "slot-under-test",
            "runtime_id": "runtime-under-test",
            "width": 64,
            "height": 32,
            "format": PixelFormat::Rgba32.wire_name(),
            "resource_type": SURFACE_RESOURCE_TYPE_PIXEL_BUFFER,
            "handle_type": "dma_buf",
            "plane_sizes": [64 * 32 * 4],
            "plane_offsets": [0],
            SURFACE_REPLY_HAS_PRODUCE_DONE_FD: true,
            SURFACE_REPLY_HAS_CONSUME_DONE_FD: false,
        })
        .to_string()
    );
}
