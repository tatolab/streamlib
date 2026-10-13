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
    gpu.install_the_engines_surface_store(store)
        .expect("the engine's surface store installs once");
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

    let held_slot_id = registry.read_the_one_off_held_under(&handle_id, |held| match held {
        Some(RegisteredHandle::Texture {
            texture,
            timeline_pair,
        }) => {
            assert!(timeline_pair.is_some());
            texture.slot_id()
        }
        _ => panic!("the registry holds the texture"),
    });
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
    gpu.install_the_engines_surface_store(store)
        .expect("the engine's surface store installs once");
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
        gpu.install_the_engines_surface_store(store)
            .expect("the engine's surface store installs once");
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
        gpu.install_the_engines_surface_store(store)
            .expect("the engine's surface store installs once");
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

/// The handle id an escalate acquire answered, or its refusal's message.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn handle_id_or_refusal_of(
    response: Option<
        crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse,
    >,
) -> std::result::Result<String, String> {
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;

    match response {
        Some(EscalateResponse::Ok(ok)) => Ok(ok.handle_id),
        Some(EscalateResponse::Err(err)) => Err(err.message),
        None => Err("the escalate op got no response".to_string()),
    }
}

/// A one-off 64x32 `rgba8_unorm` `texture_binding` texture request, the
/// shape the per-node count tests acquire.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn a_one_off_64x32_rgba8_unorm_texture_request()
-> crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest {
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestAcquireTexture;

    EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
        request_id: "req-one-off-64x32".to_string(),
        width: 64,
        height: 32,
        format: crate::core::rhi::TextureFormat::Rgba8Unorm
            .wire_name()
            .to_string(),
        processor_output_pool: None,
        usage: vec!["texture_binding".to_string()],
    })
}

/// The shape a one-off 64x32 `rgba8_unorm` `texture_binding` texture is
/// counted under, the wire's implied copy bits included.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn the_64x32_rgba8_unorm_one_off_texture_shape() -> super::OneOffAcquisitionShape {
    super::OneOffAcquisitionShape::Texture {
        width: 64,
        height: 32,
        format: crate::core::rhi::TextureFormat::Rgba8Unorm,
        usage: super::super::acquisition::parse_texture_usages(&["texture_binding".to_string()])
            .expect("a known usage"),
    }
}

/// Acquire a one-off 64x32 `rgba8_unorm` texture over the real escalate op,
/// answering its handle id or the refusal.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(
    sandbox: &crate::core::context::GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
) -> std::result::Result<String, String> {
    handle_id_or_refusal_of(super::super::handle_escalate_op(
        sandbox,
        registry,
        a_one_off_64x32_rgba8_unorm_texture_request(),
    ))
}

/// Release one handle over the real escalate op, answering the refusal if
/// the release was refused.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn release_a_handle_over_the_escalate_op(
    sandbox: &crate::core::context::GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    handle_id: &str,
) -> std::result::Result<(), String> {
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestReleaseHandle;

    handle_id_or_refusal_of(super::super::handle_escalate_op(
        sandbox,
        registry,
        EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: "req-release".to_string(),
            handle_id: handle_id.to_string(),
        }),
    ))
    .map(|_| ())
}

/// Run every release a helper's teardown owes, as its bridge's drop does.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn release_everything_the_helpers_teardown_owes(
    sandbox: &crate::core::context::GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
) {
    use super::{
        release_processor_output_pool_slot, release_surface_share_and_parent_caches_for_handle,
    };

    for (handle_id, removed_handle) in registry.drain_handles() {
        release_surface_share_and_parent_caches_for_handle(sandbox, &handle_id, &removed_handle);
    }
    for released_slot in registry.processor_output_pools().drain_slots() {
        release_processor_output_pool_slot(sandbox, released_slot);
    }
}

/// One node holding every frame its output pool may grow to, and every
/// one-off of a shape it may hold, refuses only itself: a second node's
/// pooled frame and one-off of the same shape are both served.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
#[allow(clippy::disallowed_macros)]
fn one_nodes_held_frames_never_refuse_another_nodes_frames_of_the_same_shape() {
    use uuid::Uuid;

    use super::one_off_acquisitions_held_for_one_node::ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE;
    use crate::core::context::processor_output_surface_pool::PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY;
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let node_a = EscalateHandleRegistry::new();
    let node_b = EscalateHandleRegistry::new();
    let node_a_pool_key = format!("node-a-pool-{}", Uuid::new_v4().simple());
    let node_b_pool_key = format!("node-b-pool-{}", Uuid::new_v4().simple());
    let consumer = live_service.check_out_leases().mint_holder_id();

    let node_a_frames_the_consumer_holds: Vec<String> = (0..PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY)
        .map(|_| {
            let frame = next_processor_output_frame_over_the_escalate_op(
                &sandbox,
                &node_a,
                &node_a_pool_key,
                2,
            )
            .expect("node A's output pool grows while a consumer holds its frames");
            live_service
                .check_out_leases()
                .record_check_out_lease(&frame, consumer)
                .expect("the consumer holds every frame node A publishes");
            frame
        })
        .collect();
    let node_a_pool_refusal =
        next_processor_output_frame_over_the_escalate_op(&sandbox, &node_a, &node_a_pool_key, 2)
            .expect_err("node A's seventeenth held frame is refused");
    assert!(
        node_a_pool_refusal.contains(&node_a_pool_key),
        "the refusal names node A's own pool: {node_a_pool_refusal}"
    );

    for _ in 0..ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE {
        acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(&sandbox, &node_a)
            .expect("node A acquires one-offs up to its own count of the shape");
    }
    let node_a_one_off_refusal =
        acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(&sandbox, &node_a)
            .expect_err("node A's seventeenth one-off of the shape is refused");
    assert!(
        node_a_one_off_refusal.contains("64x32 rgba8_unorm")
            && node_a_one_off_refusal.contains("16"),
        "the refusal names the shape and the count: {node_a_one_off_refusal}"
    );

    next_processor_output_frame_over_the_escalate_op(&sandbox, &node_b, &node_b_pool_key, 2)
        .expect("node B's pooled frame of the same shape is served while node A holds its own");
    acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(&sandbox, &node_b)
        .expect("node B's one-off of the same shape is served while node A holds its own");

    for frame in &node_a_frames_the_consumer_holds {
        live_service
            .check_out_leases()
            .release_one_check_out_lease(frame, consumer)
            .expect("the consumer lets each frame go");
    }
    release_everything_the_helpers_teardown_owes(&sandbox, &node_a);
    release_everything_the_helpers_teardown_owes(&sandbox, &node_b);
    live_service.stop();
}

/// At the device's memory budget every escalate acquisition that has to
/// allocate — a one-off, a pooled frame, a pixel buffer, a tensor — is
/// refused at once, never sitting out a wait for a holder to let go. The held
/// texture is released only after every answer is in, so an acquire that
/// waited would miss its answer's deadline rather than be rescued.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
#[allow(clippy::disallowed_macros)]
fn an_acquisition_at_the_budget_refuses_at_once_and_never_waits() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use uuid::Uuid;

    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
        EscalateRequestAcquirePixelBuffer, EscalateRequestAcquireStorageBuffer,
        EscalateRequestAcquireTexture, EscalateRequestProcessorOutputPool,
    };
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();
    let held_one_off =
        acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(&sandbox, &registry)
            .expect("the one-off held through the test is acquired");
    let held_shape = the_64x32_rgba8_unorm_one_off_texture_shape();

    sandbox
        .host_inner()
        .device()
        .inner
        .leave_no_room_in_any_frame_allocation_budget_for_this_test();

    let a_storage_buffer_request = |shape: Vec<u64>, pool_key: Option<String>| {
        EscalateRequest::AcquireStorageBuffer(EscalateRequestAcquireStorageBuffer {
            request_id: "req-tensor-at-the-budget".to_string(),
            shape,
            dtype: "float32".to_string(),
            processor_output_pool: pool_key.map(|pool_key| EscalateRequestProcessorOutputPool {
                pool_key,
                rotation_depth: 2,
            }),
        })
    };
    let requests_at_the_budget: Vec<(&'static str, EscalateRequest)> = vec![
        (
            "a one-off texture of the held texture's shape",
            a_one_off_64x32_rgba8_unorm_texture_request(),
        ),
        (
            "a pooled texture of a shape nobody holds",
            EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
                request_id: "req-pooled-at-the-budget".to_string(),
                width: 96,
                height: 48,
                format: "rgba8_unorm".to_string(),
                processor_output_pool: Some(EscalateRequestProcessorOutputPool {
                    pool_key: format!("budget-texture-pool-{}", Uuid::new_v4().simple()),
                    rotation_depth: 2,
                }),
                usage: vec!["texture_binding".to_string()],
            }),
        ),
        (
            "a pixel buffer of a fresh shape",
            EscalateRequest::AcquirePixelBuffer(EscalateRequestAcquirePixelBuffer {
                request_id: "req-pixel-buffer-at-the-budget".to_string(),
                width: 48,
                height: 16,
                format: "bgra".to_string(),
            }),
        ),
        (
            "a one-off tensor",
            a_storage_buffer_request(vec![1, 3, 16, 16], None),
        ),
        (
            "a pooled tensor",
            a_storage_buffer_request(
                vec![1, 3, 32, 32],
                Some(format!("budget-tensor-pool-{}", Uuid::new_v4().simple())),
            ),
        ),
    ];
    let request_count = requests_at_the_budget.len();

    let (release_signal_tx, release_signal_rx) = mpsc::channel::<()>();
    let (answer_tx, answer_rx) =
        mpsc::channel::<(&'static str, Duration, std::result::Result<String, String>)>();
    std::thread::scope(|scope| {
        let releasing_sandbox = sandbox.clone();
        let releasing_registry = std::sync::Arc::clone(&registry);
        let held_one_off = held_one_off.clone();
        scope.spawn(move || {
            if release_signal_rx.recv().is_ok() {
                release_a_handle_over_the_escalate_op(
                    &releasing_sandbox,
                    &releasing_registry,
                    &held_one_off,
                )
                .expect("the held one-off releases");
            }
        });
        let requesting_sandbox = sandbox.clone();
        let requesting_registry = std::sync::Arc::clone(&registry);
        scope.spawn(move || {
            for (what_is_requested, request) in requests_at_the_budget {
                let requested_at = Instant::now();
                let answer = handle_id_or_refusal_of(super::super::handle_escalate_op(
                    &requesting_sandbox,
                    &requesting_registry,
                    request,
                ));
                if answer_tx
                    .send((what_is_requested, requested_at.elapsed(), answer))
                    .is_err()
                {
                    return;
                }
            }
        });

        for _ in 0..request_count {
            let (what_is_requested, answered_after, answer) = answer_rx
                .recv_timeout(Duration::from_millis(500))
                .expect("every acquisition at the budget is answered at once, not after a wait");
            let refusal = answer.expect_err(what_is_requested);
            assert!(
                refusal.contains("refused a")
                    && refusal.contains("frame allocation before allocating"),
                "{what_is_requested}: the refusal is the device memory budget's, by name: \
                 {refusal}"
            );
            assert!(
                answered_after < Duration::from_millis(250),
                "{what_is_requested}: answered after {answered_after:?}, so it waited"
            );
        }
        assert_eq!(registry.handle_count(), 1, "only the held one-off is held");
        assert_eq!(
            registry.one_off_acquisitions_held_or_reserved_of_shape(&held_shape),
            1,
            "the refused one-off handed its reservation back"
        );
        release_signal_tx
            .send(())
            .expect("the releasing thread is waiting for the signal");
    });

    assert_eq!(registry.handle_count(), 0);
    release_everything_the_helpers_teardown_owes(&sandbox, &registry);
    live_service.stop();
}

/// A one-off's count goes back to its node when the helper releases it, and
/// every count it holds goes back at teardown.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[allow(clippy::disallowed_macros)]
fn a_released_or_torn_down_one_off_hands_its_count_back_to_its_node() {
    use super::RegisteredHandle;
    use super::one_off_acquisitions_held_for_one_node::ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE;
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();
    let shape = the_64x32_rgba8_unorm_one_off_texture_shape();
    let acquire_every_one_off_the_node_may_hold = || -> Vec<String> {
        (0..ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE)
            .map(|_| {
                acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(
                    &sandbox, &registry,
                )
                .expect("the node acquires up to its own count of the shape")
            })
            .collect()
    };

    let held = acquire_every_one_off_the_node_may_hold();
    assert!(
        registry.read_the_one_off_held_under(&held[0], |held_one_off| {
            matches!(held_one_off, Some(RegisteredHandle::Texture { .. }))
        }),
        "a one-off texture is held under the id the helper was answered with"
    );
    let refusal =
        acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(&sandbox, &registry)
            .expect_err("the seventeenth is refused");
    assert!(refusal.contains("64x32 rgba8_unorm"), "got: {refusal}");

    release_a_handle_over_the_escalate_op(&sandbox, &registry, &held[0])
        .expect("the helper releases one");
    assert_eq!(
        registry.one_off_acquisitions_held_or_reserved_of_shape(&shape),
        ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE - 1
    );
    acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(&sandbox, &registry)
        .expect("a released one-off's count makes room for one more");

    release_everything_the_helpers_teardown_owes(&sandbox, &registry);
    assert_eq!(registry.handle_count(), 0);
    assert_eq!(
        registry.one_off_acquisitions_held_or_reserved_of_shape(&shape),
        0
    );
    acquire_every_one_off_the_node_may_hold();
    release_everything_the_helpers_teardown_owes(&sandbox, &registry);
}

/// Releasing a one-off pixel buffer evicts the lookup its check-in id cached
/// and leaves its slot's published frame cached: on macOS the one-off's id is
/// that published frame's, which the pool alone retires.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[allow(clippy::disallowed_macros)]
fn releasing_a_one_off_pixel_buffer_evicts_its_lookup_and_leaves_its_slots_published_frame_cached()
{
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestAcquirePixelBuffer;
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();

    let handle_id = handle_id_or_refusal_of(super::super::handle_escalate_op(
        &sandbox,
        &registry,
        EscalateRequest::AcquirePixelBuffer(EscalateRequestAcquirePixelBuffer {
            request_id: "req-one-off-pixel-buffer".to_string(),
            width: 64,
            height: 32,
            format: "bgra".to_string(),
        }),
    ))
    .expect("a one-off pixel buffer is acquired");
    let published_frame_ids = sandbox
        .host_inner()
        .published_pixel_buffer_frame_ids_cached_in_this_process();
    let [published_frame_id] = published_frame_ids.as_slice() else {
        panic!("one hand-off publishes one frame: {published_frame_ids:?}");
    };
    #[cfg(target_os = "linux")]
    {
        assert_ne!(
            &handle_id, published_frame_id,
            "on Linux a one-off is checked in under its own id"
        );
        sandbox
            .host_inner()
            .get_pixel_buffer(&handle_id)
            .expect("the check-in id resolves through the surface store");
        assert!(
            sandbox
                .host_inner()
                .pooled_backing_held_in_this_process(&handle_id)
                .is_some(),
            "resolving the check-in id caches its lookup"
        );
    }
    #[cfg(target_os = "macos")]
    assert_eq!(
        &handle_id, published_frame_id,
        "on macOS a one-off's id is its slot's published frame"
    );

    release_a_handle_over_the_escalate_op(&sandbox, &registry, &handle_id)
        .expect("the helper releases the one-off");

    assert!(
        sandbox
            .host_inner()
            .pooled_backing_held_in_this_process(published_frame_id)
            .is_some(),
        "releasing a one-off leaves its slot's published frame cached"
    );
    #[cfg(target_os = "linux")]
    assert!(
        sandbox
            .host_inner()
            .pooled_backing_held_in_this_process(&handle_id)
            .is_none(),
        "releasing a one-off evicts the lookup its check-in id cached"
    );
    live_service.stop();
}
