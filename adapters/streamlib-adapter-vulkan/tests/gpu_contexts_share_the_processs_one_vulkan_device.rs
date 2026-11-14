// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! GPU contexts created from many threads of one process at once share the
//! process's one Vulkan device.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::sync::{Arc, Barrier};

use streamlib::sdk::context::GpuContext;
use streamlib::sdk::engine::HostGpuDeviceExt;

#[test]
fn gpu_contexts_created_from_many_threads_at_once_share_the_processs_one_vulkan_device() {
    const GPU_CONTEXT_CREATING_THREAD_COUNT: usize = 8;
    let all_threads_ready_to_create = Arc::new(Barrier::new(GPU_CONTEXT_CREATING_THREAD_COUNT));
    let gpu_context_creating_threads: Vec<_> = (0..GPU_CONTEXT_CREATING_THREAD_COUNT)
        .map(|_| {
            let all_threads_ready_to_create = Arc::clone(&all_threads_ready_to_create);
            std::thread::spawn(move || {
                all_threads_ready_to_create.wait();
                GpuContext::init_for_platform_sync()
                    .expect("every concurrent GpuContext::init_for_platform_sync() succeeds")
            })
        })
        .collect();
    let gpu_contexts: Vec<GpuContext> = gpu_context_creating_threads
        .into_iter()
        .map(|thread| {
            thread
                .join()
                .expect("a GPU-context-creating thread panicked")
        })
        .collect();

    let first_vulkan_device = gpu_contexts[0].device().vulkan_device();
    for gpu_context in &gpu_contexts[1..] {
        assert!(Arc::ptr_eq(
            first_vulkan_device,
            gpu_context.device().vulkan_device()
        ));
    }

    let gpu_context_created_after_the_others =
        GpuContext::init_for_platform_sync().expect("a later GpuContext succeeds");
    assert!(Arc::ptr_eq(
        first_vulkan_device,
        gpu_context_created_after_the_others
            .device()
            .vulkan_device()
    ));
}
