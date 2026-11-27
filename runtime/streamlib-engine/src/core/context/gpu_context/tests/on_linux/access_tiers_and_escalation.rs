// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs a GPU device"
)]
#[test]
fn an_owned_color_converter_shares_no_kernel_with_the_cached_one() {
    let gpu = match GpuContext::init_for_platform() {
        Ok(gpu) => gpu,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };
    let cached_once = gpu
        .color_converter(PixelFormat::Rgba32, PixelFormat::Yuyv422)
        .expect("cached");
    let cached_twice = gpu
        .color_converter(PixelFormat::Rgba32, PixelFormat::Yuyv422)
        .expect("cached again");
    let owned_first = gpu
        .create_color_converter(PixelFormat::Rgba32, PixelFormat::Yuyv422)
        .expect("owned");
    let owned_second = gpu
        .create_color_converter(PixelFormat::Rgba32, PixelFormat::Yuyv422)
        .expect("owned again");

    assert!(
        std::ptr::eq(cached_once.host_inner(), cached_twice.host_inner()),
        "the cache hands out one converter per format pair"
    );
    assert!(
        !std::ptr::eq(owned_first.host_inner(), cached_once.host_inner()),
        "an owned converter is not the cached one"
    );
    assert!(
        !std::ptr::eq(owned_first.host_inner(), owned_second.host_inner()),
        "two owners get two converters"
    );
}

/// Kernel drop past `escalate_end` (#1006 scenario 6).
///
/// A kernel constructed inside `escalate(|full| ...)` and returned
/// out of the closure must Drop cleanly after the scope ends. The
/// kernel handle's Drop is independent of any active escalate scope
/// (the scope token only validates FullAccess CALL dispatch; drop is
/// a refcount decrement on an opaque handle).
///
/// Mental revert: wiring the drop to require a live escalate
/// scope would crash here because the scope is closed before the
/// drop runs.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — kernel construction needs GPU"
)]
#[test]
fn test_kernel_drops_cleanly_after_escalate_end() {
    use crate::core::rhi::{ComputeBindingSpec, ComputeKernelDescriptor};

    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };

    let limited = GpuContextLimitedAccess::new(gpu);

    // Trivial compute SPIR-V (just an entry point — the test
    // doesn't dispatch; it only verifies kernel construction +
    // drop semantics across the escalate boundary). If the
    // workspace's existing test fixtures already include a tiny
    // valid SPIR-V, prefer that; otherwise this test is gated by
    // the hardware feature flag and the engine's own
    // `create_compute_kernel` validates SPIR-V at construction.
    let trivial_spv: &[u8] = include_bytes!(concat!(
        env!("OUT_DIR"),
        "/color_convert_nv12_buffer_to_rgba.spv"
    ));
    let bindings: &[ComputeBindingSpec] = &[
        ComputeBindingSpec::storage_buffer(0),
        ComputeBindingSpec::storage_image(1),
    ];

    let kernel_arc = limited
        .escalate(|full| {
            full.create_compute_kernel(&ComputeKernelDescriptor {
                entry_point: "main",
                label: "drop_post_escalate_smoke",
                spv: trivial_spv,
                bindings,
                push_constant_size: 96,
                specialization_constants: &[],
            })
        })
        .expect("escalate must succeed");

    drop(kernel_arc);

    // A subsequent escalate must succeed too — proves the drop
    // didn't leave any locks held.
    let after: Result<u32> = limited.escalate(|_full| Ok(13));
    assert_eq!(after.expect("escalate after kernel drop must succeed"), 13,);

    println!("kernel drops cleanly after escalate_end: OK");
}

/// `GpuContextLimitedAccess::acquire_storage_buffer` reaches the
/// shared inner context, allocates a HOST_VISIBLE storage buffer
/// with the requested byte size, and hands back a `StorageBuffer`
/// with a non-null mapped pointer. This exercises Sandbox-side
/// reachability — the path subprocess Vulkan code rides after the
/// camera carve-out (#673) lands. Returning `StorageBuffer` (not
/// `PixelBuffer`) means consumers never see synthetic pixel
/// dimensions on SSBOs.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn acquire_storage_buffer_via_limited_access() {
    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };

    let limited = GpuContextLimitedAccess::new(gpu.clone());
    let byte_size: u64 = 1024 * 64;

    let buffer = limited
        .acquire_storage_buffer(
            &crate::core::rhi::TensorStorageBufferLayout::of_bytes(byte_size)
                .expect("a non-zero size"),
        )
        .expect("Sandbox-side acquire_storage_buffer should succeed");

    // Public StorageBuffer surface: byte_size, mapped_ptr only —
    // no width/height/format getters to confuse SSBO consumers.
    assert_eq!(buffer.byte_size(), byte_size);
    assert!(
        !buffer.mapped_ptr().is_null(),
        "Sandbox-acquired SSBO must expose a non-null mapped pointer"
    );

    // FullAccess mirror also reaches the same inner context.
    let full = limited.to_full_access();
    let buffer2 = full
        .acquire_storage_buffer(
            &crate::core::rhi::TensorStorageBufferLayout::of_bytes(byte_size)
                .expect("a non-zero size"),
        )
        .expect("FullAccess mirror should succeed");
    assert_eq!(buffer2.byte_size(), byte_size);

    println!(
        "GpuContextLimitedAccess::acquire_storage_buffer: {} bytes; FullAccess mirror also OK",
        byte_size
    );
}

/// A kernel created in a node's per-frame function is refused before any
/// GPU work, naming the call and showing it wrapped in a computed value; the
/// same creation in the node's body and in a build builds the kernel.
///
/// Mental revert: drop the frame check from `create_compute_kernel`, and
/// the frame builds a kernel every frame.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — kernel construction needs GPU"
)]
#[test]
fn a_kernel_created_in_a_frame_is_refused_and_one_created_in_the_body_or_a_build_is_built() {
    use crate::core::rhi::{ComputeBindingSpec, ComputeKernelDescriptor};

    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };
    let limited = GpuContextLimitedAccess::new(gpu);
    let create_a_kernel = |limited: &GpuContextLimitedAccess| {
        limited.escalate(|full| {
            full.create_compute_kernel(&ComputeKernelDescriptor {
                entry_point: "main",
                label: "kernel_created_in_a_frame",
                spv: include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/color_convert_nv12_buffer_to_rgba.spv"
                )),
                bindings: &[
                    ComputeBindingSpec::storage_buffer(0),
                    ComputeBindingSpec::storage_image(1),
                ],
                push_constant_size: 96,
                specialization_constants: &[],
            })
        })
    };

    let mut declared = streamlib_scope::ScopeBeingDeclared::new();
    let limited_of_the_build = limited.clone();
    let built_in_a_build = declared.declare(|| {
        create_a_kernel(&limited).expect("a kernel is built in the body");
        streamlib_scope::computed(move || create_a_kernel(&limited_of_the_build).map(|_| true))
    });
    let (scope, mut view) = declared.start().unwrap();
    assert!(scope.wait_until_settled(std::time::Duration::from_secs(30)));

    let refusal = view
        .run_frame(|| {
            assert!(*built_in_a_build.get(), "a kernel is built in a build");
            create_a_kernel(&limited).err()
        })
        .expect("every computed value is built")
        .expect("a kernel created in a frame is refused");

    assert!(matches!(
        &refusal,
        Error::CallRefusedInAFrame(streamlib_scope::CallRefusedInAFrame {
            does: streamlib_scope::CallTooSlowForAFrame::CreatesAKernel,
            call,
        })
            if call == "create_compute_kernel"
    ));
    assert!(
        refusal
            .to_string()
            .contains("`let kernel = computed(move || … create_compute_kernel(…));`"),
        "{refusal}"
    );
}
