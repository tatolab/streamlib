// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Compute dispatches over tensor storage buffers: the tensor a batch's passes
//! hand along, the buffer a surface id names, and the ids refused.

use super::*;

/// Elements in the tensor the buffer tests write — enough work groups that
/// pass 2 would overlap pass 1 on the device if no barrier ordered them.
const TENSOR_ELEMENT_COUNT: usize = 1 << 20;

/// A tensor pass 1 has not written reads as this.
const TENSOR_UNWRITTEN_SENTINEL: f32 = -1.0;

/// Pass 1 of the tensor chain: every element takes its own index.
const WRITE_INDEX_PATTERN_SLANG: &str = "\
[[vk::binding(0, 0)]] RWStructuredBuffer<float> index_pattern_tensor;
[numthreads(64, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    index_pattern_tensor[invocation.x] = float(invocation.x);
}
";

/// Pass 2 of the tensor chain: every element of pass 1's tensor, doubled.
const DOUBLE_TENSOR_SLANG: &str = "\
[[vk::binding(0, 0)]] StructuredBuffer<float> undoubled_tensor;
[[vk::binding(1, 0)]] RWStructuredBuffer<float> doubled_tensor;
[numthreads(64, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    doubled_tensor[invocation.x] = undoubled_tensor[invocation.x] * 2.0;
}
";

/// A HOST_VISIBLE float32 tensor of [`TENSOR_ELEMENT_COUNT`] elements, every
/// one set to [`TENSOR_UNWRITTEN_SENTINEL`].
fn sentinel_filled_tensor_storage_buffer(
    full: &crate::core::context::GpuContextFullAccess,
) -> crate::core::error::Result<crate::core::rhi::StorageBuffer> {
    use crate::core::rhi::{TensorElementType, TensorStorageBufferLayout};
    let layout = TensorStorageBufferLayout::new(
        vec![TENSOR_ELEMENT_COUNT as u64],
        TensorElementType::Float32,
    )?;
    let buffer = full.host_inner().acquire_storage_buffer(&layout)?;
    tensor_values_mut(&buffer).fill(TENSOR_UNWRITTEN_SENTINEL);
    Ok(buffer)
}

/// The mapped elements of a HOST_VISIBLE float32 tensor.
#[allow(clippy::mut_from_ref)]
fn tensor_values_mut(buffer: &crate::core::rhi::StorageBuffer) -> &mut [f32] {
    assert!(
        !buffer.mapped_ptr().is_null(),
        "the test tensor is HOST_VISIBLE"
    );
    // SAFETY: a HOST_VISIBLE storage buffer is persistently mapped for its
    // lifetime, and every test reads it only after the dispatch's fence wait.
    unsafe {
        std::slice::from_raw_parts_mut(
            buffer.mapped_ptr() as *mut f32,
            buffer.byte_size() as usize / std::mem::size_of::<f32>(),
        )
    }
}

fn assert_every_element_is(values: &[f32], expected: impl Fn(usize) -> f32, what: &str) {
    for (index, value) in values.iter().enumerate() {
        assert_eq!(*value, expected(index), "element {index} of {what}");
    }
}

/// The batch recorder's buffer barrier: pass 2 reads what pass 1 wrote to one
/// tensor, inside one recording.
///
/// Built on `GpuContext` directly rather than through the escalate op, so it
/// needs no surface-id map and runs on every device the engine opens. A driver
/// that happens to finish pass 1 first reads back correctly with no barrier at
/// all, so under `STREAMLIB_VULKAN_SYNC_VALIDATION=1` the layer's error count
/// is the discriminating check: a missing barrier is a read-after-write hazard.
#[test]
fn a_later_pass_in_a_batch_reads_the_tensor_an_earlier_pass_wrote() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("batched tensor chain: no GPU — skipping");
        return;
    };
    let validation_counts_before = sandbox
        .host_inner()
        .device()
        .inner
        .validation_layer_message_counts();
    let write_index_pattern = register_slang_kernel(&sandbox, WRITE_INDEX_PATTERN_SLANG);
    let double_tensor = register_slang_kernel(&sandbox, DOUBLE_TENSOR_SLANG);

    let (index_pattern_tensor, doubled_tensor) = sandbox
        .escalate(|full| {
            let index_pattern_tensor = sentinel_filled_tensor_storage_buffer(full)?;
            let doubled_tensor = sentinel_filled_tensor_storage_buffer(full)?;
            let kernel = |kernel_id: &str| {
                full.compute_kernel_by_id(kernel_id)
                    .expect("the kernel just registered")
            };
            let group_count_x = (TENSOR_ELEMENT_COUNT / 64) as u32;
            let recording = [
                BatchedComputeKernelDispatch {
                    kernel: kernel(&write_index_pattern),
                    bindings: vec![BatchedComputeKernelDispatchBinding {
                        binding: 0,
                        surface_bound_resource: SurfaceBoundKernelBindingResource::StorageBuffer(
                            index_pattern_tensor.clone(),
                        ),
                    }],
                    push_constants: Vec::new(),
                    group_count_x,
                    group_count_y: 1,
                    group_count_z: 1,
                },
                BatchedComputeKernelDispatch {
                    kernel: kernel(&double_tensor),
                    bindings: vec![
                        BatchedComputeKernelDispatchBinding {
                            binding: 0,
                            surface_bound_resource:
                                SurfaceBoundKernelBindingResource::StorageBuffer(
                                    index_pattern_tensor.clone(),
                                ),
                        },
                        BatchedComputeKernelDispatchBinding {
                            binding: 1,
                            surface_bound_resource:
                                SurfaceBoundKernelBindingResource::StorageBuffer(
                                    doubled_tensor.clone(),
                                ),
                        },
                    ],
                    push_constants: Vec::new(),
                    group_count_x,
                    group_count_y: 1,
                    group_count_z: 1,
                },
            ];
            full.dispatch_compute_kernel_batch(&recording)?;
            Ok((index_pattern_tensor, doubled_tensor))
        })
        .expect("the two-pass tensor chain runs");

    assert_every_element_is(
        tensor_values_mut(&doubled_tensor),
        |index| index as f32 * 2.0,
        "the doubled tensor — pass 2 must have read pass 1's writes, not the sentinel",
    );
    assert_every_element_is(
        tensor_values_mut(&index_pattern_tensor),
        |index| index as f32,
        "the index-pattern tensor — pass 1's own output",
    );
    if let Some(validation_counts_before) = validation_counts_before {
        let validation_counts_after = sandbox
            .host_inner()
            .device()
            .inner
            .validation_layer_message_counts()
            .expect("the messenger stays installed for the whole test");
        assert_eq!(
            validation_counts_after.error_count, validation_counts_before.error_count,
            "the two-pass tensor chain raised a validation error — under synchronization \
             validation, a missing buffer barrier between the passes"
        );
    }
}

/// Register a HOST_VISIBLE tensor in the parent-wide map under `surface_id`, as
/// the escalate acquire registers a helper's — mappable, so the test reads the
/// dispatch's writes back without a copy.
fn registered_sentinel_filled_tensor(
    sandbox: &GpuContextLimitedAccess,
    surface_id: &str,
) -> crate::core::rhi::StorageBuffer {
    sandbox
        .escalate(|full| {
            let tensor = sentinel_filled_tensor_storage_buffer(full)?;
            full.host_inner()
                .register_storage_buffer_in_the_parent_wide_map(surface_id, tensor.clone());
            Ok(tensor)
        })
        .expect("a registered tensor")
}

fn write_index_pattern_into(
    sandbox: &GpuContextLimitedAccess,
    kernel_id: &str,
    surface_id: &str,
) -> EscalateResponse {
    handle_run_compute_kernel(
        sandbox,
        "run-tensor".to_string(),
        EscalateRequestRunComputeKernel {
            bindings: vec![EscalateRequestRunComputeKernelBinding {
                kind: EscalateComputeBindingKind::StorageBuffer,
                name: "index_pattern_tensor".to_string(),
                target_id: surface_id.to_string(),
            }],
            group_count_x: (TENSOR_ELEMENT_COUNT / 64) as u32,
            group_count_y: 1,
            group_count_z: 1,
            kernel_id: kernel_id.to_string(),
            push_constants_hex: String::new(),
            request_id: "run-tensor".to_string(),
        },
    )
}

/// A dispatch names a tensor storage buffer by its surface id, the parent-wide
/// map resolves it, and the kernel's writes land in that buffer.
#[test]
fn a_dispatch_writes_the_tensor_storage_buffer_its_surface_id_names() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("tensor by id: no GPU — skipping");
        return;
    };
    let kernel_id = register_slang_kernel(&sandbox, WRITE_INDEX_PATTERN_SLANG);
    let surface_id = format!("tensor-by-id-{}", uuid::Uuid::new_v4().simple());
    let tensor = registered_sentinel_filled_tensor(&sandbox, &surface_id);
    assert_every_element_is(
        tensor_values_mut(&tensor),
        |_| TENSOR_UNWRITTEN_SENTINEL,
        "the tensor before any dispatch",
    );

    let response = write_index_pattern_into(&sandbox, &kernel_id, &surface_id);
    assert!(
        matches!(response, EscalateResponse::Ok(_)),
        "a dispatch binding a tensor by id failed: {response:?}"
    );
    assert_every_element_is(
        tensor_values_mut(&tensor),
        |index| index as f32,
        "the tensor the dispatch named by id",
    );
    sandbox
        .host_inner()
        .unregister_storage_buffer_from_the_parent_wide_map(&surface_id);
}

/// A published tensor id whose slot has been recycled is refused by name, and
/// nothing is written into the slot's current tensor.
#[test]
fn a_retired_tensor_frame_id_is_refused_and_writes_nothing() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("retired tensor id: no GPU — skipping");
        return;
    };
    let kernel_id = register_slang_kernel(&sandbox, WRITE_INDEX_PATTERN_SLANG);
    let pool_slot = format!("tensor-slot-{}", uuid::Uuid::new_v4().simple());
    let tensor = registered_sentinel_filled_tensor(&sandbox, &pool_slot);
    sandbox
        .host_inner()
        .lease_aware_pool_minted_frame_generations()
        .held_for_a_hand_off()
        .retire_every_published_frame_of_slot(&pool_slot, 7);

    let message = refusal_message(write_index_pattern_into(
        &sandbox,
        &kernel_id,
        &format!("{pool_slot}#3"),
    ));
    assert!(
        message.contains("`index_pattern_tensor`") && message.contains("generation 3"),
        "must name the binding and the retired generation, got: {message}"
    );
    assert_every_element_is(
        tensor_values_mut(&tensor),
        |_| TENSOR_UNWRITTEN_SENTINEL,
        "the slot's current tensor after a refused dispatch",
    );

    let response = write_index_pattern_into(&sandbox, &kernel_id, &format!("{pool_slot}#7"));
    assert!(
        matches!(response, EscalateResponse::Ok(_)),
        "the slot's current generation binds: {response:?}"
    );
    sandbox
        .host_inner()
        .unregister_storage_buffer_from_the_parent_wide_map(&pool_slot);
    sandbox
        .host_inner()
        .lease_aware_pool_minted_frame_generations()
        .forget_slot(&pool_slot);
}

/// An id with no tensor behind it refuses naming the binding, rather than
/// binding whatever the descriptor last held.
#[test]
fn a_surface_id_with_no_tensor_behind_it_is_refused_naming_the_binding() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("unknown tensor id: no GPU — skipping");
        return;
    };
    let kernel_id = register_slang_kernel(&sandbox, WRITE_INDEX_PATTERN_SLANG);
    let message = refusal_message(write_index_pattern_into(
        &sandbox,
        &kernel_id,
        "no-tensor-here",
    ));
    assert!(
        message.contains("`index_pattern_tensor` names surface \"no-tensor-here\"")
            && message.contains("cannot resolve to a tensor storage buffer"),
        "must name the binding and the id, got: {message}"
    );
}
