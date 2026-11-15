// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const STAGING_EXPORTING_DEVICE_UUID: &str = "00112233445566778899aabbccddeeff";

/// A consumer importing the staging's OPAQUE_FD onto its own device (a
/// viewer's CPU read) checks the exporter is that device.
#[test]
fn a_staging_registration_states_the_device_that_exported_it() {
    let payload = surface_export_staging_registration_payload(
        "surface-under-test-hostvisible",
        "runtime-under-test",
        4096,
        32,
        32,
        PixelFormat::Rgba32,
        0,
        STAGING_EXPORTING_DEVICE_UUID,
        true,
    );
    assert_eq!(
        payload
            .get(SURFACE_WIRE_FIELD_EXPORTING_DEVICE_UUID)
            .and_then(|value| value.as_str()),
        Some(STAGING_EXPORTING_DEVICE_UUID),
    );
}

#[test]
fn a_staging_registration_states_the_exporters_memory_type_index() {
    const EXPORTER_MEMORY_TYPE_INDEX: u32 = 4;
    let refill_done_timeline_crosses = true;

    let payload = surface_export_staging_registration_payload(
        "surface-under-test-hostvisible",
        "runtime-under-test",
        64 * 64 * 4,
        64,
        64,
        PixelFormat::Rgba32,
        EXPORTER_MEMORY_TYPE_INDEX,
        STAGING_EXPORTING_DEVICE_UUID,
        refill_done_timeline_crosses,
    );

    assert_eq!(
        payload
            .get("vk_memory_type_index")
            .and_then(|value| value.as_u64()),
        Some(EXPORTER_MEMORY_TYPE_INDEX as u64),
        "without this the child picks a memory type by first match, which agrees \
         with a host-cached exporter only by coincidence"
    );
    assert_eq!(
        payload.get("handle_type").and_then(|value| value.as_str()),
        Some(SURFACE_HANDLE_TYPE_OPAQUE_FD),
        "the index is only meaningful because this is an OPAQUE_FD registration"
    );
}

/// Zero is a real memory type index, so it must survive onto the wire
/// rather than reading as "absent" the way a defaulted field would.
#[test]
fn a_staging_registration_states_memory_type_index_zero_rather_than_omitting_it() {
    let refill_done_timeline_crosses = true;
    let payload = surface_export_staging_registration_payload(
        "surface-under-test-hostvisible",
        "runtime-under-test",
        4096,
        32,
        32,
        PixelFormat::Rgba32,
        0,
        STAGING_EXPORTING_DEVICE_UUID,
        refill_done_timeline_crosses,
    );
    assert_eq!(
        payload
            .get("vk_memory_type_index")
            .and_then(|value| value.as_u64()),
        Some(0),
    );
}

/// The registration's `produce_done` flag is what tells the consumer how
/// many fds the check-out carries, so it follows the timeline exactly.
#[test]
fn a_staging_registration_states_a_produce_done_edge_only_when_its_timeline_crosses() {
    let staging_registration_payload = |refill_done_timeline_crosses| {
        surface_export_staging_registration_payload(
            "surface-under-test-hostvisible",
            "runtime-under-test",
            4096,
            32,
            32,
            PixelFormat::Rgba32,
            0,
            STAGING_EXPORTING_DEVICE_UUID,
            refill_done_timeline_crosses,
        )
    };
    for refill_done_timeline_crosses in [true, false] {
        let payload = staging_registration_payload(refill_done_timeline_crosses);
        assert_eq!(
            payload
                .get(SURFACE_REPLY_HAS_PRODUCE_DONE_FD)
                .and_then(|value| value.as_bool()),
            Some(refill_done_timeline_crosses),
        );
        assert_eq!(
            payload
                .get(SURFACE_REPLY_HAS_CONSUME_DONE_FD)
                .and_then(|value| value.as_bool()),
            Some(false),
            "a staging never carries a consumer-side drain edge"
        );
    }
}
