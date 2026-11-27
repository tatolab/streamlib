// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The verbs a module calls through a table it loaded: ports, messages,
//! surfaces, host memory and leases, and the node host's clock, stop flag,
//! wait, log, capabilities and the node's own failure.

use super::*;

#[test]
fn the_surface_verbs_carry_their_requests_and_a_surface_is_released_once_when_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let imported = module_host
        .import_dma_buf(&ModuleHostForeignDmaBuf {
            fd: 9,
            width: 64,
            height: 16,
            pixel_format: "nv12_full_range",
            allocation_byte_size: 3_456,
            plane_offsets_bytes: &[0, 2_304],
            plane_strides_bytes: &[128, 128],
            drm_format_modifier: 0,
        })
        .expect("the import is taken");
    let output_texture = module_host
        .acquire_texture_from_node_output_pool(&ModuleHostNodeOutputTextureRequest {
            pool_key: "video",
            rotation_depth: 3,
            width: 64,
            height: 16,
            texture_format: "rgba8_unorm",
            usage: &["storage_binding", "texture_binding"],
        })
        .expect("the acquire is taken");
    assert_eq!(imported.surface_id(), "imported-3");
    assert_eq!(output_texture.surface_id(), "pool-slot#4");

    module_host
        .convert_surface_into(
            &imported,
            &output_texture,
            &ModuleHostColorInfo {
                matrix: Some("bt709"),
                ..ModuleHostColorInfo::default()
            },
        )
        .expect("the conversion is taken");
    let refusal = module_host
        .convert_surface_into(
            &imported,
            &output_texture,
            &ModuleHostColorInfo {
                range: Some("tv"),
                ..ModuleHostColorInfo::default()
            },
        )
        .unwrap_err();
    assert_eq!(
        refusal.to_string(),
        "surface_convert_into: \"tv\" is not a range name"
    );
    drop((output_texture, imported));

    assert_eq!(
        *node_host.surface_calls.lock().unwrap(),
        vec![
            "import fd=9 64x16 nv12_full_range planes=2 offsets=[0, 2304] strides=[128, 128] \
             size=3456"
                .to_string(),
            "acquire video depth=3 64x16 rgba8_unorm storage_binding,texture_binding".to_string(),
            "convert 3 -> 4 matrix=\"bt709\" range=\"\"".to_string(),
            "release 4".to_string(),
            "release 3".to_string(),
        ]
    );
    assert_eq!(node_host.references.load(Ordering::SeqCst), 1);
}

#[test]
fn an_import_of_more_planes_than_the_table_carries_is_refused_before_the_host() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host
        .import_dma_buf(&ModuleHostForeignDmaBuf {
            fd: 9,
            width: 64,
            height: 16,
            pixel_format: "nv12_full_range",
            allocation_byte_size: 3_456,
            plane_offsets_bytes: &[0, 1, 2, 3, 4],
            plane_strides_bytes: &[128; 5],
            drm_format_modifier: 0,
        })
        .unwrap_err();
    assert!(refusal.to_string().contains("1 to 4 planes"), "{refusal}");
    assert!(node_host.surface_calls.lock().unwrap().is_empty());
}

#[test]
fn a_host_memory_write_carries_its_geometry_and_refuses_a_buffer_short_of_it() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let output_texture = module_host
        .acquire_texture_from_node_output_pool(&ModuleHostNodeOutputTextureRequest {
            pool_key: "video",
            rotation_depth: 3,
            width: 4,
            height: 2,
            texture_format: "rgba8_unorm",
            usage: &["texture_binding"],
        })
        .expect("the acquire is taken");
    let painted = [7u8; 40];

    module_host
        .write_surface_from_host_memory(
            &output_texture,
            &ModuleHostHostMemoryPixels {
                pixels: &painted,
                pixel_format: "bgra32",
                width: 4,
                height: 2,
                row_stride_bytes: 24,
            },
        )
        .expect("the write is taken");
    let refusal = module_host
        .write_surface_from_host_memory(
            &output_texture,
            &ModuleHostHostMemoryPixels {
                pixels: &painted[..39],
                pixel_format: "bgra32",
                width: 4,
                height: 2,
                row_stride_bytes: 24,
            },
        )
        .unwrap_err();
    assert!(refusal.to_string().contains("40 bytes"), "{refusal}");
    drop(output_texture);

    assert_eq!(
        *node_host.surface_calls.lock().unwrap(),
        vec![
            "acquire video depth=3 4x2 rgba8_unorm texture_binding".to_string(),
            "write 4 4x2 bgra32 stride=24 bytes=40 first=7".to_string(),
            "release 4".to_string(),
        ]
    );
}

#[test]
fn a_host_memory_read_fills_the_modules_memory_and_refuses_memory_short_of_it() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let upstream_frame = module_host
        .lend_surface_a_message_names("camera#3")
        .expect("the lend is taken");
    let mut read_back = [0u8; 40];

    module_host
        .read_surface_into_host_memory(
            &upstream_frame,
            &mut ModuleHostHostMemoryPixelsToFill {
                pixels: &mut read_back,
                pixel_format: "rgba32",
                width: 4,
                height: 2,
                row_stride_bytes: 24,
            },
        )
        .expect("the read is taken");
    assert_eq!(read_back, [6u8; 40]);
    let refusal = module_host
        .read_surface_into_host_memory(
            &upstream_frame,
            &mut ModuleHostHostMemoryPixelsToFill {
                pixels: &mut read_back[..39],
                pixel_format: "rgba32",
                width: 4,
                height: 2,
                row_stride_bytes: 24,
            },
        )
        .unwrap_err();
    assert!(refusal.to_string().contains("40 bytes"), "{refusal}");
    drop(upstream_frame);

    assert_eq!(
        *node_host.surface_calls.lock().unwrap(),
        vec![
            "lend camera#3".to_string(),
            "read 6 4x2 rgba32 stride=24 bytes=40".to_string(),
            "release 6".to_string(),
        ]
    );
}

#[test]
fn a_refused_verb_carries_the_hosts_message_and_the_verb_it_refused() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host.open_output_port("never lent").unwrap_err();
    assert_eq!(
        refusal.to_string(),
        "output_port_open: port \"never lent\" was not lent"
    );

    let stream_facts_refusal = module_host
        .open_output_port("lent")
        .unwrap()
        .set_stream_facts("image", "{}")
        .unwrap_err();
    assert_eq!(
        stream_facts_refusal.to_string(),
        "output_port_set_stream_facts: the facts are another family's"
    );
}

#[test]
fn a_message_reads_its_descriptor_and_lends_its_pooled_frame_and_a_described_write_carries_one() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let input_port = module_host.open_input_port("lent").unwrap();
    let message = input_port.read().unwrap().expect("one message waits");

    let payload_descriptor = message.payload_descriptor().unwrap();
    assert_eq!(payload_descriptor.kind, 4);
    assert_eq!(&payload_descriptor.body[..4], b"h264");
    let lent = message.lend_pooled_gpu_frame().unwrap();
    assert_eq!(lent.surface_id(), "upstream-pool-slot#8");
    assert!(
        node_host
            .surface_calls
            .lock()
            .unwrap()
            .contains(&"lend pooled frame".to_string())
    );

    let output_port = module_host.open_output_port("lent").unwrap();
    output_port
        .write_described(b"unit", Some(9), true, &payload_descriptor)
        .unwrap();
    assert_eq!(
        *node_host.described_writes.lock().unwrap(),
        vec![(
            WriteTheHostRecorded {
                port: 7,
                payload: b"unit".to_vec(),
                timestamp_ns: 9,
                starts_group: true,
            },
            payload_descriptor,
        )]
    );
}

#[test]
fn a_given_up_groups_declaration_reaches_the_host_and_a_refusal_carries_its_message() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let output_port = module_host.open_output_port("lent").unwrap();

    output_port
        .declare_groups_given_up_before_the_next_write(3)
        .unwrap();
    let refusal = output_port
        .declare_groups_given_up_before_the_next_write(0)
        .unwrap_err();

    assert_eq!(
        *node_host.given_up_group_declarations.lock().unwrap(),
        vec![(7, 3)]
    );
    assert_eq!(
        refusal.to_string(),
        "output_port_declare_groups_given_up_before_the_next_write: no group was given up"
    );
}

#[test]
fn a_pool_textures_slot_is_read_and_any_other_surfaces_is_refused() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let output_texture = module_host
        .acquire_texture_from_node_output_pool(&ModuleHostNodeOutputTextureRequest {
            pool_key: "decoded",
            rotation_depth: 3,
            width: 64,
            height: 16,
            texture_format: "rgba8_unorm",
            usage: &["texture_binding"],
        })
        .unwrap();
    let slot = output_texture.gpu_pool_slot_frame().unwrap();
    assert_eq!(slot.kind, 1);
    assert_eq!(&slot.body[16..20], &4u32.to_le_bytes());

    let lent = module_host
        .lend_surface_a_message_names("upstream")
        .unwrap();
    let refusal = lent.gpu_pool_slot_frame().unwrap_err();
    assert!(
        refusal
            .to_string()
            .contains("only a node output pool texture"),
        "{refusal}"
    );
}

#[test]
fn a_read_message_is_handed_back_to_the_host_when_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let input_port = module_host.open_input_port("lent").unwrap();

    let message = input_port.read().unwrap().expect("one message waits");
    assert_eq!(message.payload(), &[1, 2, 3]);
    assert_eq!(message.timestamp_ns(), 42);
    assert!(message.starts_group());
    assert_eq!(message.lost_since_last_read(), 3);
    assert_eq!(node_host.released_reads.load(Ordering::SeqCst), 0);
    drop(message);
    assert_eq!(node_host.released_reads.load(Ordering::SeqCst), 1);

    assert!(input_port.read().unwrap().is_none());
}

#[test]
fn the_clock_stop_flag_wait_log_and_capabilities_reach_the_host() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    assert_eq!(module_host.monotonic_now_ns(), 1_000);
    assert_eq!(module_host.stream_time_ns_of_a_machine_instant(1_000), 500);
    assert!(!module_host.stop_requested());
    node_host.stop_requested.store(1, Ordering::SeqCst);
    assert!(module_host.stop_requested());
    assert!(
        !module_host
            .wait_for_any_input_port(std::time::Duration::from_millis(1))
            .unwrap()
    );
    assert_eq!(
        module_host.host_capabilities_json(),
        "{\"platform\":\"stood-up\"}"
    );

    module_host.log(ModuleHostLogLevel::Warn, "my_module", "a record");
    assert_eq!(
        *node_host.log_records.lock().unwrap(),
        vec![(2, "my_module".to_string(), "a record".to_string())]
    );
}

#[cfg(feature = "tracing")]
#[test]
fn the_modules_own_tracing_records_reach_the_hosts_log_with_their_fields() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    tatolab_module_sdk::module_host_tracing::forward_this_modules_tracing_records_to(&module_host);
    tracing::info!(target: "tatolab_webrtc", attempt = 2, "reconnecting");
    tatolab_module_sdk::module_host_tracing::stop_forwarding_this_modules_tracing_records();
    tracing::info!(target: "tatolab_webrtc", "not forwarded");

    assert_eq!(
        *node_host.log_records.lock().unwrap(),
        vec![(
            3,
            "tatolab_webrtc".to_string(),
            "reconnecting attempt=2".to_string()
        )]
    );
}

/// A runtime lending 1.0 has no audio or surface verbs, so a module built
/// against 1.13 never reads past the end of its table.

#[test]
fn an_iosurface_import_carries_its_surface_and_is_released_once_when_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();
    let stand_in_iosurface = std::ptr::without_provenance::<c_void>(0x1000);

    let imported = module_host
        .import_iosurface(&ModuleHostForeignIOSurface {
            iosurface: stand_in_iosurface,
            pixel_format: "nv12_video_range",
            width: 1280,
            height: 720,
        })
        .expect("the import is taken");
    assert_eq!(imported.surface_id(), "imported-5");
    drop(imported);

    assert_eq!(
        *node_host.surface_calls.lock().unwrap(),
        vec![
            "import iosurface=0x1000 1280x720 nv12_video_range".to_string(),
            "release 5".to_string(),
        ]
    );
}

#[test]
fn an_iosurface_import_of_a_null_surface_is_refused_before_the_host() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host
        .import_iosurface(&ModuleHostForeignIOSurface {
            iosurface: std::ptr::null(),
            pixel_format: "nv12_video_range",
            width: 1280,
            height: 720,
        })
        .unwrap_err();
    assert!(
        refusal.to_string().contains("null IOSurfaceRef"),
        "{refusal}"
    );
    assert!(node_host.surface_calls.lock().unwrap().is_empty());
}

#[test]
fn a_surface_a_message_names_is_lent_by_its_id_and_released_once_when_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let lent = module_host
        .lend_surface_a_message_names("upstream-pool-slot#6")
        .expect("the lend is taken");
    assert_eq!(lent.surface_id(), "upstream-pool-slot#6");
    drop(lent);
    let refusal = module_host.lend_surface_a_message_names("").unwrap_err();
    assert!(
        refusal.to_string().contains("no surface id was named"),
        "{refusal}"
    );

    assert_eq!(
        *node_host.surface_calls.lock().unwrap(),
        vec![
            "lend upstream-pool-slot#6".to_string(),
            "release 6".to_string(),
        ]
    );
}

#[test]
fn a_lease_names_its_endpoint_and_tenant_reports_its_state_and_is_released_once_when_it_drops() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let lease = module_host
        .lease_shared_native_service("browser-host")
        .expect("the host grants the lease");
    assert_eq!(
        lease.endpoint_socket_path(),
        std::path::Path::new(STOOD_UP_SERVICE_ENDPOINT)
    );
    assert_eq!(lease.tenant_stream_id(), STOOD_UP_SERVICE_TENANT);
    assert_eq!(
        lease.state().unwrap(),
        ModuleHostSharedNativeServiceState::StartingAgain
    );
    assert_eq!(
        lease.state().unwrap(),
        ModuleHostSharedNativeServiceState::Running
    );
    assert_eq!(
        lease.state().unwrap(),
        ModuleHostSharedNativeServiceState::Failed(
            "the browser host crashed 5 times in a row".to_string()
        )
    );
    drop(lease);

    assert_eq!(
        *node_host.lease_calls.lock().unwrap(),
        [
            "acquire browser-host",
            "state 7",
            "state 7",
            "state 7",
            "release 7"
        ]
    );
}

#[test]
fn a_lease_the_host_refuses_carries_its_message_and_releases_nothing() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host
        .lease_shared_native_service("rive-host")
        .unwrap_err();

    assert!(
        refusal
            .to_string()
            .contains("this runtime offers no rive-host"),
        "{refusal}"
    );
    assert_eq!(
        *node_host.lease_calls.lock().unwrap(),
        ["acquire rive-host"]
    );
}

#[test]
fn a_node_fails_itself_with_the_reason_it_gives() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    module_host
        .fail_the_node_unrecoverably("the page's renderer ended 6 times in 60s")
        .expect("the host records the failure");

    assert_eq!(
        *node_host.node_failure_reasons.lock().unwrap(),
        ["the page's renderer ended 6 times in 60s"]
    );
}

#[test]
fn a_call_too_slow_for_a_frame_is_asked_about_by_what_it_does_and_its_name() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    module_host
        .refuse_while_a_frame_runs(ModuleHostCallTooSlowForAFrame::CreatesAKernel, "compile")
        .unwrap();
    module_host
        .refuse_while_a_frame_runs(ModuleHostCallTooSlowForAFrame::StartsASession, "connect")
        .unwrap();
    let refusal = module_host
        .refuse_while_a_frame_runs(
            ModuleHostCallTooSlowForAFrame::OpensADevice,
            "V4l2ZeroCopyCameraCapture",
        )
        .unwrap_err();

    assert_eq!(
        *node_host.calls_asked_about_in_a_frame.lock().unwrap(),
        vec![
            (
                MODULE_HOST_TABLE_CALL_CREATES_A_KERNEL,
                "compile".to_string()
            ),
            (
                MODULE_HOST_TABLE_CALL_STARTS_A_SESSION,
                "connect".to_string()
            ),
            (
                MODULE_HOST_TABLE_CALL_OPENS_A_DEVICE,
                "V4l2ZeroCopyCameraCapture".to_string()
            ),
        ]
    );
    assert_eq!(
        refusal,
        ModuleHostError::CallRefusedInAFrame {
            does: ModuleHostCallTooSlowForAFrame::OpensADevice,
            refusal: "device_opened_in_a_frame: `V4l2ZeroCopyCameraCapture`".to_string()
        }
    );
}

#[test]
fn a_refusal_not_led_by_the_calls_code_stays_the_verbs_own() {
    let node_host = stood_up_node_host();
    let table = v1_13_table_over(node_host);
    let module_host = unsafe { ModuleHost::from_module_host_table_pointer(&table) }.unwrap();

    let refusal = module_host
        .refuse_while_a_frame_runs(ModuleHostCallTooSlowForAFrame::StartsASession, "gone")
        .unwrap_err();

    assert_eq!(
        refusal,
        ModuleHostError::VerbRefused {
            verb: "call_refused_while_a_frame_runs",
            refusal: "the node host is gone".to_string()
        }
    );
}
