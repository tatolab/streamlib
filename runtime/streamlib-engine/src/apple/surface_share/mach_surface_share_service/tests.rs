// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::surface_share_request_dispatch_and_gpu_pool_handlers::answer_surface_share_request;
use super::*;
use crate::apple::iosurface::create_private_iosurface_with_packed_rows;
use crate::core::context::surface_share_wire_verbs::{
    VK_IMAGE_ARRAY_LAYERS_DEFAULT, VK_IMAGE_MIP_LEVELS_DEFAULT, VK_IMAGE_SAMPLES_DEFAULT,
    VK_IMAGE_TILING_DEFAULT, VK_IMAGE_TYPE_DEFAULT,
};
use crate::core::rhi::PixelFormat;
use objc2_core_foundation::CFRetained;
use streamlib_surface_client::SurfaceShareMachServiceConnection;

fn a_small_iosurface() -> CFRetained<IOSurfaceRef> {
    create_private_iosurface_with_packed_rows(16, 8, 4, PixelFormat::Bgra32)
        .expect("a private IOSurface")
}

fn a_port_to(iosurface: &IOSurfaceRef) -> OwnedMachSendRight {
    create_iosurface_mach_send_right(iosurface).expect("a port to the surface")
}

fn this_process() -> SurfaceShareMachSenderAuditIdentity {
    SurfaceShareMachSenderAuditIdentity {
        pid: std::process::id() as libc::pid_t,
        pidversion: 0,
    }
}

fn register_request(surface_id: &str) -> serde_json::Value {
    serde_json::json!({
        "op": "register",
        "surface_id": surface_id,
        "runtime_id": "R-test",
        "width": 16,
        "height": 8,
        "format": "bgra32",
    })
}

fn a_unique_service_name(label: &str) -> String {
    format!(
        "com.tatolab.streamlib.surface-share-test.{label}.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    )
}

#[test]
fn this_process_is_admitted_and_an_unknown_pid_waits() {
    let admissions = SurfaceShareHelperProcessAdmissions::default();
    let this_pid = std::process::id() as libc::pid_t;
    assert_eq!(
        admissions.verdict_for(this_process(), this_pid),
        SurfaceShareConnectionAdmissionVerdict::Admitted
    );
    let stranger = SurfaceShareMachSenderAuditIdentity {
        pid: this_pid + 1,
        pidversion: 7,
    };
    assert_eq!(
        admissions.verdict_for(stranger, this_pid),
        SurfaceShareConnectionAdmissionVerdict::NotYetAdmitted
    );
}

#[test]
fn an_admitted_pid_is_pinned_to_the_pid_version_it_first_connects_with() {
    let admissions = SurfaceShareHelperProcessAdmissions::default();
    let this_pid = 1;
    let _admission = admissions.admit_helper_process(4242);
    let helper = SurfaceShareMachSenderAuditIdentity {
        pid: 4242,
        pidversion: 3,
    };
    assert_eq!(
        admissions.verdict_for(helper, this_pid),
        SurfaceShareConnectionAdmissionVerdict::Admitted
    );
    assert_eq!(
        admissions.verdict_for(helper, this_pid),
        SurfaceShareConnectionAdmissionVerdict::Admitted
    );
    let impostor_on_a_reused_pid = SurfaceShareMachSenderAuditIdentity {
        pid: 4242,
        pidversion: 4,
    };
    let SurfaceShareConnectionAdmissionVerdict::Refused(reason) =
        admissions.verdict_for(impostor_on_a_reused_pid, this_pid)
    else {
        panic!("a different pid version must be refused");
    };
    assert!(reason.contains("reused pid"), "{reason}");
}

#[test]
fn a_withdrawn_admission_stops_admitting_the_pid() {
    let admissions = SurfaceShareHelperProcessAdmissions::default();
    let helper = SurfaceShareMachSenderAuditIdentity {
        pid: 4343,
        pidversion: 1,
    };
    let admission = admissions.admit_helper_process(4343);
    assert_eq!(
        admissions.verdict_for(helper, 1),
        SurfaceShareConnectionAdmissionVerdict::Admitted
    );
    drop(admission);
    assert_eq!(
        admissions.verdict_for(helper, 1),
        SurfaceShareConnectionAdmissionVerdict::NotYetAdmitted
    );
}

fn a_send_right_to_a_fresh_shared_event_at(
    value: u64,
) -> Option<(
    objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLSharedEvent>>,
    OwnedMachSendRight,
)> {
    use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice, MTLSharedEvent};
    let shared_event = MTLCreateSystemDefaultDevice()?.newSharedEvent()?;
    shared_event.setSignaledValue(value);
    let send_right = streamlib_surface_client::mach_send_right_of_metal_shared_event_handle(
        &shared_event.newSharedEventHandle(),
    )
    .expect("the handle's send right");
    Some((shared_event, send_right))
}

fn signaled_value_behind(send_right: &OwnedMachSendRight) -> u64 {
    use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice, MTLSharedEvent};
    let handle = streamlib_surface_client::metal_shared_event_handle_of_mach_send_right(send_right)
        .expect("a handle");
    MTLCreateSystemDefaultDevice()
        .and_then(|device| device.newSharedEventWithHandle(&handle))
        .expect("the port names a live shared event")
        .signaledValue()
}

#[test]
fn a_registration_with_a_timeline_pair_looks_up_with_both_ports_after_the_surface() {
    let (Some((_produce_done, produce_done_port)), Some((_consume_done, consume_done_port))) = (
        a_send_right_to_a_fresh_shared_event_at(11),
        a_send_right_to_a_fresh_shared_event_at(22),
    ) else {
        return;
    };
    let state = IOSurfaceShareState::new();
    let iosurface = a_small_iosurface();
    let holder = state.check_out_leases().mint_holder_id();
    let mut request = register_request("slot-timelines");
    request[SURFACE_SHARE_HAS_PRODUCE_DONE_PORT] = true.into();
    request[SURFACE_SHARE_HAS_CONSUME_DONE_PORT] = true.into();

    let (registered, _) = answer_surface_share_request(
        &state,
        &request,
        vec![a_port_to(&iosurface), produce_done_port, consume_done_port],
        holder,
    );
    assert_eq!(registered, serde_json::json!({"success": true}));

    let (looked_up, ports) = answer_surface_share_request(
        &state,
        &serde_json::json!({"op": "check_out", "surface_id": "slot-timelines"}),
        Vec::new(),
        holder,
    );
    assert_eq!(looked_up[SURFACE_SHARE_HAS_PRODUCE_DONE_PORT], true);
    assert_eq!(looked_up[SURFACE_SHARE_HAS_CONSUME_DONE_PORT], true);
    assert_eq!(ports.len(), 3);
    assert_eq!(signaled_value_behind(&ports[1]), 11);
    assert_eq!(signaled_value_behind(&ports[2]), 22);
}

#[test]
fn a_storage_buffer_registration_checks_out_with_its_shape_and_dtype() {
    let state = IOSurfaceShareState::new();
    let iosurface = create_private_iosurface_with_packed_rows(16384, 1, 1, PixelFormat::Unknown)
        .expect("a byte-shaped IOSurface");
    let holder = state.check_out_leases().mint_holder_id();
    let (registered, _) = answer_surface_share_request(
        &state,
        &serde_json::json!({
            "op": "register",
            "surface_id": "tensor-slot",
            "runtime_id": "R-test",
            "resource_type": "storage_buffer",
            "shape": [3, 7, 11],
            "dtype": "float16",
        }),
        vec![a_port_to(&iosurface)],
        holder,
    );
    assert_eq!(registered, serde_json::json!({"success": true}));

    let (checked_out, ports) = answer_surface_share_request(
        &state,
        &serde_json::json!({"op": "check_out", "surface_id": "tensor-slot"}),
        Vec::new(),
        holder,
    );
    assert_eq!(checked_out["resource_type"], "storage_buffer");
    assert_eq!(checked_out["shape"], serde_json::json!([3, 7, 11]));
    assert_eq!(checked_out["dtype"], "float16");
    assert_eq!(ports.len(), 1);
}

#[test]
fn a_storage_buffer_registration_its_surface_cannot_hold_is_refused() {
    let state = IOSurfaceShareState::new();
    let iosurface = a_small_iosurface();
    let (refused, _) = answer_surface_share_request(
        &state,
        &serde_json::json!({
            "op": "register",
            "surface_id": "tensor-too-big",
            "runtime_id": "R-test",
            "resource_type": "storage_buffer",
            "shape": [1, 3, 640, 640],
            "dtype": "float32",
        }),
        vec![a_port_to(&iosurface)],
        state.check_out_leases().mint_holder_id(),
    );
    assert!(
        refused["error"]
            .as_str()
            .is_some_and(|error| error.contains("cannot hold")),
        "{refused}"
    );
    assert!(state.registration_of("tensor-too-big").is_none());
}

#[test]
fn a_registration_announcing_one_timeline_port_is_refused() {
    let Some((_produce_done, produce_done_port)) = a_send_right_to_a_fresh_shared_event_at(0)
    else {
        return;
    };
    let state = IOSurfaceShareState::new();
    let iosurface = a_small_iosurface();
    let mut request = register_request("slot-half-pair");
    request[SURFACE_SHARE_HAS_PRODUCE_DONE_PORT] = true.into();

    let (refused, _) = answer_surface_share_request(
        &state,
        &request,
        vec![a_port_to(&iosurface), produce_done_port],
        state.check_out_leases().mint_holder_id(),
    );
    assert!(
        refused["error"]
            .as_str()
            .is_some_and(|error| error.contains("both timeline ports or neither")),
        "{refused}"
    );
    assert!(state.registration_of("slot-half-pair").is_none());
}

#[test]
fn a_host_side_report_against_a_surface_with_no_engine_pair_is_refused() {
    let state = IOSurfaceShareState::new();
    for op in [
        SURFACE_SHARE_OP_SIGNAL_CONSUME_DONE,
        SURFACE_SHARE_OP_TIMELINE_IMPORT_REFUSED,
    ] {
        let (refused, _) = answer_surface_share_request(
            &state,
            &serde_json::json!({"op": op, "surface_id": "slot-unpaired", "value": 1}),
            Vec::new(),
            state.check_out_leases().mint_holder_id(),
        );
        assert!(
            refused["error"]
                .as_str()
                .is_some_and(|error| error.contains("no engine timeline pair")),
            "{op}: {refused}"
        );
    }
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_helpers_host_side_reports_move_the_engines_own_pair() {
    use crate::apple::surface_share::CrossProcessTimelinePair;
    use crate::vulkan::rhi::{HostVulkanDevice, HostVulkanTimelineSemaphore};

    let device = HostVulkanDevice::new().expect("the rig must produce a Vulkan device");
    let pair = Arc::new(CrossProcessTimelinePair::new(
        Arc::new(HostVulkanTimelineSemaphore::new_exportable(device.device(), 0).unwrap()),
        Arc::new(HostVulkanTimelineSemaphore::new_exportable(device.device(), 0).unwrap()),
    ));
    let state = IOSurfaceShareState::new();
    state
        .cross_process_timeline_pairs()
        .insert("slot-paired", Arc::clone(&pair));
    let holder = state.check_out_leases().mint_holder_id();

    let (refused_import, _) = answer_surface_share_request(
        &state,
        &serde_json::json!({
            "op": SURFACE_SHARE_OP_TIMELINE_IMPORT_REFUSED,
            "surface_id": "slot-paired#3",
            "reason": "no VK_EXT_metal_objects",
        }),
        Vec::new(),
        holder,
    );
    assert_eq!(refused_import, serde_json::json!({"success": true}));
    assert!(pair.orders_host_side());

    pair.produce_done()
        .signal_host(4)
        .expect("produce four frames");
    let (signalled, _) = answer_surface_share_request(
        &state,
        &serde_json::json!({
            "op": SURFACE_SHARE_OP_SIGNAL_CONSUME_DONE,
            "surface_id": "slot-paired#3",
            "value": 4,
        }),
        Vec::new(),
        holder,
    );
    assert_eq!(signalled, serde_json::json!({"success": true}));
    assert_eq!(pair.consume_done().current_value().unwrap(), 4);
}

#[test]
fn a_registered_surface_looks_up_as_a_port_to_the_same_surface() {
    let state = IOSurfaceShareState::new();
    let iosurface = a_small_iosurface();
    let holder = state.check_out_leases().mint_holder_id();

    let (registered, _) = answer_surface_share_request(
        &state,
        &register_request("slot-a"),
        vec![a_port_to(&iosurface)],
        holder,
    );
    assert_eq!(registered, serde_json::json!({"success": true}));

    let (looked_up, mut ports) = answer_surface_share_request(
        &state,
        &serde_json::json!({"op": "lookup", "surface_id": "slot-a"}),
        Vec::new(),
        holder,
    );
    assert_eq!(looked_up["width"], 16);
    assert_eq!(looked_up["handle_type"], SURFACE_HANDLE_TYPE_IOSURFACE);
    assert_eq!(looked_up["plane_strides"], serde_json::json!([64]));
    assert_eq!(ports.len(), 1);
    assert!(
        iosurface.is_in_use(),
        "an outstanding port counts as the surface being in use"
    );
    let resolved = IOSurfaceRef::lookup_from_mach_port(ports[0].as_raw_name())
        .expect("the port names the surface");
    assert_eq!(resolved.id(), iosurface.id());
    ports.clear();
    assert!(!iosurface.is_in_use());
}

#[test]
fn registrations_are_refused_without_exactly_one_iosurface_port_or_with_a_generation_suffix() {
    let state = IOSurfaceShareState::new();
    let holder = state.check_out_leases().mint_holder_id();
    let (portless, _) =
        answer_surface_share_request(&state, &register_request("slot-b"), Vec::new(), holder);
    assert!(portless["error"].as_str().unwrap().contains("exactly one"));

    let iosurface = a_small_iosurface();
    let (suffixed, _) = answer_surface_share_request(
        &state,
        &register_request("slot-b#1"),
        vec![a_port_to(&iosurface)],
        holder,
    );
    assert!(suffixed["error"].as_str().unwrap().contains("reserved"));
    assert!(
        !iosurface.is_in_use(),
        "a refused registration released the port it carried"
    );

    let (first, _) = answer_surface_share_request(
        &state,
        &register_request("slot-b"),
        vec![a_port_to(&iosurface)],
        holder,
    );
    let (duplicate, _) = answer_surface_share_request(
        &state,
        &register_request("slot-b"),
        vec![a_port_to(&iosurface)],
        holder,
    );
    assert_eq!(first, serde_json::json!({"success": true}));
    assert_eq!(duplicate, serde_json::json!({"success": false}));
}

#[test]
fn a_check_out_leases_the_slot_until_its_holder_releases_it() {
    let state = IOSurfaceShareState::new();
    let iosurface = a_small_iosurface();
    let holder = state.check_out_leases().mint_holder_id();
    answer_surface_share_request(
        &state,
        &register_request("slot-c"),
        vec![a_port_to(&iosurface)],
        holder,
    );
    state
        .check_out_leases()
        .publish_frame_generation("slot-c", 1)
        .unwrap();

    let (checked_out, _) = answer_surface_share_request(
        &state,
        &serde_json::json!({"op": "check_out", "surface_id": "slot-c#1"}),
        Vec::new(),
        holder,
    );
    assert!(checked_out.get("error").is_none(), "{checked_out}");
    assert_eq!(
        state
            .check_out_leases()
            .outstanding_check_out_count("slot-c")
            .unwrap(),
        1
    );

    let (released, _) = answer_surface_share_request(
        &state,
        &serde_json::json!({"op": "release_check_out", "surface_id": "slot-c#1"}),
        Vec::new(),
        holder,
    );
    assert_eq!(
        released,
        serde_json::json!({"success": true, "released": true})
    );
    assert_eq!(
        state
            .check_out_leases()
            .outstanding_check_out_count("slot-c")
            .unwrap(),
        0
    );
}

#[test]
fn a_retired_frame_id_is_refused_before_any_port_crosses() {
    let state = IOSurfaceShareState::new();
    let iosurface = a_small_iosurface();
    let holder = state.check_out_leases().mint_holder_id();
    answer_surface_share_request(
        &state,
        &register_request("slot-d"),
        vec![a_port_to(&iosurface)],
        holder,
    );
    state
        .check_out_leases()
        .publish_frame_generation("slot-d", 2)
        .unwrap();
    let (refused, ports) = answer_surface_share_request(
        &state,
        &serde_json::json!({"op": "check_out", "surface_id": "slot-d#1"}),
        Vec::new(),
        holder,
    );
    assert!(refused.get("error").is_some(), "{refused}");
    assert!(ports.is_empty());
    assert!(!iosurface.is_in_use());
}

#[test]
fn a_registration_is_released_only_by_the_runtime_that_made_it() {
    let state = IOSurfaceShareState::new();
    let iosurface = a_small_iosurface();
    let holder = state.check_out_leases().mint_holder_id();
    answer_surface_share_request(
        &state,
        &register_request("slot-e"),
        vec![a_port_to(&iosurface)],
        holder,
    );
    let (by_a_stranger, _) = answer_surface_share_request(
        &state,
        &serde_json::json!({"op": "release", "surface_id": "slot-e", "runtime_id": "R-other"}),
        Vec::new(),
        holder,
    );
    let (by_its_runtime, _) = answer_surface_share_request(
        &state,
        &serde_json::json!({"op": "release", "surface_id": "slot-e", "runtime_id": "R-test"}),
        Vec::new(),
        holder,
    );
    assert_eq!(by_a_stranger, serde_json::json!({"success": false}));
    assert_eq!(by_its_runtime, serde_json::json!({"success": true}));
    assert!(state.surface_ids().is_empty());
}

fn started_service(label: &str) -> (IOSurfaceShareState, MachSurfaceShareService) {
    let state = IOSurfaceShareState::new();
    let mut service = MachSurfaceShareService::new(state.clone(), a_unique_service_name(label));
    service.start().expect("the service starts");
    (state, service)
}

fn connect_to(service: &MachSurfaceShareService) -> SurfaceShareMachServiceConnection {
    SurfaceShareMachServiceConnection::connect(service.service_name(), Duration::from_secs(5))
        .expect("this process is admitted")
}

fn settles_within(budget: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::yield_now();
    }
    condition()
}

#[test]
fn this_process_registers_and_looks_up_a_surface_over_the_mach_channel() {
    let (_state, service) = started_service("round-trip");
    let connection = connect_to(&service);
    let iosurface = a_small_iosurface();

    let (registered, _) = connection
        .send_request_with_ports(&register_request("slot-f"), vec![a_port_to(&iosurface)])
        .unwrap();
    assert_eq!(registered, serde_json::json!({"success": true}));

    let (looked_up, ports) = connection
        .send_request_with_ports(
            &serde_json::json!({"op": "lookup", "surface_id": "slot-f"}),
            Vec::new(),
        )
        .unwrap();
    assert_eq!(looked_up["height"], 8);
    let resolved = IOSurfaceRef::lookup_from_mach_port(ports[0].as_raw_name())
        .expect("the answered port names the surface");
    assert_eq!(resolved.id(), iosurface.id());
}

/// The whole texture registration crosses the channel and comes back on
/// a check-out: the recipe, the layout, and the pair's ports after the
/// surface's. A later `update_layout` is what the next holder sees.
#[test]
fn a_texture_registration_round_trips_its_recipe_layout_and_timeline_ports() {
    let (Some((_produce_done, produce_done_port)), Some((_consume_done, consume_done_port))) = (
        a_send_right_to_a_fresh_shared_event_at(5),
        a_send_right_to_a_fresh_shared_event_at(6),
    ) else {
        return;
    };
    const VK_IMAGE_LAYOUT_GENERAL: i32 = 1;
    const VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL: i32 = 5;
    let (_state, service) = started_service("texture-round-trip");
    let connection = connect_to(&service);
    let iosurface = a_small_iosurface();
    let mut request = register_request("texture-slot");
    request["resource_type"] = "texture".into();
    request["current_image_layout"] = VK_IMAGE_LAYOUT_GENERAL.into();
    request["vk_image_usage"] = 0x1B.into();
    request["vk_image_allocation_size"] = 4096.into();
    request[SURFACE_SHARE_HAS_PRODUCE_DONE_PORT] = true.into();
    request[SURFACE_SHARE_HAS_CONSUME_DONE_PORT] = true.into();

    let (registered, _) = connection
        .send_request_with_ports(
            &request,
            vec![a_port_to(&iosurface), produce_done_port, consume_done_port],
        )
        .unwrap();
    assert_eq!(registered, serde_json::json!({"success": true}));

    let check_out = serde_json::json!({"op": "check_out", "surface_id": "texture-slot"});
    let (checked_out, ports) = connection
        .send_request_with_ports(&check_out, Vec::new())
        .unwrap();
    assert_eq!(checked_out["resource_type"], "texture");
    assert_eq!(checked_out["handle_type"], SURFACE_HANDLE_TYPE_IOSURFACE);
    assert_eq!(checked_out["current_image_layout"], VK_IMAGE_LAYOUT_GENERAL);
    assert_eq!(checked_out["vk_image_usage"], 0x1B);
    assert_eq!(checked_out["vk_image_allocation_size"], 4096);
    assert_eq!(checked_out["vk_image_type"], VK_IMAGE_TYPE_DEFAULT);
    assert_eq!(
        checked_out["vk_image_mip_levels"],
        VK_IMAGE_MIP_LEVELS_DEFAULT
    );
    assert_eq!(
        checked_out["vk_image_array_layers"],
        VK_IMAGE_ARRAY_LAYERS_DEFAULT
    );
    assert_eq!(checked_out["vk_image_samples"], VK_IMAGE_SAMPLES_DEFAULT);
    assert_eq!(checked_out["vk_image_tiling"], VK_IMAGE_TILING_DEFAULT);
    assert_eq!(ports.len(), 3);
    let resolved = IOSurfaceRef::lookup_from_mach_port(ports[0].as_raw_name())
        .expect("the first port names the surface");
    assert_eq!(resolved.id(), iosurface.id());
    assert_eq!(signaled_value_behind(&ports[1]), 5);
    assert_eq!(signaled_value_behind(&ports[2]), 6);

    let (updated, _) = connection
        .send_request_with_ports(
            &serde_json::json!({
                "op": "update_layout",
                "surface_id": "texture-slot",
                "current_image_layout": VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            }),
            Vec::new(),
        )
        .unwrap();
    assert_eq!(updated, serde_json::json!({"success": true}));
    let (looked_up, _) = connection
        .send_request_with_ports(
            &serde_json::json!({"op": "lookup", "surface_id": "texture-slot"}),
            Vec::new(),
        )
        .unwrap();
    assert_eq!(
        looked_up["current_image_layout"],
        VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL
    );
}

/// A pixel buffer has no image, so no recipe or layout to publish.
#[test]
fn a_layout_update_for_a_pixel_buffer_is_answered_unsuccessful() {
    let state = IOSurfaceShareState::new();
    let iosurface = a_small_iosurface();
    let holder = state.check_out_leases().mint_holder_id();
    answer_surface_share_request(
        &state,
        &register_request("pixel-slot"),
        vec![a_port_to(&iosurface)],
        holder,
    );
    let (updated, _) = answer_surface_share_request(
        &state,
        &serde_json::json!({
            "op": "update_layout", "surface_id": "pixel-slot", "current_image_layout": 1,
        }),
        Vec::new(),
        holder,
    );
    assert_eq!(updated, serde_json::json!({"success": false}));
    let (looked_up, _) = answer_surface_share_request(
        &state,
        &serde_json::json!({"op": "lookup", "surface_id": "pixel-slot"}),
        Vec::new(),
        holder,
    );
    assert!(looked_up.get("current_image_layout").is_none());
    assert!(looked_up.get("vk_image_usage").is_none());
}

#[test]
fn a_closed_connection_releases_its_leases_but_not_this_processs_registrations() {
    let (state, service) = started_service("close");
    let iosurface = a_small_iosurface();
    let registering_connection = connect_to(&service);
    registering_connection
        .send_request_with_ports(&register_request("slot-g"), vec![a_port_to(&iosurface)])
        .unwrap();
    state
        .check_out_leases()
        .publish_frame_generation("slot-g", 1)
        .unwrap();

    let reading_connection = connect_to(&service);
    let (checked_out, _) = reading_connection
        .send_request_with_ports(
            &serde_json::json!({"op": "check_out", "surface_id": "slot-g#1"}),
            Vec::new(),
        )
        .unwrap();
    assert!(checked_out.get("error").is_none(), "{checked_out}");
    assert_eq!(
        state
            .check_out_leases()
            .outstanding_check_out_count("slot-g")
            .unwrap(),
        1
    );

    drop(reading_connection);
    drop(registering_connection);

    assert!(
        settles_within(Duration::from_secs(5), || {
            state
                .check_out_leases()
                .outstanding_check_out_count("slot-g")
                .unwrap()
                == 0
        }),
        "the closed connection's lease was released"
    );
    assert_eq!(state.surface_ids(), vec!["slot-g".to_string()]);
}

#[test]
fn a_second_service_under_a_live_name_is_refused() {
    let (_state, service) = started_service("duplicate");
    let mut second =
        MachSurfaceShareService::new(IOSurfaceShareState::new(), service.service_name().into());
    let refused = second.start().unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::AddrInUse);
}

#[test]
fn a_stopped_service_is_seen_going_away_by_its_clients_and_its_name_frees() {
    let (_state, mut service) = started_service("stop");
    let service_name = service.service_name().to_string();
    let connection = connect_to(&service);

    service.stop();

    assert!(
        connection
            .wait_for_the_service_to_go_away(Some(Duration::from_secs(5)))
            .unwrap()
    );
    let refused = connection
        .send_request_with_ports(&serde_json::json!({"op": "lookup"}), Vec::new())
        .unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::BrokenPipe);
    let mut restarted = MachSurfaceShareService::new(IOSurfaceShareState::new(), service_name);
    restarted
        .start()
        .expect("the name is free once the service stops");
}
