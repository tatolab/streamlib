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
    use super::{RegisteredHandle, release_surface_share_and_parent_caches_for_handle};
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
        EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
            request_id: "req-iosurface".to_string(),
            width,
            height,
            format: format.wire_name().to_string(),
            processor_output_pool: None,
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
        release_surface_share_and_parent_caches_for_handle(
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

/// A processor output pool over the real escalate op: unheld frames rotate
/// through the ring's depth, a frame a consumer checked out keeps its slot
/// while the producer keeps producing, a recycled frame's id is refused by name
/// at resolve, and the helper's teardown releases every slot.
#[cfg(target_os = "macos")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
fn a_processor_output_pool_never_rewrites_a_frame_a_consumer_holds() {
    use std::sync::Arc;

    use uuid::Uuid;

    use super::super::handle_escalate_op;
    use super::release_processor_output_pool_slot;
    use crate::apple::surface_share::{IOSurfaceShareState, MachSurfaceShareService};
    use crate::core::Error;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
        EscalateRequestAcquireTexture, EscalateRequestProcessorOutputPool,
    };
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::{
        EscalateRequest, EscalateResponse,
    };
    use crate::core::context::{GpuContext, GpuContextLimitedAccess, SurfaceStore};
    use crate::core::rhi::{TextureFormat, pool_slot_key_of_surface_id};

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let state = IOSurfaceShareState::new();
    let mut service = MachSurfaceShareService::new(
        state.clone(),
        format!(
            "com.tatolab.streamlib.processor-output-pool-test.{}.{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ),
    );
    service
        .start()
        .expect("the Mach surface-share service starts");
    let store = SurfaceStore::new_sharing_the_mach_services_tables(
        service.service_name().to_string(),
        "R-processor-output-pool".to_string(),
        Arc::clone(state.check_out_leases()),
        Arc::clone(state.cross_process_timeline_pairs()),
    );
    store.connect().expect("the store connects");
    gpu.set_surface_store(store);
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();
    let (width, height, format) = (64, 32, TextureFormat::Rgba8Unorm);
    let pool_key = format!("processor-output-pool-test-{}", Uuid::new_v4().simple());
    let next_frame_of_extent = |width: u32, height: u32| -> String {
        let response = handle_escalate_op(
            &sandbox,
            &registry,
            EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
                request_id: "req-pooled".to_string(),
                width,
                height,
                format: format.wire_name().to_string(),
                processor_output_pool: Some(EscalateRequestProcessorOutputPool {
                    pool_key: pool_key.clone(),
                    rotation_depth: 2,
                }),
                usage: vec!["texture_binding".to_string()],
            }),
        );
        match response {
            Some(EscalateResponse::Ok(ok)) => ok.handle_id,
            other => panic!("the pooled acquire_texture failed: {other:?}"),
        }
    };
    let next_frame = || next_frame_of_extent(width, height);

    let unheld: Vec<String> = (0..4).map(|_| next_frame()).collect();
    let unheld_slots: std::collections::HashSet<_> = unheld
        .iter()
        .map(|frame| pool_slot_key_of_surface_id(frame).to_string())
        .collect();
    assert_eq!(
        unheld_slots.len(),
        2,
        "with nobody holding a frame the pool rotates through exactly its depth: {unheld:?}"
    );
    assert_ne!(unheld[0], unheld[2], "a republished slot names a new frame");
    assert!(
        state
            .registration_of(pool_slot_key_of_surface_id(&unheld[0]))
            .is_some(),
        "each slot is registered with the surface-share service under its slot key"
    );

    let held_frame = next_frame();
    let consumer = state.check_out_leases().mint_holder_id();
    state
        .check_out_leases()
        .record_check_out_lease(&held_frame, consumer)
        .expect("the current frame checks out");
    let while_held: Vec<String> = (0..4).map(|_| next_frame()).collect();
    assert!(
        while_held
            .iter()
            .all(|frame| pool_slot_key_of_surface_id(frame)
                != pool_slot_key_of_surface_id(&held_frame)),
        "the pool rehanded the slot of a frame a consumer holds: {held_frame} then {while_held:?}"
    );
    sandbox
        .resolve_texture_registration_by_surface_id(&held_frame, None, width, height)
        .expect("the held frame still resolves");

    let recycled_frame = while_held
        .iter()
        .find(|frame| {
            while_held.iter().any(|later| {
                pool_slot_key_of_surface_id(later) == pool_slot_key_of_surface_id(frame)
                    && later != *frame
            })
        })
        .expect("an unheld slot republished while the other was held");
    let Err(refusal) =
        sandbox.resolve_texture_registration_by_surface_id(recycled_frame, None, width, height)
    else {
        panic!("a recycled frame's id {recycled_frame} still resolves");
    };
    assert!(
        matches!(refusal, Error::SurfaceFrameRecycled { .. }),
        "got: {refusal}"
    );

    state
        .check_out_leases()
        .release_one_check_out_lease(&held_frame, consumer)
        .unwrap();

    // An extent change replaces the pool, but a slot a consumer still holds
    // stays registered — and out of the texture pool — until it is released.
    let held_across_the_extent_change = next_frame();
    state
        .check_out_leases()
        .record_check_out_lease(&held_across_the_extent_change, consumer)
        .expect("the current frame checks out");
    let held_slot = pool_slot_key_of_surface_id(&held_across_the_extent_change).to_string();
    next_frame_of_extent(width * 2, height * 2);
    assert!(
        state.registration_of(&held_slot).is_some(),
        "an extent change released a slot a consumer still holds"
    );
    state
        .check_out_leases()
        .release_one_check_out_lease(&held_across_the_extent_change, consumer)
        .unwrap();
    next_frame_of_extent(width * 2, height * 2);
    assert!(
        state.registration_of(&held_slot).is_none(),
        "the retired slot was not released once nothing held it"
    );
    assert!(
        state
            .check_out_leases()
            .record_check_out_lease(&held_across_the_extent_change, consumer)
            .is_err(),
        "a released slot's last frame id still checks out"
    );
    let slot_keys: Vec<String> = registry
        .processor_output_pools()
        .drain_slots()
        .into_iter()
        .map(|released_slot| {
            let pool_slot_key = released_slot.pool_slot_key.clone();
            release_processor_output_pool_slot(&sandbox, released_slot);
            pool_slot_key
        })
        .collect();
    assert!(!slot_keys.is_empty());
    for pool_slot_key in &slot_keys {
        assert!(
            state.registration_of(pool_slot_key).is_none(),
            "teardown left slot {pool_slot_key} registered"
        );
    }
}

/// Ask one of the helper's processor output pools for its next frame over the
/// real escalate op, answering the published frame id or the refusal.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn next_processor_output_frame_over_the_escalate_op(
    sandbox: &crate::core::context::GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    pool_key: &str,
    rotation_depth: u32,
) -> std::result::Result<String, String> {
    use super::super::handle_escalate_op;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
        EscalateRequestAcquireTexture, EscalateRequestProcessorOutputPool,
    };
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::{
        EscalateRequest, EscalateResponse,
    };
    use crate::core::rhi::TextureFormat;

    match handle_escalate_op(
        sandbox,
        registry,
        EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
            request_id: "req-pooled".to_string(),
            width: 64,
            height: 32,
            format: TextureFormat::Rgba8Unorm.wire_name().to_string(),
            processor_output_pool: Some(EscalateRequestProcessorOutputPool {
                pool_key: pool_key.to_string(),
                rotation_depth,
            }),
            usage: vec!["texture_binding".to_string()],
        }),
    ) {
        Some(EscalateResponse::Ok(ok)) => Ok(ok.handle_id),
        Some(EscalateResponse::Err(err)) => Err(err.message),
        None => Err("the pooled acquire_texture got no response".to_string()),
    }
}

/// A reused slot is handed off with an escalate scope held open on the same
/// thread — a hand-off that entered the gate would panic on the re-entry — while
/// growing the pool still enters it, and the pool is usable once the scope
/// closes.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
fn a_reused_processor_output_slot_skips_the_escalate_gate_and_growth_enters_it() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use uuid::Uuid;

    use super::release_processor_output_pool_slot;
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};
    use crate::core::rhi::pool_slot_key_of_surface_id;

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();
    let pool_key = format!("gateless-reuse-test-{}", Uuid::new_v4().simple());
    let next_frame = |rotation_depth: u32| {
        next_processor_output_frame_over_the_escalate_op(
            &sandbox,
            &registry,
            &pool_key,
            rotation_depth,
        )
    };

    let first_frame = next_frame(1).expect("the first frame allocates its slot");
    let escalate_gate = sandbox.host_inner().escalate_gate();
    escalate_gate.enter();
    let reused_frame = next_frame(1);
    let growth_under_the_held_gate = catch_unwind(AssertUnwindSafe(|| next_frame(2)));
    escalate_gate.exit();

    let reused_frame = reused_frame.expect("a reuse hand-off completes while the gate is held");
    assert_eq!(
        pool_slot_key_of_surface_id(&reused_frame),
        pool_slot_key_of_surface_id(&first_frame),
        "with nothing held a depth-1 pool republishes its one slot"
    );
    assert_ne!(
        reused_frame, first_frame,
        "a republished slot names a new frame"
    );
    let Err(growth_panic) = growth_under_the_held_gate else {
        panic!("growing the pool must allocate inside the escalate scope");
    };
    let growth_panic_message = growth_panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| growth_panic.downcast_ref::<&str>().copied())
        .unwrap_or_default();
    assert!(
        growth_panic_message.contains("EscalateGate::enter() called twice"),
        "growth panicked for a reason other than re-entering the gate: {growth_panic_message}"
    );

    let grown_frame = next_frame(2).expect("the pool grows once the scope closes");
    assert_ne!(
        pool_slot_key_of_surface_id(&grown_frame),
        pool_slot_key_of_surface_id(&first_frame)
    );

    for released_slot in registry.processor_output_pools().drain_slots() {
        release_processor_output_pool_slot(&sandbox, released_slot);
    }
    let refusal = next_frame(2).expect_err("a hand-off after teardown is refused");
    assert!(refusal.contains("torn down"), "got: {refusal}");
}

/// A slot allocated while the helper's teardown drained its pools is handed
/// back for release rather than added to a pool nothing drains again.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
fn a_slot_allocated_across_the_helpers_teardown_is_handed_back_for_release() {
    use super::super::acquisition::parse_texture_usages;
    use super::super::handle_escalate_op;
    use super::{
        ProcessorOutputFreshSlotHandOff, ProcessorOutputSlotDescriptor,
        ProcessorOutputTextureDescriptor, release_processor_output_pool_slot,
    };
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestAcquireTexture;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::{
        EscalateRequest, EscalateResponse,
    };
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};
    use crate::core::rhi::TextureFormat;

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();
    let usage = vec!["texture_binding".to_string()];
    let descriptor = ProcessorOutputTextureDescriptor {
        width: 64,
        height: 32,
        format: TextureFormat::Rgba8Unorm,
        usage: parse_texture_usages(&usage).unwrap(),
    };
    let Some(EscalateResponse::Ok(allocated)) = handle_escalate_op(
        &sandbox,
        &registry,
        EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
            request_id: "req-fresh-slot".to_string(),
            width: descriptor.width,
            height: descriptor.height,
            format: descriptor.format.wire_name().to_string(),
            processor_output_pool: None,
            usage,
        }),
    ) else {
        panic!("the texture a fresh slot stands for was not allocated");
    };
    let registered_texture = registry
        .remove_handle(&allocated.handle_id)
        .expect("the allocation is registered");

    let mut pools = registry.processor_output_pools();
    assert!(pools.drain_slots().is_empty());
    let handed_off = pools.hand_off_a_fresh_slot(
        sandbox.host_inner(),
        "pool-torn-down-mid-allocation",
        &ProcessorOutputSlotDescriptor::Texture(descriptor),
        allocated.handle_id.clone(),
        registered_texture,
    );
    drop(pools);
    let ProcessorOutputFreshSlotHandOff::Refused {
        refusal,
        slot_owed_its_release: released_slot,
    } = handed_off
    else {
        panic!("a slot handed in after teardown was added to a pool");
    };
    assert!(refusal.to_string().contains("torn down"), "got: {refusal}");
    assert_eq!(released_slot.pool_slot_key, allocated.handle_id);
    release_processor_output_pool_slot(&sandbox, released_slot);
}

/// A live surface-share service wired into a GPU context as its store, so an
/// escalate acquire registers exactly as a helper's would.
#[cfg(target_os = "linux")]
struct LiveSurfaceShareServiceForATest {
    state: crate::linux::surface_share::SurfaceShareState,
    service: crate::linux::surface_share::UnixSocketSurfaceService,
    _socket_dir: tempfile::TempDir,
}

#[cfg(target_os = "linux")]
impl LiveSurfaceShareServiceForATest {
    fn wired_into(gpu: &crate::core::context::GpuContext) -> Self {
        use std::sync::Arc;

        use crate::core::context::SurfaceStore;
        use crate::linux::surface_share::{SurfaceShareState, UnixSocketSurfaceService};

        let socket_dir = crate::core::test_support::a_temporary_directory_at_owner_only_mode()
            .expect("temp dir for the test socket");
        let socket_path = socket_dir.path().join("surface-share.sock");
        let state = SurfaceShareState::new();
        let mut service = UnixSocketSurfaceService::new(state.clone(), socket_path.clone());
        service.start().expect("service start");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !socket_path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let store = SurfaceStore::new_reading_check_out_leases(
            socket_path.to_string_lossy().into_owned(),
            "R-tensor-storage-buffer".to_string(),
            Arc::clone(state.check_out_leases()),
        );
        store.connect().expect("the store connects");
        gpu.set_surface_store(store);
        Self {
            state,
            service,
            _socket_dir: socket_dir,
        }
    }

    fn check_out_leases(&self) -> &crate::core::context::SurfaceCheckOutLeaseRegistry {
        self.state.check_out_leases()
    }

    fn is_registered(&self, surface_id: &str) -> bool {
        self.state.get_surface_planes(surface_id).is_some()
    }

    fn stop(mut self) {
        self.service.stop();
    }
}

#[cfg(target_os = "macos")]
struct LiveSurfaceShareServiceForATest {
    state: crate::apple::surface_share::IOSurfaceShareState,
    service: crate::apple::surface_share::MachSurfaceShareService,
}

#[cfg(target_os = "macos")]
impl LiveSurfaceShareServiceForATest {
    fn wired_into(gpu: &crate::core::context::GpuContext) -> Self {
        use std::sync::Arc;

        use crate::apple::surface_share::{IOSurfaceShareState, MachSurfaceShareService};
        use crate::core::context::SurfaceStore;

        let state = IOSurfaceShareState::new();
        let mut service = MachSurfaceShareService::new(
            state.clone(),
            format!(
                "com.tatolab.streamlib.escalate-tensor-test.{}.{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ),
        );
        service
            .start()
            .expect("the Mach surface-share service starts");
        let store = SurfaceStore::new_sharing_the_mach_services_tables(
            service.service_name().to_string(),
            "R-tensor-storage-buffer".to_string(),
            Arc::clone(state.check_out_leases()),
            Arc::clone(state.cross_process_timeline_pairs()),
        );
        store.connect().expect("the store connects");
        gpu.set_surface_store(store);
        Self { state, service }
    }

    fn check_out_leases(&self) -> &crate::core::context::SurfaceCheckOutLeaseRegistry {
        self.state.check_out_leases()
    }

    fn is_registered(&self, surface_id: &str) -> bool {
        self.state.registration_of(surface_id).is_some()
    }

    fn stop(mut self) {
        self.service.stop();
    }
}

/// Acquire a tensor storage buffer over the real escalate op, from a processor
/// output pool when one is named, answering the surface id or the refusal.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn acquire_tensor_storage_buffer_over_the_escalate_op(
    sandbox: &crate::core::context::GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    shape: &[u64],
    dtype: &str,
    processor_output_pool: Option<(&str, u32)>,
) -> std::result::Result<String, String> {
    use super::super::handle_escalate_op;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
        EscalateRequestAcquireStorageBuffer, EscalateRequestProcessorOutputPool,
    };
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::{
        EscalateRequest, EscalateResponse,
    };

    match handle_escalate_op(
        sandbox,
        registry,
        EscalateRequest::AcquireStorageBuffer(EscalateRequestAcquireStorageBuffer {
            request_id: "req-tensor".to_string(),
            shape: shape.to_vec(),
            dtype: dtype.to_string(),
            processor_output_pool: processor_output_pool.map(|(pool_key, rotation_depth)| {
                EscalateRequestProcessorOutputPool {
                    pool_key: pool_key.to_string(),
                    rotation_depth,
                }
            }),
        }),
    ) {
        Some(EscalateResponse::Ok(ok)) => {
            assert_eq!(
                ok.shape.as_deref(),
                Some(shape),
                "the reply echoes the shape"
            );
            assert_eq!(
                ok.dtype.as_deref(),
                Some(dtype),
                "the reply echoes the dtype"
            );
            Ok(ok.handle_id)
        }
        Some(EscalateResponse::Err(err)) => Err(err.message),
        None => Err("acquire_storage_buffer got no response".to_string()),
    }
}

/// A one-off tensor acquire registers a `storage_buffer` surface carrying its
/// shape and dtype, enters the parent-wide map, and leaves both on release.
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
fn a_tensor_storage_buffer_registers_its_shape_and_leaves_every_table_on_release() {
    use super::super::handle_escalate_op;
    use crate::core::Error;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestReleaseHandle;
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};
    use crate::core::rhi::{TensorElementType, TensorStorageBufferLayout};

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();

    let surface_id = acquire_tensor_storage_buffer_over_the_escalate_op(
        &sandbox,
        &registry,
        &[3, 7, 11],
        "float16",
        None,
    )
    .expect("a one-off tensor storage buffer is acquired");
    let expected_layout =
        TensorStorageBufferLayout::new(vec![3, 7, 11], TensorElementType::Float16)
            .expect("an odd shape is valid");
    let checkout = live_service
        .state
        .get_surface_planes(&surface_id)
        .expect("the tensor is registered with the surface-share service");
    assert_eq!(checkout.handle_type, "opaque_fd");
    assert_eq!(
        checkout.plane_sizes,
        vec![expected_layout.byte_size()],
        "the registered size is the tensor's exact byte size, not the allocation's"
    );
    assert_eq!(checkout.tensor_layout.as_ref(), Some(&expected_layout));
    assert!(checkout.vk_memory_type_index.is_some());
    assert!(checkout.exporting_device_uuid.is_some());
    let registered = sandbox
        .host_inner()
        .resolve_storage_buffer_from_the_parent_wide_map_by_surface_id(&surface_id)
        .expect("the parent-wide map resolves the tensor by its id");
    assert_eq!(registered.byte_size(), expected_layout.byte_size());
    drop(registered);

    let released = handle_escalate_op(
        &sandbox,
        &registry,
        EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: "req-release-tensor".to_string(),
            handle_id: surface_id.clone(),
        }),
    );
    assert!(matches!(
        released,
        Some(crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse::Ok(_))
    ));
    assert!(!live_service.is_registered(&surface_id));
    assert!(matches!(
        sandbox
            .host_inner()
            .resolve_storage_buffer_from_the_parent_wide_map_by_surface_id(&surface_id),
        Err(Error::NotFound(_))
    ));
    live_service.stop();
}

/// On macOS a one-off tensor acquire allocates a byte-shaped private IOSurface
/// of 16 KiB rows, registers it as a `storage_buffer` surface carrying its
/// shape and dtype, enters the parent-wide map over those same pages, and
/// leaves both on release.
#[cfg(target_os = "macos")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
fn a_tensor_storage_buffer_crosses_on_a_byte_shaped_iosurface_and_leaves_every_table_on_release() {
    use super::super::handle_escalate_op;
    use crate::core::Error;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestReleaseHandle;
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};
    use crate::core::rhi::{TensorElementType, TensorStorageBufferLayout};

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();

    // 16385 bytes: one byte past a row, so the surface takes a second row.
    for (shape, dtype, element_type, row_count) in [
        (vec![3, 7, 11], "float16", TensorElementType::Float16, 1),
        (vec![16385], "uint8", TensorElementType::Uint8, 2),
    ] {
        let surface_id = acquire_tensor_storage_buffer_over_the_escalate_op(
            &sandbox, &registry, &shape, dtype, None,
        )
        .expect("a one-off tensor storage buffer is acquired");
        let expected_layout =
            TensorStorageBufferLayout::new(shape.clone(), element_type).expect("a valid shape");
        let registration = live_service
            .state
            .registration_of(&surface_id)
            .expect("the tensor is registered with the Mach surface-share service");
        assert_eq!(registration.resource_type, "storage_buffer");
        assert_eq!(registration.tensor_layout.as_ref(), Some(&expected_layout));
        assert_eq!(registration.iosurface.bytes_per_element(), 1);
        assert_eq!(
            registration.iosurface.bytes_per_row(),
            16384,
            "{shape:?}: rows are one packed 16 KiB page"
        );
        assert_eq!(registration.iosurface.height(), row_count, "{shape:?}");
        let registered = sandbox
            .host_inner()
            .resolve_storage_buffer_from_the_parent_wide_map_by_surface_id(&surface_id)
            .expect("the parent-wide map resolves the tensor by its id");
        assert_eq!(
            registered.byte_size(),
            expected_layout.byte_size(),
            "{shape:?}: the buffer spans exactly the tensor"
        );
        let backing_iosurface = registered
            .host_inner()
            .backing_iosurface()
            .expect("the buffer is IOSurface-backed");
        assert_eq!(
            backing_iosurface.id(),
            registration.iosurface.id(),
            "{shape:?}: the service shares the pages the buffer imports"
        );
        drop(registered);

        let released = handle_escalate_op(
            &sandbox,
            &registry,
            EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
                request_id: "req-release-tensor".to_string(),
                handle_id: surface_id.clone(),
            }),
        );
        assert!(matches!(
            released,
            Some(crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse::Ok(_))
        ));
        assert!(!live_service.is_registered(&surface_id));
        assert!(matches!(
            sandbox
                .host_inner()
                .resolve_storage_buffer_from_the_parent_wide_map_by_surface_id(&surface_id),
            Err(Error::NotFound(_))
        ));
    }
    live_service.stop();
}

/// A pooled tensor acquire never rehands the slot of a tensor a consumer
/// holds, a recycled tensor's id is refused by name, and teardown leaves no
/// registration behind.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
fn a_tensor_storage_buffer_pool_never_rewrites_a_tensor_a_consumer_holds() {
    use uuid::Uuid;

    use super::release_processor_output_pool_slot;
    use crate::core::Error;
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};
    use crate::core::rhi::pool_slot_key_of_surface_id;

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();
    let pool_key = format!("tensor-pool-test-{}", Uuid::new_v4().simple());
    let next_tensor = || {
        acquire_tensor_storage_buffer_over_the_escalate_op(
            &sandbox,
            &registry,
            &[1, 3, 640, 640],
            "float32",
            Some((&pool_key, 2)),
        )
        .expect("the pooled tensor acquire succeeds")
    };

    let unheld: Vec<String> = (0..4).map(|_| next_tensor()).collect();
    let unheld_slots: std::collections::HashSet<_> = unheld
        .iter()
        .map(|tensor| pool_slot_key_of_surface_id(tensor).to_string())
        .collect();
    assert_eq!(
        unheld_slots.len(),
        2,
        "with nobody holding a tensor the pool rotates through exactly its depth: {unheld:?}"
    );

    let held_tensor = next_tensor();
    let consumer = live_service.check_out_leases().mint_holder_id();
    live_service
        .check_out_leases()
        .record_check_out_lease(&held_tensor, consumer)
        .expect("the current tensor checks out");
    let while_held: Vec<String> = (0..4).map(|_| next_tensor()).collect();
    assert!(
        while_held
            .iter()
            .all(|tensor| pool_slot_key_of_surface_id(tensor)
                != pool_slot_key_of_surface_id(&held_tensor)),
        "the pool rehanded the slot of a tensor a consumer holds: {held_tensor} then {while_held:?}"
    );
    sandbox
        .host_inner()
        .resolve_storage_buffer_from_the_parent_wide_map_by_surface_id(&held_tensor)
        .expect("the held tensor still resolves");
    let recycled_tensor = while_held
        .iter()
        .find(|tensor| {
            while_held.iter().any(|later| {
                pool_slot_key_of_surface_id(later) == pool_slot_key_of_surface_id(tensor)
                    && later != *tensor
            })
        })
        .expect("an unheld slot republished while the other was held");
    assert!(
        matches!(
            sandbox
                .host_inner()
                .resolve_storage_buffer_from_the_parent_wide_map_by_surface_id(recycled_tensor),
            Err(Error::SurfaceFrameRecycled { .. })
        ),
        "a recycled tensor's id {recycled_tensor} must be refused as recycled"
    );
    live_service
        .check_out_leases()
        .release_one_check_out_lease(&held_tensor, consumer)
        .unwrap();

    let slot_keys: Vec<String> = registry
        .processor_output_pools()
        .drain_slots()
        .into_iter()
        .map(|released_slot| {
            let pool_slot_key = released_slot.pool_slot_key.clone();
            release_processor_output_pool_slot(&sandbox, released_slot);
            pool_slot_key
        })
        .collect();
    assert!(!slot_keys.is_empty());
    for pool_slot_key in &slot_keys {
        assert!(
            !live_service.is_registered(pool_slot_key),
            "teardown left tensor slot {pool_slot_key} registered"
        );
        assert!(
            sandbox
                .host_inner()
                .resolve_storage_buffer_from_the_parent_wide_map_by_surface_id(pool_slot_key)
                .is_err(),
            "teardown left tensor slot {pool_slot_key} in the parent-wide map"
        );
    }
    live_service.stop();
}
