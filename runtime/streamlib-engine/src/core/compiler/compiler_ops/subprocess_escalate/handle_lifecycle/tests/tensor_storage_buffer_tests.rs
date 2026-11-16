// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Tensor storage buffers: the shape each registers, the tables each leaves on
//! release, and the pool that never rewrites a tensor a consumer holds.

use super::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn acquire_tensor_storage_buffer_over_the_escalate_op(
    sandbox: &crate::core::context::GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    shape: &[u64],
    dtype: &str,
    processor_output_pool: Option<(&str, u32)>,
) -> std::result::Result<String, String> {
    use super::super::super::handle_escalate_op;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
        EscalateRequestAcquireStorageBuffer, EscalateRequestProcessorOutputPool,
    };
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::{
        EscalateRequest, EscalateResponse,
    };

    match handle_escalate_op(
        sandbox,
        registry,
        &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
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
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_tensor_storage_buffer_registers_its_shape_and_leaves_every_table_on_release() {
    use super::super::super::handle_escalate_op;
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
        &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
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
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_tensor_storage_buffer_crosses_on_a_byte_shaped_iosurface_and_leaves_every_table_on_release() {
    use super::super::super::handle_escalate_op;
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
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
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
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_tensor_storage_buffer_pool_never_rewrites_a_tensor_a_consumer_holds() {
    use uuid::Uuid;

    use super::super::release_processor_output_pool_slot;
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
