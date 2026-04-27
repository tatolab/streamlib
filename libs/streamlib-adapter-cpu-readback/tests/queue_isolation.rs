// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_cpu_readback::tests::queue_isolation` — exercises
//! the per-submit `vk::Fence` path under concurrent queue activity.
//!
//! Background: the cpu-readback adapter previously used
//! `vkQueueWaitIdle` to observe completion of its image↔buffer copies.
//! `vkQueueWaitIdle` is queue-wide — its wait set covers every prior
//! submission on the queue at the time of the call. Issue #532 replaced
//! it with a per-submit `vk::Fence` so the wait is targeted to the
//! cpu-readback's own copy and composes correctly with concurrent
//! activity from other workloads sharing the queue.
//!
//! What this test asserts: with a worker thread continuously submitting
//! unrelated command buffers to the same queue, repeated cpu-readback
//! acquire/release cycles all complete correctly within a sane bound,
//! and the round-tripped pixel data still matches the host-written
//! pattern. That demonstrates the fence-based path is robust under
//! contention and free of `vkQueueWaitIdle`-style queue-wide drain.
//!
//! What this test deliberately does NOT assert: a literal "acquire
//! returns before a prior unrelated submit finishes" timing claim.
//! Vulkan submission ordering forces our submit to wait for any prior
//! submit on the same queue, regardless of whether the wait primitive
//! is `vkQueueWaitIdle` or `vk::Fence`. A strictly-isolated timing
//! assertion would require multi-queue parallelism (separate
//! graphics/compute queues), which is out of scope here. See issue
//! #532's `Tests / validation` section for the full caveat.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use streamlib::adapter_support::VulkanDevice;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::common::HostFixture;

/// Submit a single empty (begin/end only) command buffer to `queue`
/// with a per-call fence and wait for it. Used by the worker thread to
/// keep the queue busy with unrelated submits while the main thread is
/// running cpu-readback acquires.
fn submit_noop_and_wait(device: &VulkanDevice) -> Result<(), String> {
    let raw = device.device();
    let queue = device.queue();
    let qf = device.queue_family_index();

    let pool_info = vk::CommandPoolCreateInfo::builder()
        .queue_family_index(qf)
        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
        .build();
    let pool = unsafe { raw.create_command_pool(&pool_info, None) }
        .map_err(|e| format!("create_command_pool: {e}"))?;

    let alloc_info = vk::CommandBufferAllocateInfo::builder()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1)
        .build();
    let cmd = match unsafe { raw.allocate_command_buffers(&alloc_info) } {
        Ok(v) => v[0],
        Err(e) => {
            unsafe { raw.destroy_command_pool(pool, None) };
            return Err(format!("allocate_command_buffers: {e}"));
        }
    };

    let begin_info = vk::CommandBufferBeginInfo::builder()
        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
        .build();
    unsafe { raw.begin_command_buffer(cmd, &begin_info) }
        .map_err(|e| format!("begin_command_buffer: {e}"))?;
    unsafe { raw.end_command_buffer(cmd) }
        .map_err(|e| format!("end_command_buffer: {e}"))?;

    let fence_info = vk::FenceCreateInfo::builder().build();
    let fence = unsafe { raw.create_fence(&fence_info, None) }
        .map_err(|e| format!("create_fence: {e}"))?;

    let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
        .command_buffer(cmd)
        .build()];
    let submit = vk::SubmitInfo2::builder()
        .command_buffer_infos(&cmd_infos)
        .build();
    let submit_result =
        unsafe { device.submit_to_queue(queue, &[submit], fence) }.map_err(|e| format!("{e}"));

    let wait_result = match submit_result {
        Ok(_) => unsafe { raw.wait_for_fences(&[fence], true, 5_000_000_000) }
            .map(|_| ())
            .map_err(|e| format!("wait_for_fences: {e}")),
        Err(e) => Err(e),
    };

    unsafe {
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
    }

    wait_result
}

#[test]
fn acquire_release_cycles_robust_under_concurrent_queue_submits() {
    let Some(fixture) = HostFixture::try_new() else {
        // No Vulkan device available — skip silently, matching the
        // behavior of every other test in this crate.
        return;
    };

    let descriptor = fixture.register_surface(1, 64, 32);

    // Worker thread: keep the queue busy with unrelated noop submits
    // until the main thread tells it to stop. Each iteration goes
    // through `VulkanDevice::submit_to_queue`, the same per-queue-
    // mutex-protected entrypoint the cpu-readback adapter uses, so we
    // exercise the same submission path under contention.
    let stop = Arc::new(AtomicBool::new(false));
    let worker_device = Arc::clone(fixture.adapter.device());
    let worker_stop = Arc::clone(&stop);
    let worker_iters = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let worker_iters_clone = Arc::clone(&worker_iters);
    let worker = std::thread::spawn(move || -> Result<(), String> {
        while !worker_stop.load(Ordering::Relaxed) {
            submit_noop_and_wait(&worker_device)?;
            worker_iters_clone.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    });

    // Main thread: run a meaningful number of acquire/release cycles
    // and assert each one round-trips the pixel pattern correctly.
    // Each iteration writes a per-iteration distinct pattern so a
    // stale-data bug in the fence-wait path would surface as a
    // mismatch.
    let cycles = 24;
    let started = Instant::now();
    for i in 0..cycles {
        let pattern: [u8; 4] = [i as u8, (i + 1) as u8, (i + 2) as u8, (i + 3) as u8];
        {
            let mut wg = fixture
                .ctx
                .acquire_write(&descriptor)
                .expect("acquire_write under contention");
            let bytes = wg.view_mut().plane_mut(0).bytes_mut();
            for chunk in bytes.chunks_exact_mut(4) {
                chunk.copy_from_slice(&pattern);
            }
        }
        let rg = fixture
            .ctx
            .acquire_read(&descriptor)
            .expect("acquire_read under contention");
        let view = rg.view();
        let bytes = view.plane(0).bytes();
        for chunk in bytes.chunks_exact(4) {
            assert_eq!(
                chunk, &pattern,
                "iteration {i}: round-tripped bytes do not match host pattern"
            );
        }
    }
    let elapsed = started.elapsed();

    stop.store(true, Ordering::Relaxed);
    worker
        .join()
        .expect("worker thread panicked")
        .expect("worker submit_noop_and_wait failed");

    // Sanity bound: 24 acquire/write + acquire/read cycles plus the
    // worker's noop submits should comfortably finish in well under a
    // minute on any reasonable setup. The bound exists to flag a
    // pathological queue-wide stall regression — if the fence wait
    // accidentally degrades back to `vkQueueWaitIdle`-style behavior
    // and the worker hammers the queue hard enough, this could blow
    // the bound. The assertion is intentionally loose to stay stable
    // across hardware.
    assert!(
        elapsed < Duration::from_secs(60),
        "{cycles} cpu-readback round-trip cycles took {elapsed:?} under \
         concurrent queue activity — suspect queue-wide drain regression"
    );

    // Confirm the worker actually got onto the queue (otherwise this
    // is just a single-threaded test with no contention). One iter is
    // the floor; in practice the worker lands many.
    let iters = worker_iters.load(Ordering::Relaxed);
    assert!(
        iters >= 1,
        "worker thread didn't land any noop submits — test isn't actually exercising contention"
    );
}
