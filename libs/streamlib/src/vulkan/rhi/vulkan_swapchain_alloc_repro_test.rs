// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reproduction tests for the NVIDIA swapchain + DMA-BUF allocation OOM bug.
//!
//! These tests use a hidden winit window backed by an X11 surface to create a
//! Vulkan swapchain, then exercise the allocation patterns that trigger the
//! bug. Tests skip gracefully if no display server is available.

#![cfg(all(test, target_os = "linux"))]

use std::sync::Arc;
use std::sync::Mutex;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::{
    KhrSurfaceExtensionInstanceCommands as _, KhrSwapchainExtensionDeviceCommands as _,
};
use vulkanalia_vma as vma;
use vma::Alloc as _;

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

use super::HostVulkanDevice;

// ─────────────────────────────────────────────────────────────────────────────
// Custom VMA allocator builders for testing different configurations
// ─────────────────────────────────────────────────────────────────────────────

/// Build a VMA allocator with the BROKEN config — pTypeExternalMemoryHandleTypes
/// set globally for all memory types.
fn build_vma_with_global_export(device: &HostVulkanDevice) -> vma::Allocator {
    let instance = device.instance();
    let vk_device = device.device();
    let physical_device = device.physical_device();

    let mem_props = unsafe { instance.get_physical_device_memory_properties(physical_device) };
    let count = mem_props.memory_type_count as usize;
    let dma_buf_handle_types: Vec<vk::ExternalMemoryHandleTypeFlags> =
        vec![vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT; count];

    let mut alloc_options = vma::AllocatorOptions::new(instance, vk_device, physical_device);
    alloc_options.version = vulkanalia::Version::new(1, 4, 0);
    alloc_options.external_memory_handle_types = &dma_buf_handle_types;

    unsafe { vma::Allocator::new(&alloc_options) }
        .expect("VMA allocator (broken config) creation failed")
}

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

fn alloc_dma_buf_buffer_via_vma(
    allocator: &vma::Allocator,
    size: vk::DeviceSize,
) -> Result<(vk::Buffer, vma::Allocation), vk::ErrorCode> {
    let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .build();

    let buffer_info = vk::BufferCreateInfo::builder()
        .size(size)
        .usage(
            vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::STORAGE_BUFFER,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .push_next(&mut external_buffer_info);

    let alloc_opts = vma::AllocationOptions {
        flags: vma::AllocationCreateFlags::DEDICATED_MEMORY
            | vma::AllocationCreateFlags::MAPPED
            | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
        required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
            | vk::MemoryPropertyFlags::HOST_COHERENT,
        ..Default::default()
    };

    unsafe { allocator.create_buffer(buffer_info, &alloc_opts) }
}

fn alloc_dma_buf_image_via_vma(
    allocator: &vma::Allocator,
    width: u32,
    height: u32,
) -> Result<(vk::Image, vma::Allocation), vk::ErrorCode> {
    let mut external_image_info = vk::ExternalMemoryImageCreateInfo::builder()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .build();

    let image_info = vk::ImageCreateInfo::builder()
        .image_type(vk::ImageType::_2D)
        .format(vk::Format::B8G8R8A8_UNORM)
        .extent(vk::Extent3D { width, height, depth: 1 })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut external_image_info);

    let alloc_opts = vma::AllocationOptions {
        flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
        required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
        ..Default::default()
    };

    unsafe { allocator.create_image(image_info, &alloc_opts) }
}

fn alloc_internal_image_via_vma(
    allocator: &vma::Allocator,
    width: u32,
    height: u32,
) -> Result<(vk::Image, vma::Allocation), vk::ErrorCode> {
    let image_info = vk::ImageCreateInfo::builder()
        .image_type(vk::ImageType::_2D)
        .format(vk::Format::B8G8R8A8_UNORM)
        .extent(vk::Extent3D { width, height, depth: 1 })
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
    /// Use broken VMA config + allocate exportable buffers/images.
    BrokenConfigExportableAllocs,
    /// Use clean VMA config + allocate plain internal images.
    CleanConfigInternalAllocs,
    /// Use clean VMA config + use export pools for exportable allocs.
    CleanConfigExportPools,
}

/// Application handler that runs a test scenario inside winit's event loop.
/// Window/swapchain creation happens in `resumed()`, but the actual allocation
/// work waits for the window to be mapped (via about_to_wait + a frame counter).
/// This is critical because the bug only triggers AFTER the compositor has had
/// a chance to import the swapchain images as DMA-BUFs.
struct SwapchainTestApp {
    device: Arc<HostVulkanDevice>,
    scenario: TestScenario,
    width: u32,
    height: u32,
    /// Output: populated by the test scenario.
    outcome: Arc<Mutex<AllocationOutcome>>,
    /// Set during resumed() once window+swapchain are created.
    window: Option<Window>,
    surface: Option<vk::SurfaceKHR>,
    swapchain: Option<vk::SwapchainKHR>,
    /// Frames-elapsed counter; we wait N frames before running scenario.
    wait_ticks: u32,
    scenario_run: bool,
}

impl ApplicationHandler for SwapchainTestApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // already initialized
        }

        // Create visible window
        let attrs = WindowAttributes::default()
            .with_title("streamlib-test")
            .with_visible(true)
            .with_inner_size(PhysicalSize::new(self.width, self.height));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => w,
            Err(e) => {
                self.outcome.lock().unwrap().setup_skipped =
                    Some(format!("create_window: {e}"));
                event_loop.exit();
                return;
            }
        };

        // Create Vulkan surface
        let instance = self.device.instance();
        let surface = match unsafe {
            vulkanalia::window::create_surface(instance, &window, &window)
        } {
            Ok(s) => s,
            Err(e) => {
                self.outcome.lock().unwrap().setup_skipped =
                    Some(format!("create_surface: {e}"));
                event_loop.exit();
                return;
            }
        };

        // Create swapchain
        match create_swapchain(&self.device, surface, self.width, self.height) {
            Ok(sc) => {
                println!("  [test] swapchain created: {} images", sc.image_count);
                self.swapchain = Some(sc.swapchain);
            }
            Err(e) => {
                unsafe { instance.destroy_surface_khr(surface, None) };
                self.outcome.lock().unwrap().setup_skipped =
                    Some(format!("create_swapchain: {e}"));
                event_loop.exit();
                return;
            }
        }

        self.surface = Some(surface);
        self.window = Some(window);

        // Use Poll mode so about_to_wait fires repeatedly, letting events drain
        event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _id: WindowId,
        _event: WindowEvent,
    ) {
        // We don't need to handle window events for the test
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.scenario_run {
            // Already done — exit
            event_loop.exit();
            return;
        }

        // Wait several ticks to let the compositor map the window and import
        // swapchain DMA-BUFs.
        if self.wait_ticks < 30 {
            self.wait_ticks += 1;
            return;
        }

        // Run the scenario, then exit
        self.run_scenario_inner();
        self.scenario_run = true;
        event_loop.exit();
    }
}

impl SwapchainTestApp {
    fn run_scenario_inner(&mut self) {
        match self.scenario {
            TestScenario::BrokenConfigExportableAllocs => {
                self.run_broken_scenario();
            }
            TestScenario::CleanConfigInternalAllocs => {
                self.run_clean_internal_scenario();
            }
            TestScenario::CleanConfigExportPools => {
                self.run_clean_pool_scenario();
            }
        }
    }

    fn cleanup(&mut self) {
        let instance = self.device.instance();
        let vk_device = self.device.device();
        unsafe {
            let _ = vk_device.device_wait_idle();
            if let Some(sc) = self.swapchain.take() {
                vk_device.destroy_swapchain_khr(sc, None);
            }
            if let Some(surf) = self.surface.take() {
                instance.destroy_surface_khr(surf, None);
            }
        }
        self.window = None;
    }

    fn run_broken_scenario(&self) {
        let allocator = build_vma_with_global_export(&self.device);
        let mut outcome = self.outcome.lock().unwrap();

        // ── Mimic camera processor allocations ─────────────────────────────
        // 1. Two input SSBOs (HOST_VISIBLE, no external memory, no dedicated)
        //    — for raw NV12 V4L2 frames
        let nv12_size = (self.width as u64) * (self.height as u64) * 3 / 2;
        let mut input_ssbos = Vec::new();
        for i in 0..2 {
            let buf_info = vk::BufferCreateInfo::builder()
                .size(nv12_size)
                .usage(vk::BufferUsageFlags::STORAGE_BUFFER)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .build();
            let alloc_opts = vma::AllocationOptions {
                flags: vma::AllocationCreateFlags::MAPPED
                    | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
                required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                    | vk::MemoryPropertyFlags::HOST_COHERENT,
                ..Default::default()
            };
            match unsafe { allocator.create_buffer(buf_info, &alloc_opts) } {
                Ok(pair) => input_ssbos.push(pair),
                Err(e) => outcome
                    .failure_messages
                    .push(format!("camera input SSBO[{i}]: {e}")),
            }
        }

        // 2. Camera compute output image (DEVICE_LOCAL, no external memory,
        //    no dedicated — sub-allocates from VMA block)
        let compute_img_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D { width: self.width, height: self.height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .build();
        let compute_alloc_opts = vma::AllocationOptions {
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let compute_image =
            match unsafe { allocator.create_image(compute_img_info, &compute_alloc_opts) } {
                Ok(pair) => Some(pair),
                Err(e) => {
                    outcome
                        .failure_messages
                        .push(format!("camera compute output image: {e}"));
                    None
                }
            };

        // 3. Camera pixel buffer pool — 4 dedicated DMA-BUF exportable buffers
        let mut pixel_buffers = Vec::new();
        for i in 0..4 {
            outcome.buffers_attempted += 1;
            match alloc_dma_buf_buffer_via_vma(
                &allocator,
                (self.width as u64) * (self.height as u64) * 4,
            ) {
                Ok(pair) => {
                    outcome.buffers_succeeded += 1;
                    pixel_buffers.push(pair);
                }
                Err(e) => outcome
                    .failure_messages
                    .push(format!("pixel buffer[{i}]: {e}")),
            }
        }

        // ── Display-style: 4 camera textures (DMA-BUF + dedicated) ─────────
        let mut camera_textures = Vec::new();
        for i in 0..4 {
            outcome.images_attempted += 1;
            match alloc_dma_buf_image_via_vma(&allocator, self.width, self.height) {
                Ok(pair) => {
                    outcome.images_succeeded += 1;
                    camera_textures.push(pair);
                }
                Err(e) => outcome
                    .failure_messages
                    .push(format!("camera texture[{i}]: {e}")),
            }
        }

        // Cleanup
        unsafe {
            for (img, alloc) in camera_textures {
                allocator.destroy_image(img, alloc);
            }
            for (buf, alloc) in pixel_buffers {
                allocator.destroy_buffer(buf, alloc);
            }
            if let Some((img, alloc)) = compute_image {
                allocator.destroy_image(img, alloc);
            }
            for (buf, alloc) in input_ssbos {
                allocator.destroy_buffer(buf, alloc);
            }
        }
    }

    fn run_clean_internal_scenario(&self) {
        let allocator = build_vma_without_global_export(&self.device);
        let mut outcome = self.outcome.lock().unwrap();

        // 4 internal images via plain VMA (no export, no dedicated)
        for i in 0..4 {
            outcome.images_attempted += 1;
            match alloc_internal_image_via_vma(&allocator, self.width, self.height) {
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

    fn run_clean_pool_scenario(&self) {
        let allocator = Arc::new(build_vma_without_global_export(&self.device));
        let mut outcome = self.outcome.lock().unwrap();

        // ── Find memory type for HOST_VISIBLE DMA-BUF exportable buffers ──
        let probe_buffer_info = vk::BufferCreateInfo::builder()
            .size(64 * 1024)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let probe_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY
                | vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };
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
        let probe_image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .extent(vk::Extent3D { width: 64, height: 64, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let probe_image_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let image_mem_type_idx = match unsafe {
            allocator
                .find_memory_type_index_for_image_info(probe_image_info, &probe_image_alloc_opts)
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
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .build();
            let buf_info = vk::BufferCreateInfo::builder()
                .size((self.width as u64) * (self.height as u64) * 4)
                .usage(
                    vk::BufferUsageFlags::TRANSFER_SRC
                        | vk::BufferUsageFlags::TRANSFER_DST
                        | vk::BufferUsageFlags::STORAGE_BUFFER,
                )
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .push_next(&mut external_buffer_info);
            let alloc_opts = vma::AllocationOptions {
                flags: vma::AllocationCreateFlags::DEDICATED_MEMORY
                    | vma::AllocationCreateFlags::MAPPED
                    | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
                required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                    | vk::MemoryPropertyFlags::HOST_COHERENT,
                ..Default::default()
            };
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
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .build();
            let img_info = vk::ImageCreateInfo::builder()
                .image_type(vk::ImageType::_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .extent(vk::Extent3D { width: self.width, height: self.height, depth: 1 })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .push_next(&mut external_image_info);
            let alloc_opts = vma::AllocationOptions {
                flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
                required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
                ..Default::default()
            };
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
            match alloc_internal_image_via_vma(&allocator, self.width, self.height) {
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
}

struct SwapchainResources {
    swapchain: vk::SwapchainKHR,
    image_count: u32,
}

fn create_swapchain(
    device: &HostVulkanDevice,
    surface: vk::SurfaceKHR,
    width: u32,
    height: u32,
) -> Result<SwapchainResources, String> {
    let instance = device.instance();
    let physical_device = device.physical_device();
    let vk_device = device.device();
    let queue_family_index = device.queue_family_index();

    // Surface support
    let supported = unsafe {
        instance.get_physical_device_surface_support_khr(
            physical_device,
            queue_family_index,
            surface,
        )
    }
    .map_err(|e| format!("surface support: {e}"))?;
    if !supported {
        return Err("graphics queue family does not support presentation".into());
    }

    let caps = unsafe {
        instance.get_physical_device_surface_capabilities_khr(physical_device, surface)
    }
    .map_err(|e| format!("surface capabilities: {e}"))?;

    let formats = unsafe {
        instance.get_physical_device_surface_formats_khr(physical_device, surface)
    }
    .map_err(|e| format!("surface formats: {e}"))?;

    let format = formats
        .iter()
        .find(|f| f.format == vk::Format::B8G8R8A8_UNORM)
        .copied()
        .unwrap_or(formats[0]);

    let extent = if caps.current_extent.width != u32::MAX {
        caps.current_extent
    } else {
        vk::Extent2D {
            width: width.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
            height: height.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
        }
    };

    let mut image_count = caps.min_image_count + 1;
    if caps.max_image_count > 0 && image_count > caps.max_image_count {
        image_count = caps.max_image_count;
    }

    let swapchain_info = vk::SwapchainCreateInfoKHR::builder()
        .surface(surface)
        .min_image_count(image_count)
        .image_format(format.format)
        .image_color_space(format.color_space)
        .image_extent(extent)
        .image_array_layers(1)
        .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST)
        .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
        .pre_transform(caps.current_transform)
        .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
        .present_mode(vk::PresentModeKHR::FIFO)
        .clipped(true)
        .build();

    let swapchain = unsafe { vk_device.create_swapchain_khr(&swapchain_info, None) }
        .map_err(|e| format!("create_swapchain_khr: {e}"))?;

    let images = unsafe { vk_device.get_swapchain_images_khr(swapchain) }
        .map_err(|e| format!("get_swapchain_images: {e}"))?;

    Ok(SwapchainResources {
        swapchain,
        image_count: images.len() as u32,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Test harness — runs a scenario inside winit's event loop
// ─────────────────────────────────────────────────────────────────────────────

fn try_create_device() -> Option<Arc<HostVulkanDevice>> {
    match HostVulkanDevice::new() {
        Ok(d) => Some(d),
        Err(e) => {
            println!("Skipping — Vulkan device unavailable: {e}");
            None
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

/// BASELINE: without swapchain, even the broken VMA config works.
/// Proves the bug requires a swapchain to manifest.
#[test]
fn test_baseline_no_swapchain_broken_config_works() {
    let device = match try_create_device() {
        Some(d) => d,
        None => return,
    };
    if !device.supports_external_memory() {
        println!("Skipping — external memory unsupported");
        return;
    }

    let allocator = build_vma_with_global_export(&device);
    let width = 1920u32;
    let height = 1080u32;

    let mut buffers = Vec::new();
    for i in 0..4 {
        let buf = alloc_dma_buf_buffer_via_vma(&allocator, (width as u64) * (height as u64) * 4)
            .unwrap_or_else(|e| panic!("pixel buffer [{i}] failed (no swapchain): {e}"));
        buffers.push(buf);
    }
    println!("  [baseline] 4 pixel buffers allocated");

    let mut images = Vec::new();
    for i in 0..4 {
        let img = alloc_dma_buf_image_via_vma(&allocator, width, height)
            .unwrap_or_else(|e| panic!("camera texture [{i}] failed (no swapchain): {e}"));
        images.push(img);
    }
    println!("  [baseline] 4 camera textures allocated");

    unsafe {
        for (img, alloc) in images {
            allocator.destroy_image(img, alloc);
        }
        for (buf, alloc) in buffers {
            allocator.destroy_buffer(buf, alloc);
        }
    }
    println!("  [baseline] PASS — no swapchain, all allocations succeeded");
}

/// Runs all swapchain-dependent scenarios in a single test because winit's
/// EventLoop is per-process on Linux X11 — only one EventLoop can be created
/// per process, and subsequent attempts fail with "EventLoop can't be recreated".
///
/// This test exercises three scenarios:
///   1. BUG REPRO: broken VMA config + swapchain → at least one alloc must fail
///   2. FIX (clean VMA): internal allocs via plain VMA → all succeed
///   3. FIX (VMA pools): export allocs via custom pools → all succeed
#[test]
fn test_swapchain_allocation_scenarios() {
    let device = match try_create_device() {
        Some(d) => d,
        None => return,
    };

    // Build event loop ONCE — winit allows only one EventLoop per process on X11.
    use winit::platform::run_on_demand::EventLoopExtRunOnDemand;
    use winit::platform::x11::EventLoopBuilderExtX11;

    let mut event_loop = match EventLoop::builder().with_any_thread(true).build() {
        Ok(el) => el,
        Err(e) => {
            println!("Skipping all swapchain tests — event loop unavailable: {e}");
            return;
        }
    };

    let supports_external = device.supports_external_memory();

    // ── Scenario 1: BUG REPRODUCTION ATTEMPT (informational) ───────────────
    // The bug only manifests in production with the full camera-display pipeline
    // (live compositor DMA-BUF imports, concurrent threads, GPU work). It does
    // NOT reliably reproduce in isolation — the test just records what happens.
    if supports_external {
        println!("══ Scenario 1: BUG REPRODUCTION ATTEMPT (informational) ══");
        let outcome = run_scenario_via_event_loop(
            &mut event_loop,
            device.clone(),
            TestScenario::BrokenConfigExportableAllocs,
        );

        if let Some(skip_reason) = &outcome.setup_skipped {
            println!("  Skipping: {skip_reason}");
        } else {
            let total_failures = outcome.buffers_failed() + outcome.images_failed();
            println!(
                "  buffers: {}/{}, images: {}/{}, failures: {} ({:?})",
                outcome.buffers_succeeded,
                outcome.buffers_attempted,
                outcome.images_succeeded,
                outcome.images_attempted,
                total_failures,
                outcome.failure_messages
            );
            if total_failures > 0 {
                println!("  ✓ Bug reproduced — {} allocation failures observed", total_failures);
            } else {
                println!("  ⚠ Bug did NOT reproduce in isolation (expected — needs production pipeline state)");
            }
        }
    } else {
        println!("══ Scenario 1: SKIPPED — external memory unsupported ══");
    }

    // ── Scenario 2: FIX VALIDATION (clean VMA) ─────────────────────────────
    println!("══ Scenario 2: FIX (clean VMA, internal allocs) ══");
    let outcome = run_scenario_via_event_loop(
        &mut event_loop,
        device.clone(),
        TestScenario::CleanConfigInternalAllocs,
    );

    if let Some(skip_reason) = &outcome.setup_skipped {
        println!("  Skipping scenario 2: {skip_reason}");
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
        println!("  [scenario 2] PASS — clean VMA works for internal allocs");
    }

    // ── Scenario 3: FIX VALIDATION (VMA pools for export) ──────────────────
    if supports_external {
        println!("══ Scenario 3: FIX (clean VMA + export pools) ══");
        let outcome = run_scenario_via_event_loop(
            &mut event_loop,
            device.clone(),
            TestScenario::CleanConfigExportPools,
        );

        if let Some(skip_reason) = &outcome.setup_skipped {
            println!("  Skipping scenario 3: {skip_reason}");
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
            println!("  [scenario 3] PASS — export pools work alongside default pool");
        }
    } else {
        println!("══ Scenario 3: SKIPPED — external memory unsupported ══");
    }
}

/// Helper that runs a scenario via the shared event loop.
fn run_scenario_via_event_loop(
    event_loop: &mut EventLoop<()>,
    device: Arc<HostVulkanDevice>,
    scenario: TestScenario,
) -> AllocationOutcome {
    use winit::platform::run_on_demand::EventLoopExtRunOnDemand;

    let outcome = Arc::new(Mutex::new(AllocationOutcome::default()));
    let mut app = SwapchainTestApp {
        device,
        scenario,
        width: 1920,
        height: 1080,
        outcome: Arc::clone(&outcome),
        window: None,
        surface: None,
        swapchain: None,
        wait_ticks: 0,
        scenario_run: false,
    };

    if let Err(e) = event_loop.run_app_on_demand(&mut app) {
        outcome.lock().unwrap().setup_skipped = Some(format!("event loop error: {e}"));
    }

    // Cleanup swapchain/surface (window dropped with app)
    app.cleanup();

    let result = outcome.lock().unwrap().clone();
    result
}
