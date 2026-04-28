// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Carve-out semantic tests for `ConsumerVulkanDevice`.
//!
//! Phase 1 (#561) only landed thin "constructs + exposes queue" tests.
//! Phase 2 (#560) lands the cdylib swap to `ConsumerVulkanDevice`, so
//! the consumer device starts running in production. These tests
//! cover the carve-out paths that aren't already covered by the
//! adapter-vulkan host↔subprocess round-trip integration tests
//! (`streamlib-adapter-vulkan/tests/round_trip_*` — those exercise
//! the full DMA-BUF import + bind + map flow against a real host
//! allocation):
//!
//! - **Concurrent submit serialization** — two threads issuing
//!   `submit_to_queue` against the same `VkQueue`. Vulkan requires
//!   external synchronization for `vkQueueSubmit2` from multiple
//!   threads; the consumer device's per-queue mutex is the canonical
//!   serializer. Without it the driver races (UB) or returns
//!   `VK_ERROR_VALIDATION_FAILED_EXT` under VVL.
//! - **Leak-tracing on drop** — `Drop` emits `tracing::warn!` when
//!   dropped with live imports. We assert the silent path (no leak)
//!   here; the warning path is exercised by adapter integration tests
//!   that intentionally crash mid-import.
//!
//! All tests skip gracefully when no GPU / no Vulkan loader is
//! available, matching `consumer_vulkan_device.rs::tests`.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use streamlib_consumer_rhi::{ConsumerVulkanDevice, VulkanRhiDevice};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

fn try_consumer() -> Option<Arc<ConsumerVulkanDevice>> {
    match ConsumerVulkanDevice::new() {
        Ok(d) => Some(Arc::new(d)),
        Err(e) => {
            println!("skip: ConsumerVulkanDevice::new failed: {e}");
            None
        }
    }
}

#[test]
fn consumer_device_concurrent_submit_serializes() {
    let consumer = match try_consumer() {
        Some(d) => d,
        None => return,
    };
    let device = consumer.device();
    let queue = consumer.queue();

    // Empty SubmitInfo2 is a valid no-op submit (zero command buffers,
    // zero waits, zero signals). The serialization being exercised is
    // the per-queue mutex, not the driver's internal synchronization.
    let consumer_a = Arc::clone(&consumer);
    let t1 = std::thread::spawn(move || -> Result<(), String> {
        for _ in 0..32 {
            let info = vk::SubmitInfo2::builder().build();
            let submits = [info];
            unsafe {
                <ConsumerVulkanDevice as VulkanRhiDevice>::submit_to_queue(
                    &consumer_a,
                    queue,
                    &submits,
                    vk::Fence::null(),
                )
                .map_err(|e| format!("thread-a submit: {e}"))?;
            }
        }
        Ok(())
    });
    let consumer_b = Arc::clone(&consumer);
    let t2 = std::thread::spawn(move || -> Result<(), String> {
        for _ in 0..32 {
            let info = vk::SubmitInfo2::builder().build();
            let submits = [info];
            unsafe {
                <ConsumerVulkanDevice as VulkanRhiDevice>::submit_to_queue(
                    &consumer_b,
                    queue,
                    &submits,
                    vk::Fence::null(),
                )
                .map_err(|e| format!("thread-b submit: {e}"))?;
            }
        }
        Ok(())
    });

    t1.join().expect("thread-a panic").expect("thread-a submit ok");
    t2.join().expect("thread-b panic").expect("thread-b submit ok");

    unsafe {
        let _ = device.queue_wait_idle(queue);
    }
}

#[test]
fn consumer_device_drops_silently_when_no_imports() {
    let consumer = match try_consumer() {
        Some(d) => d,
        None => return,
    };
    assert_eq!(
        consumer.live_import_allocation_count(),
        0,
        "fresh ConsumerVulkanDevice has zero live imports"
    );
    drop(consumer);
}

#[test]
fn consumer_device_implements_vulkan_rhi_device_trait() {
    let consumer = match try_consumer() {
        Some(d) => d,
        None => return,
    };
    fn assert_consumer<D: VulkanRhiDevice>(_: &D) {}
    assert_consumer(&*consumer);

    // The trait surface adapter-vulkan + raw_handles depend on:
    let _instance: &vulkanalia::Instance = consumer.instance();
    let _physical: vk::PhysicalDevice = consumer.physical_device();
    let _device: &vulkanalia::Device = consumer.device();
    let _queue: vk::Queue = consumer.queue();
    let _qfi: u32 = consumer.queue_family_index();
}
