// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Fix validation for the NVIDIA swapchain + DMA-BUF allocation OOM bug.
//!
//! These tests take a window from the process's one window event pump, build
//! a swapchain on it and present on it, then prove the allocation patterns the
//! engine ships hold up behind it. Tests skip gracefully if no display server
//! is available.

#![cfg(all(test, target_os = "linux"))]

use std::sync::Arc;

use vma::Alloc as _;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use super::{HostVulkanDevice, VulkanPresentTarget};
use crate::core::window_event_pump::{
    WindowRegistrationRequestFromOwningProcessor, process_wide_window_event_pump,
};

const DMA_BUF_EXPORT_HANDLE_TYPES: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;

// ─────────────────────────────────────────────────────────────────────────────
// Custom VMA allocator builders for testing different configurations
// ─────────────────────────────────────────────────────────────────────────────

/// Build a VMA allocator with NO global export config. Pools handle export.
fn build_vma_without_global_export(device: &HostVulkanDevice) -> vma::Allocator {
    let instance = device.instance();
    let vk_device = device.device();
    let physical_device = device.physical_device();

    let mut alloc_options = vma::AllocatorOptions::new(instance, vk_device, physical_device);
    alloc_options.version = vulkanalia::Version::new(1, 4, 0);

    unsafe { vma::Allocator::new(&alloc_options) }
        .expect("VMA allocator (clean config) creation failed")
}

// ─────────────────────────────────────────────────────────────────────────────
// Allocation helpers — simulate camera-display pipeline allocations
// ─────────────────────────────────────────────────────────────────────────────

/// The one exportable-buffer shape this fixture allocates. A pool probe and
/// the real allocation must be built from it alike: a DMA-BUF external buffer
/// has a narrower `memoryTypeBits` than a plain one, so a probe that differs
/// picks a memory type the real buffer rejects at bind time
/// (VUID-vkBindBufferMemory-memory-01035). Mirrors the probe in
/// `HostVulkanDevice::create_dma_buf_pools`.
fn dma_buf_buffer_create_info<'a>(
    size: vk::DeviceSize,
    external_buffer_info: &'a mut vk::ExternalMemoryBufferCreateInfo,
) -> vk::BufferCreateInfoBuilder<'a> {
    vk::BufferCreateInfo::builder()
        .size(size)
        .usage(
            vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::STORAGE_BUFFER,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .push_next(external_buffer_info)
}

/// The allocation options every exportable buffer here is allocated with.
fn dma_buf_buffer_allocation_options() -> vma::AllocationOptions {
    vma::AllocationOptions {
        flags: vma::AllocationCreateFlags::DEDICATED_MEMORY
            | vma::AllocationCreateFlags::MAPPED
            | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
        required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
            | vk::MemoryPropertyFlags::HOST_COHERENT,
        ..Default::default()
    }
}

/// The one exportable-image shape this fixture allocates — same probe/real
/// contract as [`dma_buf_buffer_create_info`], via
/// `VUID-vkBindImageMemory-memory-01047`. The usage set is this fixture's own
/// and is narrower than the one `HostVulkanDevice::create_dma_buf_pools`
/// probes with, which carries `TRANSFER_SRC` as well.
fn dma_buf_image_create_info<'a>(
    width: u32,
    height: u32,
    external_image_info: &'a mut vk::ExternalMemoryImageCreateInfo,
) -> vk::ImageCreateInfoBuilder<'a> {
    vk::ImageCreateInfo::builder()
        .image_type(vk::ImageType::_2D)
        .format(vk::Format::B8G8R8A8_UNORM)
        .extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(external_image_info)
}

/// The allocation options every exportable image here is allocated with.
fn dma_buf_image_allocation_options() -> vma::AllocationOptions {
    vma::AllocationOptions {
        flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
        required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
        ..Default::default()
    }
}

fn alloc_internal_image_via_vma(
    allocator: &vma::Allocator,
    width: u32,
    height: u32,
) -> Result<(vk::Image, vma::Allocation), vk::ErrorCode> {
    let image_info = vk::ImageCreateInfo::builder()
        .image_type(vk::ImageType::_2D)
        .format(vk::Format::B8G8R8A8_UNORM)
        .extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .build();

    let alloc_opts = vma::AllocationOptions {
        required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
        ..Default::default()
    };

    unsafe { allocator.create_image(image_info, &alloc_opts) }
}

// ─────────────────────────────────────────────────────────────────────────────
// Swapchain test harness — creates window + surface + swapchain, runs callback
// ─────────────────────────────────────────────────────────────────────────────

/// Test outcome bundle — includes detailed counters so tests can assert.
#[derive(Debug, Default, Clone)]
pub struct AllocationOutcome {
    pub buffers_attempted: usize,
    pub buffers_succeeded: usize,
    pub images_attempted: usize,
    pub images_succeeded: usize,
    pub failure_messages: Vec<String>,
    pub setup_skipped: Option<String>,
}

impl AllocationOutcome {
    fn buffers_failed(&self) -> usize {
        self.buffers_attempted - self.buffers_succeeded
    }
    fn images_failed(&self) -> usize {
        self.images_attempted - self.images_succeeded
    }
}

/// What to test inside the swapchain context.
enum TestScenario {
    /// Use clean VMA config + allocate plain internal images.
    CleanConfigInternalAllocs,
    /// Use clean VMA config + use export pools for exportable allocs.
    CleanConfigExportPools,
}

fn run_clean_internal_scenario(
    device: &HostVulkanDevice,
    width: u32,
    height: u32,
    outcome: &mut AllocationOutcome,
) {
    let allocator = build_vma_without_global_export(device);

    // 4 internal images via plain VMA (no export, no dedicated)
    for i in 0..4 {
        outcome.images_attempted += 1;
        match alloc_internal_image_via_vma(&allocator, width, height) {
            Ok((img, alloc)) => {
                outcome.images_succeeded += 1;
                unsafe { allocator.destroy_image(img, alloc) };
            }
            Err(e) => {
                outcome.failure_messages.push(format!("image[{i}]: {e}"));
            }
        }
    }
}

fn run_clean_pool_scenario(
    device: &HostVulkanDevice,
    width: u32,
    height: u32,
    outcome: &mut AllocationOutcome,
) {
    let allocator = Arc::new(build_vma_without_global_export(device));

    // ── Find memory type for HOST_VISIBLE DMA-BUF exportable buffers ──
    let mut probe_buffer_external_info = vk::ExternalMemoryBufferCreateInfo::builder()
        .handle_types(DMA_BUF_EXPORT_HANDLE_TYPES)
        .build();
    let probe_buffer_info = dma_buf_buffer_create_info(64 * 1024, &mut probe_buffer_external_info);
    let probe_alloc_opts = dma_buf_buffer_allocation_options();
    let buffer_mem_type_idx = match unsafe {
        allocator.find_memory_type_index_for_buffer_info(probe_buffer_info, &probe_alloc_opts)
    } {
        Ok(idx) => idx,
        Err(e) => {
            outcome
                .failure_messages
                .push(format!("find buffer mem type: {e}"));
            return;
        }
    };

    // Build buffer export pool
    let mut export_info_buffer = vk::ExportMemoryAllocateInfo::builder()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .build();
    let mut buffer_pool_options = vma::PoolOptions::default();
    buffer_pool_options = buffer_pool_options.push_next(&mut export_info_buffer);
    buffer_pool_options.memory_type_index = buffer_mem_type_idx;
    let buffer_pool = match allocator.create_pool(&buffer_pool_options) {
        Ok(p) => p,
        Err(e) => {
            outcome
                .failure_messages
                .push(format!("create buffer pool: {e}"));
            return;
        }
    };

    // ── Find memory type for DEVICE_LOCAL DMA-BUF exportable images ──
    let mut probe_image_external_info = vk::ExternalMemoryImageCreateInfo::builder()
        .handle_types(DMA_BUF_EXPORT_HANDLE_TYPES)
        .build();
    let probe_image_info = dma_buf_image_create_info(64, 64, &mut probe_image_external_info);
    let probe_image_alloc_opts = dma_buf_image_allocation_options();
    let image_mem_type_idx = match unsafe {
        allocator.find_memory_type_index_for_image_info(probe_image_info, &probe_image_alloc_opts)
    } {
        Ok(idx) => idx,
        Err(e) => {
            outcome
                .failure_messages
                .push(format!("find image mem type: {e}"));
            drop(buffer_pool);
            return;
        }
    };

    let mut export_info_image = vk::ExportMemoryAllocateInfo::builder()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .build();
    let mut image_pool_options = vma::PoolOptions::default();
    image_pool_options = image_pool_options.push_next(&mut export_info_image);
    image_pool_options.memory_type_index = image_mem_type_idx;
    let image_pool = match allocator.create_pool(&image_pool_options) {
        Ok(p) => p,
        Err(e) => {
            outcome
                .failure_messages
                .push(format!("create image pool: {e}"));
            drop(buffer_pool);
            return;
        }
    };

    // 4 exportable buffers via pool
    let mut buffers = Vec::new();
    for i in 0..4 {
        outcome.buffers_attempted += 1;
        let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(DMA_BUF_EXPORT_HANDLE_TYPES)
            .build();
        let buf_info = dma_buf_buffer_create_info(
            (width as u64) * (height as u64) * 4,
            &mut external_buffer_info,
        );
        let alloc_opts = dma_buf_buffer_allocation_options();
        match unsafe { buffer_pool.create_buffer(buf_info, &alloc_opts) } {
            Ok(pair) => {
                outcome.buffers_succeeded += 1;
                buffers.push(pair);
            }
            Err(e) => outcome
                .failure_messages
                .push(format!("export buffer[{i}]: {e}")),
        }
    }

    // 4 exportable images via pool
    let mut images = Vec::new();
    for i in 0..4 {
        outcome.images_attempted += 1;
        let mut external_image_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(DMA_BUF_EXPORT_HANDLE_TYPES)
            .build();
        let img_info = dma_buf_image_create_info(width, height, &mut external_image_info);
        let alloc_opts = dma_buf_image_allocation_options();
        match unsafe { image_pool.create_image(img_info, &alloc_opts) } {
            Ok(pair) => {
                outcome.images_succeeded += 1;
                images.push(pair);
            }
            Err(e) => outcome
                .failure_messages
                .push(format!("export image[{i}]: {e}")),
        }
    }

    // Also test: internal images via default pool (no export) work fine
    let mut internal_images = Vec::new();
    for i in 0..4 {
        outcome.images_attempted += 1;
        match alloc_internal_image_via_vma(&allocator, width, height) {
            Ok(pair) => {
                outcome.images_succeeded += 1;
                internal_images.push(pair);
            }
            Err(e) => outcome
                .failure_messages
                .push(format!("internal image[{i}] (default pool): {e}")),
        }
    }

    // Cleanup
    unsafe {
        for (img, alloc) in internal_images {
            allocator.destroy_image(img, alloc);
        }
        for (img, alloc) in images {
            allocator.destroy_image(img, alloc);
        }
        for (buf, alloc) in buffers {
            allocator.destroy_buffer(buf, alloc);
        }
    }
    drop(buffer_pool);
    drop(image_pool);
}

// ─────────────────────────────────────────────────────────────────────────────
// Test harness — runs a scenario with a swapchain live and presenting
// ─────────────────────────────────────────────────────────────────────────────

/// Width and height of every buffer and image a scenario allocates.
const SCENARIO_ALLOCATION_WIDTH: u32 = 1920;
const SCENARIO_ALLOCATION_HEIGHT: u32 = 1080;

/// Frames presented before a scenario runs, so the compositor has mapped the
/// window and imported the swapchain images as DMA-BUFs: the bug triggers
/// only after that import.
const FRAMES_PRESENTED_BEFORE_A_SCENARIO_RUNS: usize = 30;

fn try_create_device() -> Option<Arc<HostVulkanDevice>> {
    match HostVulkanDevice::new() {
        Ok(d) => Some(d),
        Err(e) => {
            tracing::warn!("Skipping — Vulkan device unavailable: {e}");
            None
        }
    }
}

/// Run `scenario` on `device` with a swapchain live on a window from the
/// process's one window event pump, which every test in the process shares:
/// winit permits one event loop per process.
fn run_scenario_with_a_swapchain_live(
    device: &Arc<HostVulkanDevice>,
    scenario: TestScenario,
) -> AllocationOutcome {
    let mut outcome = AllocationOutcome::default();
    let window = match process_wide_window_event_pump().and_then(|pump| {
        pump.request_window_for_owning_processor(WindowRegistrationRequestFromOwningProcessor {
            window_title: "streamlib-test".to_string(),
            initial_width_in_logical_pixels: 640,
            initial_height_in_logical_pixels: 360,
        })
    }) {
        Ok(window) => window,
        Err(e) => {
            outcome.setup_skipped = Some(format!("window: {e}"));
            return outcome;
        }
    };
    let (window_width, window_height) = window.physical_size_when_minted();
    let mut present_target = match VulkanPresentTarget::new(
        device,
        window.present_surface_source(),
        window_width,
        window_height,
        true,
        None,
    ) {
        Ok(present_target) => present_target,
        Err(e) => {
            outcome.setup_skipped = Some(format!("swapchain: {e}"));
            return outcome;
        }
    };
    for _ in 0..FRAMES_PRESENTED_BEFORE_A_SCENARIO_RUNS {
        present_target
            .render_frame(|frame| {
                frame.begin_rendering(Some([0.0, 0.0, 0.0, 1.0]))?;
                frame.end_rendering()
            })
            .expect("a frame presents on the swapchain");
    }

    match scenario {
        TestScenario::CleanConfigInternalAllocs => run_clean_internal_scenario(
            device,
            SCENARIO_ALLOCATION_WIDTH,
            SCENARIO_ALLOCATION_HEIGHT,
            &mut outcome,
        ),
        TestScenario::CleanConfigExportPools => run_clean_pool_scenario(
            device,
            SCENARIO_ALLOCATION_WIDTH,
            SCENARIO_ALLOCATION_HEIGHT,
            &mut outcome,
        ),
    }
    drop(present_target);
    drop(window);
    outcome
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

/// Exercises two scenarios, both against a live swapchain:
///   1. FIX (clean VMA): internal allocs via plain VMA → all succeed
///   2. FIX (VMA pools): export allocs via custom pools → all succeed
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
fn test_swapchain_allocation_scenarios() {
    let device = match try_create_device() {
        Some(d) => d,
        None => return,
    };

    // ── Scenario 1: FIX VALIDATION (clean VMA) ─────────────────────────────
    println!("══ Scenario 1: FIX (clean VMA, internal allocs) ══");
    let outcome =
        run_scenario_with_a_swapchain_live(&device, TestScenario::CleanConfigInternalAllocs);

    if let Some(skip_reason) = &outcome.setup_skipped {
        println!("  Skipping scenario 1: {skip_reason}");
    } else {
        println!(
            "  images: {}/{}, failures: {:?}",
            outcome.images_succeeded, outcome.images_attempted, outcome.failure_messages
        );
        assert_eq!(
            outcome.images_failed(),
            0,
            "All internal images should succeed with clean VMA config + swapchain. \
             Failures: {:?}",
            outcome.failure_messages
        );
        println!("  [scenario 1] PASS — clean VMA works for internal allocs");
    }

    // ── Scenario 2: FIX VALIDATION (VMA pools for export) ──────────────────
    if device.supports_external_memory() {
        println!("══ Scenario 2: FIX (clean VMA + export pools) ══");
        let outcome =
            run_scenario_with_a_swapchain_live(&device, TestScenario::CleanConfigExportPools);

        if let Some(skip_reason) = &outcome.setup_skipped {
            println!("  Skipping scenario 2: {skip_reason}");
        } else {
            println!(
                "  buffers: {}/{}, images: {}/{}, failures: {:?}",
                outcome.buffers_succeeded,
                outcome.buffers_attempted,
                outcome.images_succeeded,
                outcome.images_attempted,
                outcome.failure_messages
            );
            assert_eq!(
                outcome.buffers_failed(),
                0,
                "All export buffers should succeed via pool. Failures: {:?}",
                outcome.failure_messages
            );
            assert_eq!(
                outcome.images_failed(),
                0,
                "All images (export + internal) should succeed. Failures: {:?}",
                outcome.failure_messages
            );
            println!("  [scenario 2] PASS — export pools work alongside default pool");
        }
    } else {
        println!("══ Scenario 2: SKIPPED — external memory unsupported ══");
    }
}
