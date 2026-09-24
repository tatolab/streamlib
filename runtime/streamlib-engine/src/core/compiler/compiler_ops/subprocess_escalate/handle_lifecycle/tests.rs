// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::EscalateHandleRegistry;

#[test]
fn release_handle_flags_unknown_handle() {
    // Registry-level release of an unknown handle. A full
    // integration test that exercises [`handle_escalate_op`]
    // against a real `GpuContextLimitedAccess` lives in the
    // escalate root's `handle_escalate_op_end_to_end` test — it is gated
    // on [`GpuContext::init_for_platform`] succeeding so CI
    // machines without a GPU still build+run the rest of the
    // suite.
    let registry = EscalateHandleRegistry::new();
    assert_eq!(registry.handle_count(), 0);
    assert!(registry.remove_handle("missing").is_none());
}

/// On macOS an escalate `acquire_texture` allocates over an IOSurface and
/// registers the texture whole with the surface-share service. While the
/// helper holds it the pool hands the slot to no one else; the teardown a
/// killed helper's bridge runs frees the slot and the registration both.
#[cfg(target_os = "macos")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
fn a_helpers_texture_crosses_on_an_iosurface_and_its_slot_is_held_until_teardown() {
    use std::sync::Arc;

    use uuid::Uuid;

    use super::super::acquisition::parse_texture_usages;
    use super::super::handle_escalate_op;
    use super::{RegisteredHandle, release_surface_share_and_texture_cache_for_handle};
    use crate::apple::surface_share::{IOSurfaceShareState, MachSurfaceShareService};
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestAcquireTexture;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::{
        EscalateRequest, EscalateResponse,
    };
    use crate::core::context::{
        GpuContext, GpuContextLimitedAccess, SurfaceStore, TextureCrossProcessImportability,
        TexturePoolDescriptor,
    };
    use crate::core::rhi::TextureFormat;
    use crate::core::runtime::mesh::a_mesh_link_ingress_table_carrying_nothing;

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let state = IOSurfaceShareState::new();
    let mut service = MachSurfaceShareService::new(
        state.clone(),
        format!(
            "com.tatolab.streamlib.escalate-texture-test.{}.{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ),
    );
    service
        .start()
        .expect("the Mach surface-share service starts");
    let store = SurfaceStore::new_sharing_the_mach_services_tables(
        service.service_name().to_string(),
        "R-escalate-texture".to_string(),
        Arc::clone(state.check_out_leases()),
        Arc::clone(state.cross_process_timeline_pairs()),
    );
    store.connect().expect("the store connects");
    gpu.set_surface_store(store);
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();
    let (width, height, format) = (64, 32, TextureFormat::Rgba8Unorm);
    let usage = vec!["texture_binding".to_string(), "storage_binding".to_string()];

    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
            request_id: "req-iosurface".to_string(),
            width,
            height,
            format: format.wire_name().to_string(),
            usage: usage.clone(),
        }),
    );
    let handle_id = match response {
        Some(EscalateResponse::Ok(ok)) => ok.handle_id,
        other => panic!("acquire_texture failed: {other:?}"),
    };
    let registration = state
        .registration_of(&handle_id)
        .expect("the texture is registered with the surface-share service");
    assert_eq!(registration.resource_type, "texture");
    assert!(registration.timeline_send_rights.is_some());
    let texture_image = registration
        .texture_image
        .expect("a texture registration carries its image");
    assert_eq!(texture_image.recipe.vk_image_tiling, 0, "OPTIMAL");
    assert!(
        state
            .cross_process_timeline_pairs()
            .pair_of(&handle_id)
            .is_some()
    );

    let held_slot_id = {
        let handles = registry.handles.lock().expect("poisoned");
        match handles.get(&handle_id) {
            Some(RegisteredHandle::Texture {
                texture,
                timeline_pair,
            }) => {
                assert!(timeline_pair.is_some());
                texture.slot_id()
            }
            _ => panic!("the registry holds the texture"),
        }
    };
    let same_bucket = TexturePoolDescriptor::new(width, height, format)
        .with_usage(parse_texture_usages(&usage).expect("usage"))
        .with_cross_process_importability(TextureCrossProcessImportability::IOSurface);
    let while_held = sandbox
        .acquire_texture(&same_bucket)
        .expect("a second slot");
    assert_ne!(
        while_held.slot_id(),
        held_slot_id,
        "the pool rehanded a slot a helper still holds"
    );
    drop(while_held);

    for (drained_handle_id, removed_handle) in registry.drain_handles() {
        release_surface_share_and_texture_cache_for_handle(
            &sandbox,
            &drained_handle_id,
            &removed_handle,
        );
    }
    assert!(state.registration_of(&handle_id).is_none());
    assert!(
        state
            .cross_process_timeline_pairs()
            .pair_of(&handle_id)
            .is_none()
    );
    let mut reacquired_slot_ids = Vec::new();
    let mut reacquired = Vec::new();
    for _ in 0..2 {
        let texture = sandbox.acquire_texture(&same_bucket).expect("a slot");
        reacquired_slot_ids.push(texture.slot_id());
        reacquired.push(texture);
    }
    assert!(
        reacquired_slot_ids.contains(&held_slot_id),
        "the teardown did not return the helper's slot to the pool"
    );
}
