// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Writes `2 × input + 1` at each index, so an output names the input it was
/// computed from.
const DOUBLE_PLUS_ONE_SLANG: &str = r#"
[[vk::binding(0, 0)]] RWStructuredBuffer<uint> input;
[[vk::binding(1, 0)]] RWStructuredBuffer<uint> output;
[numthreads(64, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    output[invocation.x] = input[invocation.x] * 2 + 1;
}
"#;

const ELEMENT_COUNT: u32 = 256;

fn double_plus_one_kernel(device: &Arc<HostVulkanDevice>) -> VulkanComputeKernel {
    let spv = crate::core::rhi::SlangShaderSourceCompiler::new()
        .compile_or_reuse(
            DOUBLE_PLUS_ONE_SLANG,
            crate::core::rhi::ShaderCompilationTargetStage::Compute,
            "main",
            "double-plus-one",
        )
        .expect("the shader compiles");
    VulkanComputeKernel::new(
        device,
        &ComputeKernelDescriptor {
            label: "double-plus-one",
            spv: &spv,
            entry_point: "main",
            bindings: &[
                ComputeBindingSpec::storage_buffer(0),
                ComputeBindingSpec::storage_buffer(1),
            ],
            push_constant_size: 0,
            specialization_constants: &[],
        },
    )
    .expect("the kernel builds")
}

/// The input one thread writes on one round, distinct for every thread and
/// round.
fn input_of(thread: u32, round: u32) -> Vec<u32> {
    (0..ELEMENT_COUNT)
        .map(|index| thread * 1_000_000 + round * 1_000 + index)
        .collect()
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn two_threads_dispatching_one_kernel_each_read_their_own_output() {
    const ROUNDS: u32 = 100;
    let device = vulkan_device_for_dispatch_tests();
    let kernel = double_plus_one_kernel(&device);
    std::thread::scope(|scope| {
        for thread in 0..2 {
            let device = &device;
            let kernel = &kernel;
            scope.spawn(move || {
                let input = make_storage_buffer(device, ELEMENT_COUNT);
                let output = make_storage_buffer(device, ELEMENT_COUNT);
                for round in 0..ROUNDS {
                    let values = input_of(thread, round);
                    write_buffer_u32(&input, &values);
                    let mut dispatch = kernel.begin_dispatch();
                    dispatch
                        .set_storage_buffer_pixel(0, &input)
                        .expect("the input binds");
                    dispatch
                        .set_storage_buffer_pixel(1, &output)
                        .expect("the output binds");
                    dispatch
                        .dispatch(ELEMENT_COUNT / 64, 1, 1)
                        .expect("the dispatch runs");
                    let expected: Vec<u32> = values.iter().map(|value| value * 2 + 1).collect();
                    assert_eq!(
                        read_buffer_u32(&output, ELEMENT_COUNT as usize),
                        expected,
                        "thread {thread}, round {round} read another dispatch's output"
                    );
                }
            });
        }
    });
    assert_no_validation_errors(&device);
}

fn assert_no_validation_errors(device: &HostVulkanDevice) {
    if let Some(counts) = device.validation_layer_message_counts() {
        assert_eq!(
            counts.error_count, 0,
            "the dispatches raised validation errors"
        );
    }
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_dispatch_runs_on_another_slot_while_one_slot_is_held() {
    let device = vulkan_device_for_dispatch_tests();
    let kernel = double_plus_one_kernel(&device);
    let held_slot = kernel.host_inner().dispatch_slots[0].lock();
    let (finished, finished_signal) = std::sync::mpsc::channel();
    let dispatching_thread = {
        let device = Arc::clone(&device);
        let kernel = kernel.clone();
        std::thread::spawn(move || {
            let buffer = make_storage_buffer(&device, ELEMENT_COUNT);
            let mut dispatch = kernel.begin_dispatch();
            dispatch
                .set_storage_buffer_pixel(0, &buffer)
                .expect("the input binds");
            dispatch
                .set_storage_buffer_pixel(1, &buffer)
                .expect("the output binds");
            let outcome = dispatch.dispatch(ELEMENT_COUNT / 64, 1, 1);
            let _ = finished.send(outcome);
        })
    };
    let outcome = finished_signal.recv_timeout(std::time::Duration::from_secs(10));
    drop(held_slot);
    outcome
        .expect("a dispatch waited on the held slot instead of taking the free one")
        .expect("the dispatch runs");
    dispatching_thread
        .join()
        .expect("the dispatching thread ends");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_thousand_dispatches_in_a_row_each_complete() {
    let device = vulkan_device_for_dispatch_tests();
    let kernel = double_plus_one_kernel(&device);
    let input = make_storage_buffer(&device, ELEMENT_COUNT);
    let output = make_storage_buffer(&device, ELEMENT_COUNT);
    for round in 0..1_000 {
        let values = input_of(0, round);
        write_buffer_u32(&input, &values);
        kernel
            .set_storage_buffer_pixel(0, &input)
            .expect("the input binds");
        kernel
            .set_storage_buffer_pixel(1, &output)
            .expect("the output binds");
        kernel
            .dispatch(ELEMENT_COUNT / 64, 1, 1)
            .expect("the dispatch runs");
        assert_eq!(
            read_buffer_u32(&output, 1),
            vec![values[0] * 2 + 1],
            "round {round}"
        );
    }
    assert_no_validation_errors(&device);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_dispatch_with_a_binding_left_unset_is_refused_and_leaves_the_kernel_usable() {
    let device = vulkan_device_for_dispatch_tests();
    let kernel = double_plus_one_kernel(&device);
    let buffer = make_storage_buffer(&device, ELEMENT_COUNT);
    let mut incomplete = kernel.begin_dispatch();
    incomplete
        .set_storage_buffer_pixel(0, &buffer)
        .expect("the input binds");
    let refusal = incomplete
        .dispatch(ELEMENT_COUNT / 64, 1, 1)
        .expect_err("binding 1 is unset")
        .to_string();
    assert!(refusal.contains("binding 1"), "{refusal}");

    let mut complete = kernel.begin_dispatch();
    complete
        .set_storage_buffer_pixel(0, &buffer)
        .expect("the input binds");
    complete
        .set_storage_buffer_pixel(1, &buffer)
        .expect("the output binds");
    complete
        .dispatch(ELEMENT_COUNT / 64, 1, 1)
        .expect("the kernel still dispatches after a refusal");
}
