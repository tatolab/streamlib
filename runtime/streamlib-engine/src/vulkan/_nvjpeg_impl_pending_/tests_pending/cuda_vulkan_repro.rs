// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Minimal repro for the drone-racer `vkCreateComputePipelines` SIGSEGV.
//!
//! Hypothesis under test: loading CUDA (via the nvJPEG `Auto` probe)
//! before creating a Vulkan compute pipeline corrupts NVIDIA's Vulkan
//! shader compiler — independent of thread count / busy-process state.
//!
//! `Auto` probes nvJPEG: it loads `libcuda`, `cudaSetDevice(0)` fails
//! with `cudaErrorInsufficientDriver` on this host's CUDA-runtime /
//! driver split, then it falls back to the Vulkan-compute backend
//! (`vkCreateComputePipelines`). That is the EXACT sequence the
//! drone-racer's jpeg processor runs. `gpu_decode.rs` (the clean
//! reference) never loads CUDA — it builds the Vulkan kernel directly.
//!
//! If the SIGSEGV reproduces HERE — a simple single-threaded test
//! process — then the busy host (tokio/zbus/audio/iceoryx2 threads) is
//! NOT required, and the root cause is the CUDA <-> Vulkan-driver
//! interaction, not threading. If this stays clean, the busy process is
//! a necessary ingredient and the hypothesis is wrong.

#![cfg(target_os = "linux")]

use streamlib::sdk::context::GpuContext;
use streamlib::sdk::engine::host_rhi::HostVulkanDevice;
use vulkan_jpeg::{JpegBackendKind, JpegBackendPreference, SimpleJpegDecoder};

fn fresh_gpu_context() -> Option<GpuContext> {
    HostVulkanDevice::new().ok()?;
    GpuContext::init_for_platform().ok()
}

/// THE REPRO. `Auto` loads CUDA, then builds the Vulkan compute kernel.
/// Run this test ALONE (a SIGSEGV aborts the whole test process).
#[test]
fn auto_backend_loads_cuda_then_builds_vulkan_kernel() {
    let Some(gpu) = fresh_gpu_context() else {
        return;
    };
    let result = gpu
        .limited_access()
        .escalate(|full| SimpleJpegDecoder::new(full, 64, 64));
    // Reaching this line at all means NO SIGSEGV occurred.
    match result {
        Ok(d) => tracing::warn!(
            backend = ?d.backend_kind(),
            "REPRO RESULT: Auto SimpleJpegDecoder::new SURVIVED in a simple process \
             (CUDA-load-then-Vulkan-pipeline is NOT sufficient on its own)"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            "REPRO RESULT: Auto SimpleJpegDecoder::new ERRORED (not crashed)"
        ),
    }
}

/// THE THREAD TEST. Device created on the MAIN thread
/// (`fresh_gpu_context`), but the Vulkan kernel built on a SPAWNED worker
/// thread — exactly like the drone-racer's processor-setup threads. The
/// ground-truth fault is a NULL-pointer deref in libnvidia-glcore
/// (`mov 0x28(%rdi),%rbx` with `rdi == 0`), consistent with the compiler
/// reading per-thread driver state that's only initialized on the
/// device-creating thread. No CUDA here (Force(VulkanCompute)) so the
/// thread is the ONLY variable. If "build on a non-device-creating
/// thread" is the trigger, THIS crashes; if it's clean, the worker thread
/// alone is not the cause.
#[test]
fn vulkan_kernel_built_on_spawned_thread() {
    let Some(gpu) = fresh_gpu_context() else {
        return;
    };
    let handle = std::thread::spawn(move || {
        gpu.limited_access().escalate(|full| {
            SimpleJpegDecoder::new_with_preference(
                full,
                64,
                64,
                JpegBackendPreference::Force(JpegBackendKind::VulkanCompute),
            )
        })
    });
    let result = handle.join().expect("spawned thread panicked");
    assert!(
        result.is_ok(),
        "Vulkan kernel on a spawned (non-device-creating) thread: {result:?}"
    );
}

/// Control: `Force(VulkanCompute)` never loads CUDA — must always be
/// clean. Establishes that the Vulkan kernel build itself is fine.
#[test]
fn force_vulkan_no_cuda_is_clean() {
    let Some(gpu) = fresh_gpu_context() else {
        return;
    };
    let result = gpu.limited_access().escalate(|full| {
        SimpleJpegDecoder::new_with_preference(
            full,
            64,
            64,
            JpegBackendPreference::Force(JpegBackendKind::VulkanCompute),
        )
    });
    assert!(
        result.is_ok(),
        "Force(VulkanCompute) must construct cleanly (no CUDA, no crash): {result:?}"
    );
}
