// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::time::Duration;

use objc2_io_surface::IOSurfaceRef;
use streamlib_surface_client::SurfaceShareMachServiceConnection;

use crate::apple::surface_share::{IOSurfaceShareState, MachSurfaceShareService};
use crate::core::context::{GpuContext, SurfaceStore};
use crate::core::rhi::{PixelFormat, pool_slot_key_of_surface_id};

fn gpu_or_skip() -> Option<GpuContext> {
    match GpuContext::init_for_platform() {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            tracing::warn!("skipping — no GPU device: {e}");
            None
        }
    }
}

fn engine_pattern_byte(index: usize) -> u8 {
    (index.wrapping_mul(13).wrapping_add(5)) as u8
}

/// A pooled frame is an IOSurface the service hands out: another
/// connection checks the published frame id out, and the surface its port
/// names holds the pixels the producer wrote into the pooled buffer.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_pooled_frame_crosses_the_mach_service_as_the_iosurface_holding_its_pixels() {
    let Some(gpu) = gpu_or_skip() else {
        return;
    };
    let state = IOSurfaceShareState::new();
    let mut service = MachSurfaceShareService::new(
        state.clone(),
        format!(
            "com.tatolab.streamlib.surface-share-test.pool.{}",
            std::process::id()
        ),
    );
    service.start().expect("the service starts");
    let store = SurfaceStore::new_reading_check_out_leases(
        service.service_name().to_string(),
        "R-pool-test".to_string(),
        std::sync::Arc::clone(state.check_out_leases()),
    );
    store.connect().expect("the store connects");
    gpu.install_the_engines_surface_store(store)
        .expect("the engine's surface store installs once");

    let (frame_id, pooled_buffer) = gpu
        .acquire_pixel_buffer(64, 32, PixelFormat::Bgra32)
        .expect("a pooled frame");
    let pooled_bytes = unsafe {
        std::slice::from_raw_parts_mut(pooled_buffer.buffer_ref().inner.mapped_ptr(), 64 * 32 * 4)
    };
    for (index, byte) in pooled_bytes.iter_mut().enumerate() {
        *byte = engine_pattern_byte(index);
    }
    assert!(
        state
            .surface_ids()
            .contains(&pool_slot_key_of_surface_id(frame_id.to_string().as_str()).to_string()),
        "the pool registered its slot with the service"
    );

    let reader =
        SurfaceShareMachServiceConnection::connect(service.service_name(), Duration::from_secs(10))
            .expect("a reader connects");
    let (answer, ports) = reader
        .send_request_with_ports(
            &serde_json::json!({"op": "check_out", "surface_id": frame_id.to_string()}),
            Vec::new(),
        )
        .expect("check_out round-trip");
    assert!(answer.get("error").is_none(), "{answer}");
    assert_eq!(answer["plane_strides"], serde_json::json!([64 * 4]));
    let iosurface = IOSurfaceRef::lookup_from_mach_port(ports[0].as_raw_name())
        .expect("the port names the slot's IOSurface");
    let shared_bytes = unsafe {
        std::slice::from_raw_parts(iosurface.base_address().as_ptr().cast::<u8>(), 64 * 32 * 4)
    };
    assert!(
        shared_bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| *byte == engine_pattern_byte(index)),
        "the shared surface holds the pooled buffer's pixels"
    );
}

/// A started service with this process's store connected to it, the way
/// the runtime's `start()` builds one.
fn a_store_connected_to_a_started_service(
    gpu: &GpuContext,
    label: &str,
) -> (IOSurfaceShareState, MachSurfaceShareService) {
    let state = IOSurfaceShareState::new();
    let mut service = MachSurfaceShareService::new(
        state.clone(),
        format!(
            "com.tatolab.streamlib.surface-share-test.{label}.{}",
            std::process::id()
        ),
    );
    service.start().expect("the service starts");
    let store = SurfaceStore::new_sharing_the_mach_services_tables(
        service.service_name().to_string(),
        "R-store-test".to_string(),
        std::sync::Arc::clone(state.check_out_leases()),
        std::sync::Arc::clone(state.cross_process_timeline_pairs()),
    );
    store.connect().expect("the store connects");
    gpu.install_the_engines_surface_store(store)
        .expect("the engine's surface store installs once");
    (state, service)
}

fn a_timeline_pair(
    gpu: &GpuContext,
    exportable: bool,
) -> std::sync::Arc<crate::apple::surface_share::CrossProcessTimelinePair> {
    use crate::vulkan::rhi::HostVulkanTimelineSemaphore;
    let device = gpu.device().inner.device();
    let timeline = || {
        std::sync::Arc::new(if exportable {
            HostVulkanTimelineSemaphore::new_exportable(device, 0).expect("a timeline")
        } else {
            HostVulkanTimelineSemaphore::new(device, 0).expect("a timeline")
        })
    };
    std::sync::Arc::new(crate::apple::surface_share::CrossProcessTimelinePair::new(
        timeline(),
        timeline(),
    ))
}

/// A pool slot registered with its timeline pair checks out with both
/// shared-event ports after its IOSurface's, and the service can reach
/// the engine's pair for a helper's host-side reports.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_slot_registered_with_its_timeline_pair_checks_out_with_both_shared_event_ports() {
    let Some(gpu) = gpu_or_skip() else {
        return;
    };
    let (state, service) = a_store_connected_to_a_started_service(&gpu, "timeline-pair");
    let store = gpu.surface_store().expect("the store");
    let (_, pixel_buffer) = gpu
        .acquire_pixel_buffer(16, 8, PixelFormat::Bgra32)
        .expect("a pooled frame");
    let pair = a_timeline_pair(&gpu, true);

    store
        .host_register_pixel_buffer_with_timeline_pair("slot-with-pair", &pixel_buffer, &pair)
        .expect("the registration crosses");

    let reader =
        SurfaceShareMachServiceConnection::connect(service.service_name(), Duration::from_secs(10))
            .expect("a reader connects");
    let (answer, ports) = reader
        .send_request_with_ports(
            &serde_json::json!({"op": "check_out", "surface_id": "slot-with-pair"}),
            Vec::new(),
        )
        .expect("check_out round-trip");
    assert_eq!(answer["has_produce_done_port"], true, "{answer}");
    assert_eq!(ports.len(), 3);
    assert!(!pair.orders_host_side());
    assert!(
        state
            .cross_process_timeline_pairs()
            .pair_of("slot-with-pair")
            .is_some()
    );
}

/// A second registration under a live id is refused, and the first
/// registration keeps its pair.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_refused_duplicate_registration_leaves_the_live_registrations_pair() {
    let Some(gpu) = gpu_or_skip() else {
        return;
    };
    let (state, _service) = a_store_connected_to_a_started_service(&gpu, "timeline-dup");
    let store = gpu.surface_store().expect("the store");
    let (_, pixel_buffer) = gpu
        .acquire_pixel_buffer(16, 8, PixelFormat::Bgra32)
        .expect("a pooled frame");
    let live_pair = a_timeline_pair(&gpu, true);
    store
        .host_register_pixel_buffer_with_timeline_pair("slot-dup", &pixel_buffer, &live_pair)
        .expect("the first registration crosses");

    store
        .host_register_pixel_buffer_with_timeline_pair(
            "slot-dup",
            &pixel_buffer,
            &a_timeline_pair(&gpu, true),
        )
        .expect_err("a second registration under a live id is refused");

    let kept = state
        .cross_process_timeline_pairs()
        .pair_of("slot-dup")
        .expect("the live registration keeps a pair");
    assert!(std::sync::Arc::ptr_eq(&kept, &live_pair));
}

/// A pair whose timelines will not export crosses without ports and
/// orders host-side, without a rebuild or a refused registration.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_slot_whose_timelines_will_not_export_registers_ordering_host_side() {
    let Some(gpu) = gpu_or_skip() else {
        return;
    };
    let (_state, service) = a_store_connected_to_a_started_service(&gpu, "timeline-fallback");
    let store = gpu.surface_store().expect("the store");
    let (_, pixel_buffer) = gpu
        .acquire_pixel_buffer(16, 8, PixelFormat::Bgra32)
        .expect("a pooled frame");
    let pair = a_timeline_pair(&gpu, false);

    store
        .host_register_pixel_buffer_with_timeline_pair("slot-host-side", &pixel_buffer, &pair)
        .expect("the registration still crosses");

    let reader =
        SurfaceShareMachServiceConnection::connect(service.service_name(), Duration::from_secs(10))
            .expect("a reader connects");
    let (answer, ports) = reader
        .send_request_with_ports(
            &serde_json::json!({"op": "lookup", "surface_id": "slot-host-side"}),
            Vec::new(),
        )
        .expect("lookup round-trip");
    assert_eq!(answer["has_produce_done_port"], false, "{answer}");
    assert_eq!(ports.len(), 1);
    assert!(pair.orders_host_side());
}

/// Register `iosurface` under `surface_id` from a connection of its own,
/// stating `width` pixels a row.
fn register_from_another_connection(
    service: &MachSurfaceShareService,
    surface_id: &str,
    iosurface: &IOSurfaceRef,
    width: u32,
) -> SurfaceShareMachServiceConnection {
    let registering_connection =
        SurfaceShareMachServiceConnection::connect(service.service_name(), Duration::from_secs(10))
            .expect("a registering connection");
    let (registered, _) = registering_connection
        .send_request_with_ports(
            &serde_json::json!({
                "op": "register",
                "surface_id": surface_id,
                "runtime_id": "R-elsewhere",
                "width": width,
                "height": iosurface.height(),
                "format": "bgra32",
            }),
            vec![
                crate::apple::iosurface::create_iosurface_mach_send_right(iosurface)
                    .expect("a port to the surface"),
            ],
        )
        .expect("register round-trip");
    assert_eq!(registered, serde_json::json!({"success": true}));
    registering_connection
}

/// The store resolves a surface another connection registered into a
/// pixel buffer over its IOSurface, at the extent and format the service
/// states, holding the registering side's pixels byte for byte.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn the_store_resolves_a_surface_registered_elsewhere_into_its_pixels() {
    let Some(gpu) = gpu_or_skip() else {
        return;
    };
    let (_state, service) = a_store_connected_to_a_started_service(&gpu, "store-lookup");
    let iosurface = crate::apple::iosurface::create_private_iosurface_with_packed_rows(
        48,
        16,
        4,
        PixelFormat::Bgra32,
    )
    .expect("a private IOSurface");
    let surface_bytes = unsafe {
        std::slice::from_raw_parts_mut(iosurface.base_address().as_ptr().cast::<u8>(), 48 * 16 * 4)
    };
    for (index, byte) in surface_bytes.iter_mut().enumerate() {
        *byte = engine_pattern_byte(index);
    }
    let _registering_connection =
        register_from_another_connection(&service, "slot-elsewhere", &iosurface, 48);

    let store = gpu.surface_store().expect("the store");
    for resolved in [
        store
            .lookup_buffer("slot-elsewhere")
            .expect("lookup_buffer resolves it"),
        store
            .check_out("slot-elsewhere")
            .expect("check_out resolves it"),
    ] {
        assert_eq!((resolved.width, resolved.height), (48, 16));
        assert_eq!(resolved.format(), PixelFormat::Bgra32);
        let resolved_bytes = unsafe {
            std::slice::from_raw_parts(resolved.buffer_ref().inner.mapped_ptr(), 48 * 16 * 4)
        };
        assert!(
            resolved_bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| *byte == engine_pattern_byte(index)),
            "the resolved buffer holds the registered surface's pixels"
        );
    }
}

/// A surface whose rows do not pack at the stated width is refused by
/// name rather than read at the wrong stride.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_surface_whose_rows_do_not_pack_at_the_stated_width_is_refused() {
    let Some(gpu) = gpu_or_skip() else {
        return;
    };
    let (_state, service) = a_store_connected_to_a_started_service(&gpu, "store-stride");
    let seventeen_pixel_rows = crate::apple::iosurface::create_private_iosurface_with_packed_rows(
        17,
        8,
        4,
        PixelFormat::Bgra32,
    )
    .expect("a private IOSurface");
    let _registering_connection =
        register_from_another_connection(&service, "slot-padded", &seventeen_pixel_rows, 16);

    let refusal = gpu
        .surface_store()
        .expect("the store")
        .lookup_buffer("slot-padded")
        .expect_err("rows of 68 bytes are not 16 packed BGRA pixels");
    assert!(
        refusal.to_string().contains("rows are 68 bytes"),
        "the refusal names the stride: {refusal}"
    );
}

/// The pool never rehands a slot whose IOSurface the kernel reports in
/// use — here by a use count this process holds, which counts the same
/// as one a helper holds — and takes it back once the use ends.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_slot_whose_iosurface_is_in_use_is_not_rehanded_to_its_producer() {
    let Some(gpu) = gpu_or_skip() else {
        return;
    };
    // Every acquisition below is dropped at once, so the ring never grows
    // past its pre-allocated slots and this many visits each several times.
    let acquisitions_that_revisit_every_slot = 16;
    let slot_key_of = |frame_id: &crate::core::rhi::PublishedPixelBufferFrameId| {
        pool_slot_key_of_surface_id(frame_id.to_string().as_str()).to_string()
    };

    let (held_frame_id, held_buffer) = gpu
        .acquire_pixel_buffer(32, 32, PixelFormat::Bgra32)
        .expect("a pooled frame");
    let held_slot_key = slot_key_of(&held_frame_id);
    let held_iosurface = held_buffer
        .buffer_ref()
        .inner
        .backing_iosurface()
        .map(objc2_core_foundation::CFRetained::<IOSurfaceRef>::from)
        .expect("a macOS pool slot is an IOSurface");
    held_iosurface.increment_use_count();
    drop(held_buffer);

    for _ in 0..acquisitions_that_revisit_every_slot {
        let (frame_id, _) = gpu
            .acquire_pixel_buffer(32, 32, PixelFormat::Bgra32)
            .expect("a pooled frame");
        assert_ne!(
            slot_key_of(&frame_id),
            held_slot_key,
            "a slot in use was rehanded"
        );
    }

    held_iosurface.decrement_use_count();
    let the_slot_came_back = (0..acquisitions_that_revisit_every_slot).any(|_| {
        let (frame_id, _) = gpu
            .acquire_pixel_buffer(32, 32, PixelFormat::Bgra32)
            .expect("a pooled frame");
        slot_key_of(&frame_id) == held_slot_key
    });
    assert!(the_slot_came_back, "the slot returns once nothing uses it");
}
