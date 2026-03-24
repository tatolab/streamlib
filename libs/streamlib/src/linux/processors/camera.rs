// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use ash::vk;
use crate::core::rhi::PixelFormat;
use crate::core::{GpuContext, Result, RuntimeContext, StreamError};
use crate::iceoryx2::OutputWriter;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use v4l::buffer::Type;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::FourCC;

/// Number of V4L2 mmap buffers to request.
const V4L2_BUFFER_COUNT: u32 = 4;

/// Default V4L2 device path.
const DEFAULT_DEVICE_PATH: &str = "/dev/video0";

#[derive(Debug, Clone)]
pub struct LinuxCameraDevice {
    pub id: String,
    pub name: String,
}

#[crate::processor("com.tatolab.camera")]
pub struct LinuxCameraProcessor {
    camera_name: String,
    gpu_context: Option<GpuContext>,
    is_capturing: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    capture_thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl crate::core::ManualProcessor for LinuxCameraProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.gpu_context = Some(ctx.gpu.clone());
        tracing::info!("Camera: setup() complete");
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        let frame_count = self.frame_counter.load(Ordering::Relaxed);
        tracing::info!(
            "Camera {}: Teardown (generated {} frames)",
            self.camera_name,
            frame_count
        );
        self.is_capturing.store(false, Ordering::Release);
        if let Some(handle) = self.capture_thread_handle.take() {
            let _ = handle.join();
        }
        std::future::ready(Ok(()))
    }

    fn start(&mut self) -> Result<()> {
        let gpu_context = self.gpu_context.clone().ok_or_else(|| {
            StreamError::Configuration("GPU context not initialized. Call setup() first.".into())
        })?;

        let device_path = self
            .config
            .device_id
            .clone()
            .unwrap_or_else(|| DEFAULT_DEVICE_PATH.to_string());

        // Open the V4L2 device
        let mut dev = v4l::Device::with_path(&device_path).map_err(|e| {
            StreamError::Configuration(format!("Failed to open V4L2 device '{}': {}", device_path, e))
        })?;

        // Query device capabilities
        let caps = dev.query_caps().map_err(|e| {
            StreamError::Configuration(format!("Failed to query device capabilities: {}", e))
        })?;
        self.camera_name = caps.card.clone();
        tracing::info!(
            "Camera: opened '{}' (driver: {}, bus: {})",
            caps.card,
            caps.driver,
            caps.bus
        );

        // Read the device's current format as a fallback baseline
        let current_fmt = dev.format().map_err(|e| {
            StreamError::Configuration(format!("Failed to read current format: {}", e))
        })?;

        // Negotiate format + resolution: enumerate frame sizes for NV12 (preferred) or
        // YUYV, pick the highest resolution, then set_format with those parameters.
        let fmt = {
            let nv12_fourcc = FourCC::new(b"NV12");
            let yuyv_fourcc = FourCC::new(b"YUYV");

            let mut negotiated: Option<v4l::format::Format> = None;

            // Try NV12 first — enumerate available frame sizes and pick largest
            if let Ok(framesizes) = dev.enum_framesizes(nv12_fourcc) {
                let mut best_pixels = 0u64;
                let mut best_w = current_fmt.width;
                let mut best_h = current_fmt.height;
                for fs in &framesizes {
                    match &fs.size {
                        v4l::framesize::FrameSizeEnum::Discrete(d) => {
                            let pixels = d.width as u64 * d.height as u64;
                            if pixels > best_pixels {
                                best_pixels = pixels;
                                best_w = d.width;
                                best_h = d.height;
                            }
                        }
                        v4l::framesize::FrameSizeEnum::Stepwise(s) => {
                            let pixels = s.max_width as u64 * s.max_height as u64;
                            if pixels > best_pixels {
                                best_pixels = pixels;
                                best_w = s.max_width;
                                best_h = s.max_height;
                            }
                        }
                    }
                }
                if best_pixels > 0 {
                    let mut try_fmt = current_fmt.clone();
                    try_fmt.fourcc = nv12_fourcc;
                    try_fmt.width = best_w;
                    try_fmt.height = best_h;
                    if let Ok(f) = dev.set_format(&try_fmt) {
                        if f.fourcc == nv12_fourcc {
                            tracing::info!(
                                "Camera {}: NV12 available, highest resolution {}x{}",
                                self.camera_name,
                                f.width,
                                f.height
                            );
                            negotiated = Some(f);
                        }
                    }
                }
            }

            // If NV12 didn't work, try YUYV with highest available resolution
            if negotiated.is_none() {
                tracing::info!(
                    "Camera {}: NV12 not available, trying YUYV",
                    self.camera_name
                );

                let (best_w, best_h) = if let Ok(framesizes) = dev.enum_framesizes(yuyv_fourcc) {
                    let mut best_pixels = 0u64;
                    let mut w = current_fmt.width;
                    let mut h = current_fmt.height;
                    for fs in &framesizes {
                        match &fs.size {
                            v4l::framesize::FrameSizeEnum::Discrete(d) => {
                                let pixels = d.width as u64 * d.height as u64;
                                if pixels > best_pixels {
                                    best_pixels = pixels;
                                    w = d.width;
                                    h = d.height;
                                }
                            }
                            v4l::framesize::FrameSizeEnum::Stepwise(s) => {
                                let pixels = s.max_width as u64 * s.max_height as u64;
                                if pixels > best_pixels {
                                    best_pixels = pixels;
                                    w = s.max_width;
                                    h = s.max_height;
                                }
                            }
                        }
                    }
                    (w, h)
                } else {
                    // enum_framesizes not supported — use current resolution
                    (current_fmt.width, current_fmt.height)
                };

                let mut try_fmt = current_fmt;
                try_fmt.fourcc = yuyv_fourcc;
                try_fmt.width = best_w;
                try_fmt.height = best_h;
                let f = dev.set_format(&try_fmt).map_err(|e| {
                    StreamError::Configuration(format!(
                        "Failed to set camera format (tried NV12, YUYV): {}",
                        e
                    ))
                })?;
                if f.fourcc != yuyv_fourcc {
                    return Err(StreamError::Configuration(format!(
                        "Camera does not support NV12 or YUYV (driver negotiated {:?})",
                        f.fourcc
                    )));
                }
                negotiated = Some(f);
            }

            negotiated.unwrap()
        };

        let capture_width = fmt.width;
        let capture_height = fmt.height;
        let capture_fourcc = fmt.fourcc;

        tracing::info!(
            "Camera {}: capturing {}x{} {:?}",
            self.camera_name,
            capture_width,
            capture_height,
            capture_fourcc
        );

        // Create mmap stream with V4L2_BUFFER_COUNT buffers
        let mut stream =
            v4l::io::mmap::Stream::with_buffers(&mut dev, Type::VideoCapture, V4L2_BUFFER_COUNT)
                .map_err(|e| {
                    StreamError::Configuration(format!(
                        "Failed to create V4L2 mmap stream: {}",
                        e
                    ))
                })?;

        // Set a poll timeout so the capture thread can check is_capturing periodically.
        // Without this, stream.next() blocks indefinitely on poll(), causing stop() to deadlock
        // if the camera stops producing frames (USB disconnect, device error).
        stream.set_timeout(std::time::Duration::from_secs(1));

        self.is_capturing.store(true, Ordering::Release);

        let is_capturing = Arc::clone(&self.is_capturing);
        let frame_counter = Arc::clone(&self.frame_counter);
        let outputs: Arc<OutputWriter> = self.outputs.clone();
        let camera_name = self.camera_name.clone();

        let handle = std::thread::Builder::new()
            .name(format!("v4l2-capture-{}", device_path))
            .spawn(move || {
                capture_thread_loop(
                    stream,
                    is_capturing,
                    frame_counter,
                    outputs,
                    gpu_context,
                    camera_name,
                    capture_width,
                    capture_height,
                    capture_fourcc,
                );
            })
            .map_err(|e| {
                StreamError::Configuration(format!("Failed to spawn capture thread: {}", e))
            })?;

        self.capture_thread_handle = Some(handle);

        tracing::info!(
            "Camera {}: V4L2 capture started ({}x{} {:?}, {} mmap buffers)",
            self.camera_name,
            capture_width,
            capture_height,
            capture_fourcc,
            V4L2_BUFFER_COUNT
        );
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.is_capturing.store(false, Ordering::Release);

        if let Some(handle) = self.capture_thread_handle.take() {
            let _ = handle.join();
        }

        tracing::info!(
            "Camera {}: Stopped ({} frames)",
            self.camera_name,
            self.frame_counter.load(Ordering::Relaxed)
        );
        Ok(())
    }
}

/// V4L2 capture thread main loop.
///
/// Polls for frames from the mmap stream, converts NV12/YUYV to BGRA via
/// GPU compute shader, writes to a pooled Vulkan pixel buffer, and publishes
/// via OutputWriter.
fn capture_thread_loop(
    mut stream: v4l::io::mmap::Stream,
    is_capturing: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    outputs: Arc<OutputWriter>,
    gpu_context: GpuContext,
    camera_name: String,
    width: u32,
    height: u32,
    fourcc: FourCC,
) {
    let vulkan_device = &gpu_context.device().inner;
    let device = vulkan_device.device();
    let queue = vulkan_device.queue();
    let queue_family_index = vulkan_device.queue_family_index();

    let fourcc_bytes = fourcc.repr;

    // Determine input buffer size and select shader SPIR-V based on pixel format
    let (input_byte_size, shader_spirv): (usize, &[u8]) = match &fourcc_bytes {
        b"NV12" => (
            (width as usize) * (height as usize) * 3 / 2,
            include_bytes!("shaders/nv12_to_bgra.spv"),
        ),
        b"YUYV" => (
            (width as usize) * (height as usize) * 2,
            include_bytes!("shaders/yuyv_to_bgra.spv"),
        ),
        _ => {
            eprintln!(
                "[Camera {}] Unsupported format {:?} — no GPU compute shader available",
                camera_name, fourcc
            );
            return;
        }
    };

    // Pad input size to uint32 alignment for SSBO
    let input_alloc_size = ((input_byte_size + 3) / 4 * 4) as vk::DeviceSize;

    // -----------------------------------------------------------------------
    // Create double-buffered input SSBOs (HOST_VISIBLE) for raw V4L2 data upload.
    // CPU uploads frame N+1 to SSBO[1] while GPU processes frame N on SSBO[0].
    // -----------------------------------------------------------------------
    let input_buffer_info = vk::BufferCreateInfo::default()
        .size(input_alloc_size)
        .usage(vk::BufferUsageFlags::STORAGE_BUFFER)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);

    let mut input_buffers = [vk::Buffer::null(); 2];
    let mut input_memories = [vk::DeviceMemory::null(); 2];
    let mut input_mapped_ptrs = [std::ptr::null_mut::<u8>(); 2];

    for i in 0..2 {
        let input_buffer = match unsafe { device.create_buffer(&input_buffer_info, None) } {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[Camera {}] Failed to create input SSBO[{}]: {}", camera_name, i, e);
                for j in 0..i {
                    unsafe {
                        device.unmap_memory(input_memories[j]);
                        device.free_memory(input_memories[j], None);
                        device.destroy_buffer(input_buffers[j], None);
                    }
                }
                return;
            }
        };

        let input_mem_reqs = unsafe { device.get_buffer_memory_requirements(input_buffer) };

        let input_memory_type = match vulkan_device.find_memory_type(
            input_mem_reqs.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        ) {
            Ok(idx) => idx,
            Err(e) => {
                eprintln!("[Camera {}] No suitable memory type for input SSBO[{}]: {}", camera_name, i, e);
                unsafe { device.destroy_buffer(input_buffer, None) };
                for j in 0..i {
                    unsafe {
                        device.unmap_memory(input_memories[j]);
                        device.free_memory(input_memories[j], None);
                        device.destroy_buffer(input_buffers[j], None);
                    }
                }
                return;
            }
        };

        let input_alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(input_mem_reqs.size)
            .memory_type_index(input_memory_type);

        let input_memory = match unsafe { device.allocate_memory(&input_alloc_info, None) } {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[Camera {}] Failed to allocate input SSBO[{}] memory: {}", camera_name, i, e);
                unsafe { device.destroy_buffer(input_buffer, None) };
                for j in 0..i {
                    unsafe {
                        device.unmap_memory(input_memories[j]);
                        device.free_memory(input_memories[j], None);
                        device.destroy_buffer(input_buffers[j], None);
                    }
                }
                return;
            }
        };

        if let Err(e) = unsafe { device.bind_buffer_memory(input_buffer, input_memory, 0) } {
            eprintln!("[Camera {}] Failed to bind input SSBO[{}] memory: {}", camera_name, i, e);
            unsafe {
                device.free_memory(input_memory, None);
                device.destroy_buffer(input_buffer, None);
            }
            for j in 0..i {
                unsafe {
                    device.unmap_memory(input_memories[j]);
                    device.free_memory(input_memories[j], None);
                    device.destroy_buffer(input_buffers[j], None);
                }
            }
            return;
        }

        let input_mapped_ptr = match unsafe {
            device.map_memory(input_memory, 0, input_alloc_size, vk::MemoryMapFlags::empty())
        } {
            Ok(ptr) => ptr as *mut u8,
            Err(e) => {
                eprintln!("[Camera {}] Failed to map input SSBO[{}]: {}", camera_name, i, e);
                unsafe {
                    device.free_memory(input_memory, None);
                    device.destroy_buffer(input_buffer, None);
                }
                for j in 0..i {
                    unsafe {
                        device.unmap_memory(input_memories[j]);
                        device.free_memory(input_memories[j], None);
                        device.destroy_buffer(input_buffers[j], None);
                    }
                }
                return;
            }
        };

        input_buffers[i] = input_buffer;
        input_memories[i] = input_memory;
        input_mapped_ptrs[i] = input_mapped_ptr;
    }

    // -----------------------------------------------------------------------
    // Create compute pipeline
    // -----------------------------------------------------------------------

    // Shader module
    let spirv_code = match ash::util::read_spv(&mut std::io::Cursor::new(shader_spirv)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("[Camera {}] Failed to read SPIR-V: {}", camera_name, e);
            unsafe {
                for k in 0..2 {
                    device.unmap_memory(input_memories[k]);
                    device.free_memory(input_memories[k], None);
                    device.destroy_buffer(input_buffers[k], None);
                }
            }
            return;
        }
    };

    let shader_module_info = vk::ShaderModuleCreateInfo::default().code(&spirv_code);
    let shader_module = match unsafe { device.create_shader_module(&shader_module_info, None) } {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[Camera {}] Failed to create shader module: {}", camera_name, e);
            unsafe {
                for k in 0..2 {
                    device.unmap_memory(input_memories[k]);
                    device.free_memory(input_memories[k], None);
                    device.destroy_buffer(input_buffers[k], None);
                }
            }
            return;
        }
    };

    // Descriptor set layout: binding 0 = input SSBO, binding 1 = output storage image
    let bindings = [
        vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
    ];

    let descriptor_set_layout_info =
        vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);

    let descriptor_set_layout =
        match unsafe { device.create_descriptor_set_layout(&descriptor_set_layout_info, None) } {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[Camera {}] Failed to create descriptor set layout: {}", camera_name, e);
                unsafe {
                    device.destroy_shader_module(shader_module, None);
                    for k in 0..2 {
                        device.unmap_memory(input_memories[k]);
                        device.free_memory(input_memories[k], None);
                        device.destroy_buffer(input_buffers[k], None);
                    }
                }
                return;
            }
        };

    // Push constant range: width + height (2 × uint32 = 8 bytes)
    let push_constant_range = vk::PushConstantRange::default()
        .stage_flags(vk::ShaderStageFlags::COMPUTE)
        .offset(0)
        .size(8);

    let set_layouts = [descriptor_set_layout];
    let push_constant_ranges = [push_constant_range];
    let pipeline_layout_info = vk::PipelineLayoutCreateInfo::default()
        .set_layouts(&set_layouts)
        .push_constant_ranges(&push_constant_ranges);

    let pipeline_layout =
        match unsafe { device.create_pipeline_layout(&pipeline_layout_info, None) } {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[Camera {}] Failed to create pipeline layout: {}", camera_name, e);
                unsafe {
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                    for k in 0..2 {
                        device.unmap_memory(input_memories[k]);
                        device.free_memory(input_memories[k], None);
                        device.destroy_buffer(input_buffers[k], None);
                    }
                }
                return;
            }
        };

    // Compute pipeline
    let stage_info = vk::PipelineShaderStageCreateInfo::default()
        .stage(vk::ShaderStageFlags::COMPUTE)
        .module(shader_module)
        .name(c"main");

    let compute_pipeline_info = vk::ComputePipelineCreateInfo::default()
        .stage(stage_info)
        .layout(pipeline_layout);

    let compute_pipeline = match unsafe {
        device.create_compute_pipelines(vk::PipelineCache::null(), &[compute_pipeline_info], None)
    } {
        Ok(pipelines) => pipelines[0],
        Err((_, e)) => {
            eprintln!("[Camera {}] Failed to create compute pipeline: {}", camera_name, e);
            unsafe {
                device.destroy_pipeline_layout(pipeline_layout, None);
                device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                device.destroy_shader_module(shader_module, None);
                for k in 0..2 {
                    device.unmap_memory(input_memories[k]);
                    device.free_memory(input_memories[k], None);
                    device.destroy_buffer(input_buffers[k], None);
                }
            }
            return;
        }
    };

    // Descriptor pool (1 set, 1 storage buffer + 1 storage image)
    let pool_sizes = [
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1),
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count(1),
    ];

    let descriptor_pool_info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(1)
        .pool_sizes(&pool_sizes);

    let descriptor_pool =
        match unsafe { device.create_descriptor_pool(&descriptor_pool_info, None) } {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[Camera {}] Failed to create descriptor pool: {}", camera_name, e);
                unsafe {
                    device.destroy_pipeline(compute_pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                    for k in 0..2 {
                        device.unmap_memory(input_memories[k]);
                        device.free_memory(input_memories[k], None);
                        device.destroy_buffer(input_buffers[k], None);
                    }
                }
                return;
            }
        };

    // Allocate descriptor set
    let alloc_info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(descriptor_pool)
        .set_layouts(&set_layouts);

    let descriptor_set = match unsafe { device.allocate_descriptor_sets(&alloc_info) } {
        Ok(sets) => sets[0],
        Err(e) => {
            eprintln!("[Camera {}] Failed to allocate descriptor set: {}", camera_name, e);
            unsafe {
                device.destroy_descriptor_pool(descriptor_pool, None);
                device.destroy_pipeline(compute_pipeline, None);
                device.destroy_pipeline_layout(pipeline_layout, None);
                device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                device.destroy_shader_module(shader_module, None);
                for k in 0..2 {
                    device.unmap_memory(input_memories[k]);
                    device.free_memory(input_memories[k], None);
                    device.destroy_buffer(input_buffers[k], None);
                }
            }
            return;
        }
    };

    // -----------------------------------------------------------------------
    // Create command pool + command buffer + fence for compute dispatch
    // -----------------------------------------------------------------------
    let pool_info = vk::CommandPoolCreateInfo::default()
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
        .queue_family_index(queue_family_index);

    let compute_command_pool = match unsafe { device.create_command_pool(&pool_info, None) } {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[Camera {}] Failed to create compute command pool: {}", camera_name, e);
            unsafe {
                device.destroy_descriptor_pool(descriptor_pool, None);
                device.destroy_pipeline(compute_pipeline, None);
                device.destroy_pipeline_layout(pipeline_layout, None);
                device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                device.destroy_shader_module(shader_module, None);
                for k in 0..2 {
                    device.unmap_memory(input_memories[k]);
                    device.free_memory(input_memories[k], None);
                    device.destroy_buffer(input_buffers[k], None);
                }
            }
            return;
        }
    };

    let cmd_alloc_info = vk::CommandBufferAllocateInfo::default()
        .command_pool(compute_command_pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);

    let compute_command_buffer =
        match unsafe { device.allocate_command_buffers(&cmd_alloc_info) } {
            Ok(bufs) => bufs[0],
            Err(e) => {
                eprintln!("[Camera {}] Failed to allocate compute command buffer: {}", camera_name, e);
                unsafe {
                    device.destroy_command_pool(compute_command_pool, None);
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(compute_pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                    for k in 0..2 {
                        device.unmap_memory(input_memories[k]);
                        device.free_memory(input_memories[k], None);
                        device.destroy_buffer(input_buffers[k], None);
                    }
                }
                return;
            }
        };

    let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
    let mut compute_fences = [vk::Fence::null(); 2];
    for i in 0..2 {
        compute_fences[i] = match unsafe { device.create_fence(&fence_info, None) } {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "[Camera {}] Failed to create compute fence[{}]: {}",
                    camera_name, i, e
                );
                unsafe {
                    for j in 0..i {
                        device.destroy_fence(compute_fences[j], None);
                    }
                    device.destroy_command_pool(compute_command_pool, None);
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(compute_pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                    for k in 0..2 {
                        device.unmap_memory(input_memories[k]);
                        device.free_memory(input_memories[k], None);
                        device.destroy_buffer(input_buffers[k], None);
                    }
                }
                return;
            }
        };
    }

    // -----------------------------------------------------------------------
    // Create DEVICE_LOCAL storage image for compute output (fast VRAM writes)
    // -----------------------------------------------------------------------
    let compute_output_image_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);

    let compute_output_image =
        match unsafe { device.create_image(&compute_output_image_info, None) } {
            Ok(img) => img,
            Err(e) => {
                eprintln!(
                    "[Camera {}] Failed to create compute output image: {}",
                    camera_name, e
                );
                unsafe {
                    for k in 0..2 {
                        device.destroy_fence(compute_fences[k], None);
                    }
                    device.destroy_command_pool(compute_command_pool, None);
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(compute_pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                    for k in 0..2 {
                        device.unmap_memory(input_memories[k]);
                        device.free_memory(input_memories[k], None);
                        device.destroy_buffer(input_buffers[k], None);
                    }
                }
                return;
            }
        };

    let compute_output_mem_reqs =
        unsafe { device.get_image_memory_requirements(compute_output_image) };

    let compute_output_memory_type = match vulkan_device.find_memory_type(
        compute_output_mem_reqs.memory_type_bits,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    ) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!(
                "[Camera {}] No device-local memory for compute output image: {}",
                camera_name, e
            );
            unsafe {
                device.destroy_image(compute_output_image, None);
                for k in 0..2 {
                    device.destroy_fence(compute_fences[k], None);
                }
                device.destroy_command_pool(compute_command_pool, None);
                device.destroy_descriptor_pool(descriptor_pool, None);
                device.destroy_pipeline(compute_pipeline, None);
                device.destroy_pipeline_layout(pipeline_layout, None);
                device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                device.destroy_shader_module(shader_module, None);
                for k in 0..2 {
                    device.unmap_memory(input_memories[k]);
                    device.free_memory(input_memories[k], None);
                    device.destroy_buffer(input_buffers[k], None);
                }
            }
            return;
        }
    };

    let compute_output_alloc_info = vk::MemoryAllocateInfo::default()
        .allocation_size(compute_output_mem_reqs.size)
        .memory_type_index(compute_output_memory_type);

    let compute_output_image_memory =
        match unsafe { device.allocate_memory(&compute_output_alloc_info, None) } {
            Ok(m) => m,
            Err(e) => {
                eprintln!(
                    "[Camera {}] Failed to allocate compute output image memory: {}",
                    camera_name, e
                );
                unsafe {
                    device.destroy_image(compute_output_image, None);
                    for k in 0..2 {
                        device.destroy_fence(compute_fences[k], None);
                    }
                    device.destroy_command_pool(compute_command_pool, None);
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(compute_pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                    for k in 0..2 {
                        device.unmap_memory(input_memories[k]);
                        device.free_memory(input_memories[k], None);
                        device.destroy_buffer(input_buffers[k], None);
                    }
                }
                return;
            }
        };

    if let Err(e) =
        unsafe { device.bind_image_memory(compute_output_image, compute_output_image_memory, 0) }
    {
        eprintln!(
            "[Camera {}] Failed to bind compute output image memory: {}",
            camera_name, e
        );
        unsafe {
            device.free_memory(compute_output_image_memory, None);
            device.destroy_image(compute_output_image, None);
            for k in 0..2 {
                device.destroy_fence(compute_fences[k], None);
            }
            device.destroy_command_pool(compute_command_pool, None);
            device.destroy_descriptor_pool(descriptor_pool, None);
            device.destroy_pipeline(compute_pipeline, None);
            device.destroy_pipeline_layout(pipeline_layout, None);
            device.destroy_descriptor_set_layout(descriptor_set_layout, None);
            device.destroy_shader_module(shader_module, None);
            for k in 0..2 {
                device.unmap_memory(input_memories[k]);
                device.free_memory(input_memories[k], None);
                device.destroy_buffer(input_buffers[k], None);
            }
        }
        return;
    }

    let compute_output_view_info = vk::ImageViewCreateInfo::default()
        .image(compute_output_image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .subresource_range(
            vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .base_mip_level(0)
                .level_count(1)
                .base_array_layer(0)
                .layer_count(1),
        );

    let compute_output_image_view =
        match unsafe { device.create_image_view(&compute_output_view_info, None) } {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "[Camera {}] Failed to create compute output image view: {}",
                    camera_name, e
                );
                unsafe {
                    device.free_memory(compute_output_image_memory, None);
                    device.destroy_image(compute_output_image, None);
                    for k in 0..2 {
                        device.destroy_fence(compute_fences[k], None);
                    }
                    device.destroy_command_pool(compute_command_pool, None);
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(compute_pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(shader_module, None);
                    for k in 0..2 {
                        device.unmap_memory(input_memories[k]);
                        device.free_memory(input_memories[k], None);
                        device.destroy_buffer(input_buffers[k], None);
                    }
                }
                return;
            }
        };

    // Write descriptor set — input SSBO binding will be updated per-frame for double buffering
    let input_buffer_descriptor = vk::DescriptorBufferInfo::default()
        .buffer(input_buffers[0])
        .offset(0)
        .range(input_alloc_size);
    let input_buffer_infos = [input_buffer_descriptor];

    let output_image_descriptor = vk::DescriptorImageInfo::default()
        .image_layout(vk::ImageLayout::GENERAL)
        .image_view(compute_output_image_view)
        .sampler(vk::Sampler::null());
    let output_image_infos = [output_image_descriptor];

    let descriptor_writes = [
        vk::WriteDescriptorSet::default()
            .dst_set(descriptor_set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .buffer_info(&input_buffer_infos),
        vk::WriteDescriptorSet::default()
            .dst_set(descriptor_set)
            .dst_binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
            .image_info(&output_image_infos),
    ];

    unsafe {
        device.update_descriptor_sets(&descriptor_writes, &[]);
    }

    let dispatch_x = (width + 15) / 16;
    let dispatch_y = (height + 15) / 16;
    let output_buffer_size = (width as vk::DeviceSize) * (height as vk::DeviceSize) * 4;

    eprintln!(
        "[Camera {}] GPU compute pipeline ready ({:?}, {}x{}, dispatch {}x{})",
        camera_name, fourcc, width, height, dispatch_x, dispatch_y
    );

    // -----------------------------------------------------------------------
    // Main capture loop (double-buffered: CPU uploads to SSBO[N] while GPU
    // processes SSBO[1-N])
    // -----------------------------------------------------------------------
    let mut ping_pong_index: usize = 0;

    while is_capturing.load(Ordering::Acquire) {
        let (buf, meta) = match stream.next() {
            Ok(frame) => frame,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                // Poll timeout — check is_capturing and retry
                continue;
            }
            Err(e) => {
                if is_capturing.load(Ordering::Acquire) {
                    eprintln!("[Camera {}] V4L2 stream error: {}", camera_name, e);
                }
                break;
            }
        };

        if !is_capturing.load(Ordering::Acquire) {
            break;
        }

        let frame_num = frame_counter.fetch_add(1, Ordering::Relaxed);

        // Acquire a BGRA pixel buffer from the pool (used as compute output SSBO)
        let (pool_id, pooled_buffer) =
            match gpu_context.acquire_pixel_buffer(width, height, PixelFormat::Bgra32) {
                Ok(result) => result,
                Err(e) => {
                    if frame_num == 0 {
                        eprintln!(
                            "[Camera {}] Failed to acquire pixel buffer: {}",
                            camera_name, e
                        );
                    }
                    continue;
                }
            };

        let output_vk_buffer = pooled_buffer.buffer_ref().inner.buffer();

        let current_ssbo = ping_pong_index;
        let current_fence = compute_fences[current_ssbo];

        // Wait for any previous GPU work on this SSBO slot before uploading
        unsafe {
            let _ = device.wait_for_fences(&[current_fence], true, u64::MAX);
            let _ = device.reset_fences(&[current_fence]);
        }

        // Upload raw V4L2 frame data to current input SSBO
        let copy_len = buf.len().min(input_byte_size);
        unsafe {
            std::ptr::copy_nonoverlapping(buf.as_ptr(), input_mapped_ptrs[current_ssbo], copy_len);
        }

        // Update descriptor set binding 0 to point to current input SSBO
        let input_buffer_descriptor = vk::DescriptorBufferInfo::default()
            .buffer(input_buffers[current_ssbo])
            .offset(0)
            .range(input_alloc_size);
        let input_buffer_infos = [input_buffer_descriptor];
        let input_descriptor_write = vk::WriteDescriptorSet::default()
            .dst_set(descriptor_set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .buffer_info(&input_buffer_infos);
        unsafe {
            device.update_descriptor_sets(&[input_descriptor_write], &[]);
        }

        // Record compute dispatch command buffer
        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        let color_subresource_range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .base_mip_level(0)
            .level_count(1)
            .base_array_layer(0)
            .layer_count(1);

        unsafe {
            if device
                .reset_command_buffer(compute_command_buffer, vk::CommandBufferResetFlags::empty())
                .is_err()
            {
                continue;
            }

            if device
                .begin_command_buffer(compute_command_buffer, &begin_info)
                .is_err()
            {
                continue;
            }

            // Transition storage image: UNDEFINED → GENERAL (discard old contents)
            let image_barrier_to_general = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(compute_output_image)
                .subresource_range(color_subresource_range)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::SHADER_WRITE);

            device.cmd_pipeline_barrier(
                compute_command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[image_barrier_to_general],
            );

            device.cmd_bind_pipeline(
                compute_command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                compute_pipeline,
            );

            device.cmd_bind_descriptor_sets(
                compute_command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                pipeline_layout,
                0,
                &[descriptor_set],
                &[],
            );

            // Push constants: width, height
            let push_data = [width, height];
            let push_bytes: &[u8] = std::slice::from_raw_parts(
                push_data.as_ptr() as *const u8,
                std::mem::size_of_val(&push_data),
            );
            device.cmd_push_constants(
                compute_command_buffer,
                pipeline_layout,
                vk::ShaderStageFlags::COMPUTE,
                0,
                push_bytes,
            );

            device.cmd_dispatch(compute_command_buffer, dispatch_x, dispatch_y, 1);

            // Transition storage image: GENERAL → TRANSFER_SRC_OPTIMAL
            let image_barrier_to_transfer = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(compute_output_image)
                .subresource_range(color_subresource_range)
                .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ);

            device.cmd_pipeline_barrier(
                compute_command_buffer,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[image_barrier_to_transfer],
            );

            // Copy storage image → pooled pixel buffer (for IPC sharing)
            let copy_region = vk::BufferImageCopy::default()
                .buffer_offset(0)
                .buffer_row_length(width)
                .buffer_image_height(height)
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .mip_level(0)
                        .base_array_layer(0)
                        .layer_count(1),
                )
                .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .image_extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                });

            device.cmd_copy_image_to_buffer(
                compute_command_buffer,
                compute_output_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                output_vk_buffer,
                &[copy_region],
            );

            // Buffer barrier: transfer write → host/transfer read
            let buffer_barrier = vk::BufferMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::HOST_READ | vk::AccessFlags::TRANSFER_READ)
                .buffer(output_vk_buffer)
                .offset(0)
                .size(output_buffer_size);

            device.cmd_pipeline_barrier(
                compute_command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::HOST | vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[buffer_barrier],
                &[],
            );

            if device.end_command_buffer(compute_command_buffer).is_err() {
                continue;
            }

            // Submit and wait for completion
            let command_buffers = [compute_command_buffer];
            let submit_info = vk::SubmitInfo::default().command_buffers(&command_buffers);

            if let Err(e) = device.queue_submit(queue, &[submit_info], current_fence) {
                if frame_num == 0 {
                    eprintln!(
                        "[Camera {}] Failed to submit compute dispatch: {}",
                        camera_name, e
                    );
                }
                continue;
            }

            // Wait for this frame to complete before publishing
            let _ = device.wait_for_fences(&[current_fence], true, u64::MAX);
        }

        let surface_id = pool_id.to_string();
        let timestamp_ns = crate::core::media_clock::MediaClock::now().as_nanos() as i64;

        let ipc_frame = crate::_generated_::Videoframe {
            surface_id,
            width,
            height,
            timestamp_ns: timestamp_ns.to_string(),
            frame_index: frame_num.to_string(),
        };

        if let Err(e) = outputs.write("video", &ipc_frame) {
            eprintln!("[Camera {}] Failed to write frame: {}", camera_name, e);
            continue;
        }

        if frame_num == 0 {
            eprintln!(
                "[Camera {}] First frame captured via GPU compute (seq={}, {}x{} {:?})",
                camera_name, meta.sequence, width, height, fourcc
            );
        } else if frame_num % 300 == 0 {
            eprintln!("[Camera {}] Frame #{}", camera_name, frame_num);
        }

        // Toggle ping-pong index for next frame
        ping_pong_index = 1 - ping_pong_index;
    }

    // -----------------------------------------------------------------------
    // Cleanup — destroy all compute pipeline resources
    // -----------------------------------------------------------------------
    unsafe {
        let _ = device.device_wait_idle();
        device.destroy_image_view(compute_output_image_view, None);
        device.destroy_image(compute_output_image, None);
        device.free_memory(compute_output_image_memory, None);
        for k in 0..2 {
            device.destroy_fence(compute_fences[k], None);
        }
        device.destroy_command_pool(compute_command_pool, None);
        device.destroy_descriptor_pool(descriptor_pool, None);
        device.destroy_pipeline(compute_pipeline, None);
        device.destroy_pipeline_layout(pipeline_layout, None);
        device.destroy_descriptor_set_layout(descriptor_set_layout, None);
        device.destroy_shader_module(shader_module, None);
        for k in 0..2 {
            device.unmap_memory(input_memories[k]);
            device.free_memory(input_memories[k], None);
            device.destroy_buffer(input_buffers[k], None);
        }
    }
}

impl LinuxCameraProcessor::Processor {
    /// Enumerate available V4L2 camera devices.
    pub fn list_devices() -> Result<Vec<LinuxCameraDevice>> {
        let mut devices = Vec::new();

        // Scan /dev/video* devices
        for entry in std::fs::read_dir("/dev").map_err(|e| {
            StreamError::Configuration(format!("Failed to read /dev: {}", e))
        })? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };

            if !name.starts_with("video") {
                continue;
            }

            let dev = match v4l::Device::with_path(&path) {
                Ok(d) => d,
                Err(_) => continue,
            };

            let caps = match dev.query_caps() {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Only include devices with video capture capability
            if !caps
                .capabilities
                .contains(v4l::capability::Flags::VIDEO_CAPTURE)
            {
                continue;
            }

            devices.push(LinuxCameraDevice {
                id: path.to_string_lossy().to_string(),
                name: caps.card,
            });
        }

        Ok(devices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::_generated_::CameraConfig;
    use crate::core::GeneratedProcessor;

    #[test]
    fn test_list_devices() {
        let devices = LinuxCameraProcessor::Processor::list_devices();
        assert!(devices.is_ok());

        if let Ok(devices) = devices {
            println!("Found {} V4L2 camera devices:", devices.len());
            for device in &devices {
                println!("  [{}] {}", device.id, device.name);
            }
        }
    }

    #[test]
    fn test_create_default_processor() {
        let config = CameraConfig {
            device_id: None,
            min_fps: None,
            max_fps: None,
        };

        let result = LinuxCameraProcessor::Processor::from_config(config);

        match result {
            Ok(_processor) => {
                println!("Successfully created camera processor from config");
            }
            Err(e) => {
                println!(
                    "Note: Could not create camera processor (may require permissions): {}",
                    e
                );
            }
        }
    }

    #[test]
    #[ignore] // Requires real V4L2 camera hardware - not available in CI
    fn test_capture_single_frame() {
        let mut dev = v4l::Device::with_path(DEFAULT_DEVICE_PATH)
            .expect("Failed to open /dev/video0");

        let caps = dev.query_caps().expect("Failed to query caps");
        println!("Device: {} ({})", caps.card, caps.driver);

        let mut fmt = dev.format().expect("Failed to read format");
        println!("Default format: {}x{} {:?}", fmt.width, fmt.height, fmt.fourcc);

        // Try NV12 first, fall back to YUYV if device is busy or doesn't support NV12
        fmt.fourcc = FourCC::new(b"NV12");
        let fmt = match dev.set_format(&fmt) {
            Ok(f) => f,
            Err(e) => {
                println!("NV12 not available ({}), trying YUYV", e);
                let mut fmt = dev.format().expect("Failed to read format");
                fmt.fourcc = FourCC::new(b"YUYV");
                match dev.set_format(&fmt) {
                    Ok(f) => f,
                    Err(e2) => {
                        println!(
                            "YUYV also not available ({}), skipping test (device likely busy)",
                            e2
                        );
                        return;
                    }
                }
            }
        };
        println!("Capture format: {}x{} {:?}", fmt.width, fmt.height, fmt.fourcc);

        let mut stream =
            v4l::io::mmap::Stream::with_buffers(&mut dev, Type::VideoCapture, 4)
                .expect("Failed to create mmap stream");

        let (buf, meta) = stream.next().expect("Failed to capture frame");

        println!(
            "Captured frame: {} bytes, seq={}, timestamp={}",
            buf.len(),
            meta.sequence,
            meta.timestamp
        );

        assert!(
            !buf.is_empty(),
            "Frame is empty - camera may not be producing frames"
        );

        // Verify frame has non-zero data
        let nonzero_count = buf.iter().filter(|&&b| b != 0).count();
        assert!(
            nonzero_count > 0,
            "Frame is all zeros - camera may not be producing frames"
        );

        println!(
            "Frame validation passed ({} bytes, {} non-zero)",
            buf.len(),
            nonzero_count
        );
    }
}
