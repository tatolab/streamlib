// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cyberpunk Compositor - Person segmentation with procedural cyberpunk background.
//!
//! Uses Apple Vision framework for real-time person segmentation.
//! The segmentation request is stateful and reused across frames for temporal smoothing.

use objc2::rc::{autoreleasepool, Retained};
use objc2::AnyThread;
use objc2_core_video::{
    kCVPixelFormatType_OneComponent8, CVPixelBuffer, CVPixelBufferGetBaseAddress,
    CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight, CVPixelBufferGetWidth,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSDictionary};
use objc2_vision::{
    VNGeneratePersonSegmentationRequest, VNGeneratePersonSegmentationRequestQualityLevel,
    VNImageOption, VNImageRequestHandler, VNPixelBufferObservation, VNRequest,
};
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;
use streamlib::core::rhi::{
    PixelBufferDescriptor, PixelFormat, RhiPixelBufferPool, RhiTextureCache,
};
use streamlib::core::{
    GpuContext, LinkInput, LinkOutput, Result, RuntimeContext, StreamError, VideoFrame,
};

/// Set real-time thread priority using Mach time-constraint policy.
fn set_realtime_thread_priority() {
    use mach2::kern_return::KERN_SUCCESS;
    use mach2::thread_policy::{
        thread_time_constraint_policy_data_t, THREAD_TIME_CONSTRAINT_POLICY,
    };

    extern "C" {
        fn mach_thread_self() -> u32;
        fn thread_policy_set(
            thread: u32,
            flavor: u32,
            policy_info: *const i32,
            policy_info_count: u32,
        ) -> i32;
    }

    let period_ns = 10_000_000u64; // 10ms
    let computation_ns = 5_000_000u64; // 5ms
    let constraint_ns = 7_000_000u64; // 7ms

    let mut timebase_info = mach2::mach_time::mach_timebase_info_data_t { numer: 0, denom: 0 };

    unsafe {
        mach2::mach_time::mach_timebase_info(&mut timebase_info as *mut _);

        let period = (period_ns * timebase_info.denom as u64) / timebase_info.numer as u64;
        let computation =
            (computation_ns * timebase_info.denom as u64) / timebase_info.numer as u64;
        let constraint = (constraint_ns * timebase_info.denom as u64) / timebase_info.numer as u64;

        let policy = thread_time_constraint_policy_data_t {
            period: period as u32,
            computation: computation as u32,
            constraint: constraint as u32,
            preemptible: 1,
        };

        let result = thread_policy_set(
            mach_thread_self(),
            THREAD_TIME_CONSTRAINT_POLICY,
            &policy as *const _ as *const i32,
            (std::mem::size_of::<thread_time_constraint_policy_data_t>() / 4) as u32,
        );

        if result == KERN_SUCCESS {
            tracing::info!("Vision thread: applied real-time priority");
        } else {
            tracing::warn!(
                "Vision thread: failed to set real-time priority ({})",
                result
            );
        }
    }
}

#[repr(C)]
struct CompositorUniforms {
    time: f32,
    mask_threshold: f32,
    edge_feather: f32,
    _padding: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, streamlib::ConfigDescriptor)]
pub struct CyberpunkCompositorConfig {
    /// Quality level: 0=Fast (real-time), 1=Balanced, 2=Accurate.
    /// Fast is recommended for live video. Accurate enables temporal smoothing.
    pub quality_level: u8,
    /// Mask threshold for person/background separation (0.0-1.0).
    pub mask_threshold: f32,
    /// Edge feather amount for smooth mask edges (0.0-0.5).
    pub edge_feather: f32,
}

impl Default for CyberpunkCompositorConfig {
    fn default() -> Self {
        Self {
            quality_level: 0, // Fast - best for real-time video
            mask_threshold: 0.5,
            edge_feather: 0.15,
        }
    }
}

/// Mask data produced by segmentation thread.
struct SegmentationMaskData {
    pixels: Vec<u8>,
    width: u64,
    height: u64,
}

#[streamlib::processor(
    name = "CyberpunkCompositor",
    execution = Reactive,
    description = "Person segmentation with cyberpunk procedural background",
    unsafe_send
)]
pub struct CyberpunkCompositorProcessor {
    #[streamlib::input(description = "Video frames to process")]
    video_in: LinkInput<VideoFrame>,

    #[streamlib::output(description = "Composited video frames")]
    video_out: LinkOutput<VideoFrame>,

    #[streamlib::config]
    config: CyberpunkCompositorConfig,

    gpu_context: Option<GpuContext>,
    metal_command_queue: Option<metal::CommandQueue>,
    render_pipeline: Option<metal::RenderPipelineState>,
    sampler: Option<metal::SamplerState>,
    uniforms_buffer: Option<metal::Buffer>,
    default_mask_texture: Option<metal::Texture>,
    current_mask_texture: Option<metal::Texture>,
    current_mask_dimensions: Option<(u64, u64)>,
    blur_h_pipeline: Option<metal::ComputePipelineState>,
    blur_v_pipeline: Option<metal::ComputePipelineState>,
    blur_temp_texture: Option<metal::Texture>,
    start_time: Option<Instant>,
    frame_count: AtomicU64,

    // Async segmentation
    segmentation_sender: Option<std::sync::mpsc::SyncSender<streamlib::core::rhi::RhiPixelBuffer>>,
    pending_mask: Arc<parking_lot::Mutex<Option<SegmentationMaskData>>>,
    mask_ready: Arc<AtomicBool>,
    segmentation_running: Arc<AtomicBool>,
    segmentation_thread: Option<JoinHandle<()>>,

    // RHI resources
    texture_cache: Option<RhiTextureCache>,
    output_pool: Option<RhiPixelBufferPool>,
    output_pool_dimensions: Option<(u32, u32)>,
}

/// Run Vision segmentation using a reusable request (for temporal smoothing).
fn run_vision_segmentation(
    buffer: &streamlib::core::rhi::RhiPixelBuffer,
    request: &VNGeneratePersonSegmentationRequest,
) -> Option<SegmentationMaskData> {
    let pixel_buffer = buffer.as_ptr() as *const CVPixelBuffer;

    // Create handler for this frame's pixel buffer
    let empty_dict: Retained<NSDictionary<VNImageOption, objc2::runtime::AnyObject>> =
        NSDictionary::new();

    let handler = unsafe {
        let pixel_buffer_ref = &*pixel_buffer;
        VNImageRequestHandler::initWithCVPixelBuffer_options(
            VNImageRequestHandler::alloc(),
            pixel_buffer_ref,
            &empty_dict,
        )
    };

    // Perform the request
    let requests: Retained<NSArray<VNRequest>> = {
        let request_ref: &VNRequest = request;
        NSArray::from_slice(&[request_ref])
    };

    if handler.performRequests_error(&requests).is_err() {
        return None;
    }

    // Extract mask data
    unsafe {
        let results = request.results()?;
        if results.count() == 0 {
            return None;
        }

        let observation: Retained<VNPixelBufferObservation> = results.objectAtIndex(0);
        let mask_buffer = observation.pixelBuffer();

        let lock_result =
            CVPixelBufferLockBaseAddress(&mask_buffer, CVPixelBufferLockFlags::ReadOnly);
        if lock_result != 0 {
            return None;
        }

        let base_address = CVPixelBufferGetBaseAddress(&mask_buffer);
        let width = CVPixelBufferGetWidth(&mask_buffer);
        let height = CVPixelBufferGetHeight(&mask_buffer);
        let bytes_per_row = CVPixelBufferGetBytesPerRow(&mask_buffer);

        if base_address.is_null() {
            CVPixelBufferUnlockBaseAddress(&mask_buffer, CVPixelBufferLockFlags::ReadOnly);
            return None;
        }

        let mut pixels = Vec::with_capacity(width * height);
        for y in 0..height {
            let row_ptr = (base_address as *const u8).add(y * bytes_per_row);
            for x in 0..width {
                pixels.push(*row_ptr.add(x));
            }
        }

        CVPixelBufferUnlockBaseAddress(&mask_buffer, CVPixelBufferLockFlags::ReadOnly);

        Some(SegmentationMaskData {
            pixels,
            width: width as u64,
            height: height as u64,
        })
    }
}

impl streamlib::core::ReactiveProcessor for CyberpunkCompositorProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let result = (|| {
            tracing::info!("CyberpunkCompositor: Setting up (reactive mode)...");

            self.gpu_context = Some(ctx.gpu.clone());
            self.start_time = Some(Instant::now());
            self.texture_cache = None; // Deferred to avoid race with camera init

            let metal_device = ctx.gpu.metal_device();
            let metal_device_ref = {
                use metal::foreign_types::ForeignTypeRef;
                let device_ptr = metal_device.device() as *const _ as *mut c_void;
                unsafe { metal::DeviceRef::from_ptr(device_ptr as *mut _) }
            };

            let command_queue = metal_device_ref.new_command_queue();

            // Compile shaders
            let shader_source = include_str!("shaders/cyberpunk_compositor.metal");
            let library = metal_device_ref
                .new_library_with_source(shader_source, &metal::CompileOptions::new())
                .map_err(|e| StreamError::Configuration(format!("Shader compile failed: {}", e)))?;

            let vertex_fn = library
                .get_function("compositor_vertex", None)
                .map_err(|e| StreamError::Configuration(format!("Vertex not found: {}", e)))?;
            let fragment_fn = library
                .get_function("compositor_procedural_fragment", None)
                .map_err(|e| StreamError::Configuration(format!("Fragment not found: {}", e)))?;

            // Render pipeline (procedural background only)
            let pipeline_desc = metal::RenderPipelineDescriptor::new();
            pipeline_desc.set_vertex_function(Some(&vertex_fn));
            pipeline_desc.set_fragment_function(Some(&fragment_fn));
            pipeline_desc
                .color_attachments()
                .object_at(0)
                .unwrap()
                .set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);

            let render_pipeline = metal_device_ref
                .new_render_pipeline_state(&pipeline_desc)
                .map_err(|e| StreamError::Configuration(format!("Pipeline failed: {}", e)))?;

            // Blur compute pipelines
            let blur_h_fn = library
                .get_function("gaussian_blur_horizontal", None)
                .map_err(|e| StreamError::Configuration(format!("Blur H not found: {}", e)))?;
            let blur_h_pipeline = metal_device_ref
                .new_compute_pipeline_state_with_function(&blur_h_fn)
                .map_err(|e| StreamError::Configuration(format!("Blur H failed: {}", e)))?;

            let blur_v_fn = library
                .get_function("gaussian_blur_vertical", None)
                .map_err(|e| StreamError::Configuration(format!("Blur V not found: {}", e)))?;
            let blur_v_pipeline = metal_device_ref
                .new_compute_pipeline_state_with_function(&blur_v_fn)
                .map_err(|e| StreamError::Configuration(format!("Blur V failed: {}", e)))?;

            // Default white mask (1x1)
            let mask_desc = metal::TextureDescriptor::new();
            mask_desc.set_texture_type(metal::MTLTextureType::D2);
            mask_desc.set_pixel_format(metal::MTLPixelFormat::R8Unorm);
            mask_desc.set_width(1);
            mask_desc.set_height(1);
            mask_desc.set_usage(metal::MTLTextureUsage::ShaderRead);
            let default_mask = metal_device_ref.new_texture(&mask_desc);
            default_mask.replace_region(
                metal::MTLRegion {
                    origin: metal::MTLOrigin { x: 0, y: 0, z: 0 },
                    size: metal::MTLSize {
                        width: 1,
                        height: 1,
                        depth: 1,
                    },
                },
                0,
                [255u8].as_ptr() as *const _,
                1,
            );

            // Sampler
            let sampler_desc = metal::SamplerDescriptor::new();
            sampler_desc.set_min_filter(metal::MTLSamplerMinMagFilter::Linear);
            sampler_desc.set_mag_filter(metal::MTLSamplerMinMagFilter::Linear);
            sampler_desc.set_address_mode_s(metal::MTLSamplerAddressMode::ClampToEdge);
            sampler_desc.set_address_mode_t(metal::MTLSamplerAddressMode::ClampToEdge);
            let sampler = metal_device_ref.new_sampler(&sampler_desc);

            // Uniforms buffer
            let uniforms_buffer = metal_device_ref.new_buffer(
                std::mem::size_of::<CompositorUniforms>() as u64,
                metal::MTLResourceOptions::CPUCacheModeDefaultCache,
            );

            self.metal_command_queue = Some(command_queue);
            self.render_pipeline = Some(render_pipeline);
            self.blur_h_pipeline = Some(blur_h_pipeline);
            self.blur_v_pipeline = Some(blur_v_pipeline);
            self.sampler = Some(sampler);
            self.uniforms_buffer = Some(uniforms_buffer);
            self.default_mask_texture = Some(default_mask);

            // Spawn segmentation thread with REUSED request (stateful for temporal smoothing)
            let quality_level = self.config.quality_level;
            let pending_mask = Arc::clone(&self.pending_mask);
            let mask_ready = Arc::clone(&self.mask_ready);
            let segmentation_running = Arc::clone(&self.segmentation_running);
            segmentation_running.store(true, Ordering::Release);

            let (sender, receiver) =
                std::sync::mpsc::sync_channel::<streamlib::core::rhi::RhiPixelBuffer>(1);
            self.segmentation_sender = Some(sender);

            let thread_handle = std::thread::Builder::new()
                .name("vision-segmentation".to_string())
                .spawn(move || {
                    // Set real-time thread priority for consistent segmentation timing
                    set_realtime_thread_priority();

                    // Create ONE request at thread start - reuse for temporal smoothing
                    let request = unsafe { VNGeneratePersonSegmentationRequest::new() };

                    // Set quality level
                    let vn_quality = match quality_level {
                        0 => VNGeneratePersonSegmentationRequestQualityLevel::Fast,
                        1 => VNGeneratePersonSegmentationRequestQualityLevel::Balanced,
                        _ => VNGeneratePersonSegmentationRequestQualityLevel::Accurate,
                    };
                    unsafe { request.setQualityLevel(vn_quality) };

                    // Set output format to 8-bit grayscale for efficiency
                    unsafe { request.setOutputPixelFormat(kCVPixelFormatType_OneComponent8) };

                    tracing::info!(
                        "Vision segmentation thread started (quality={}, format=OneComponent8)",
                        quality_level
                    );

                    while segmentation_running.load(Ordering::Acquire) {
                        let buffer =
                            match receiver.recv_timeout(std::time::Duration::from_millis(100)) {
                                Ok(buf) => buf,
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                            };

                        // Wrap Vision work in autoreleasepool
                        autoreleasepool(|_| {
                            if let Some(mask_data) = run_vision_segmentation(&buffer, &request) {
                                *pending_mask.lock() = Some(mask_data);
                                mask_ready.store(true, Ordering::Release);
                            }
                        });
                    }

                    tracing::info!("Vision segmentation thread exiting");
                })
                .map_err(|e| {
                    StreamError::Runtime(format!("Failed to spawn segmentation thread: {}", e))
                })?;

            self.segmentation_thread = Some(thread_handle);

            tracing::info!("CyberpunkCompositor: Initialized");
            Ok(())
        })();
        std::future::ready(result)
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        self.segmentation_running.store(false, Ordering::Release);
        self.segmentation_sender.take();

        if let Some(handle) = self.segmentation_thread.take() {
            let _ = handle.join();
        }

        tracing::info!(
            "CyberpunkCompositor: Shutdown ({} frames)",
            self.frame_count.load(Ordering::Relaxed)
        );
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
        let Some(frame) = self.video_in.read() else {
            return Ok(());
        };

        let width = frame.width();
        let height = frame.height();
        let timestamp_ns = frame.timestamp_ns;
        let frame_number = frame.frame_number;

        let elapsed = self
            .start_time
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0);

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;

        // Lazy texture cache creation
        if self.texture_cache.is_none() {
            self.texture_cache = Some(gpu_ctx.create_texture_cache()?);
        }
        let texture_cache = self.texture_cache.as_ref().unwrap();

        let input_view = texture_cache.create_view(frame.buffer())?;
        let input_metal = input_view.as_metal_texture();

        let command_queue = self.metal_command_queue.as_ref().unwrap();
        let metal_device = gpu_ctx.metal_device();
        let metal_device_ref = {
            use metal::foreign_types::ForeignTypeRef;
            let device_ptr = metal_device.device() as *const _ as *mut c_void;
            unsafe { metal::DeviceRef::from_ptr(device_ptr as *mut _) }
        };

        // Check for new mask from segmentation thread
        if self.mask_ready.swap(false, Ordering::AcqRel) {
            if let Some(mask_data) = self.pending_mask.lock().take() {
                let needs_create = match self.current_mask_dimensions {
                    Some((w, h)) => w != mask_data.width || h != mask_data.height,
                    None => true,
                };

                if needs_create {
                    let mask_desc = metal::TextureDescriptor::new();
                    mask_desc.set_texture_type(metal::MTLTextureType::D2);
                    mask_desc.set_pixel_format(metal::MTLPixelFormat::R8Unorm);
                    mask_desc.set_width(mask_data.width);
                    mask_desc.set_height(mask_data.height);
                    mask_desc.set_usage(
                        metal::MTLTextureUsage::ShaderRead | metal::MTLTextureUsage::ShaderWrite,
                    );
                    self.current_mask_texture = Some(metal_device_ref.new_texture(&mask_desc));
                    self.blur_temp_texture = Some(metal_device_ref.new_texture(&mask_desc));
                    self.current_mask_dimensions = Some((mask_data.width, mask_data.height));
                }

                if let Some(ref mask_texture) = self.current_mask_texture {
                    mask_texture.replace_region(
                        metal::MTLRegion {
                            origin: metal::MTLOrigin { x: 0, y: 0, z: 0 },
                            size: metal::MTLSize {
                                width: mask_data.width,
                                height: mask_data.height,
                                depth: 1,
                            },
                        },
                        0,
                        mask_data.pixels.as_ptr() as *const _,
                        mask_data.width,
                    );
                }
            }
        }

        // Submit frame to segmentation thread
        if let Some(ref sender) = self.segmentation_sender {
            let _ = sender.try_send(frame.buffer().clone());
        }

        // GPU work: blur + render
        let command_buffer = command_queue.new_command_buffer();

        // Apply blur if we have a mask
        if let Some(ref mask_texture) = self.current_mask_texture {
            if let (Some(ref blur_h), Some(ref blur_v), Some(ref blur_temp)) = (
                &self.blur_h_pipeline,
                &self.blur_v_pipeline,
                &self.blur_temp_texture,
            ) {
                let (mask_w, mask_h) = self.current_mask_dimensions.unwrap_or((1, 1));
                let thread_group_size = metal::MTLSize {
                    width: 16,
                    height: 16,
                    depth: 1,
                };
                let thread_groups = metal::MTLSize {
                    width: mask_w.div_ceil(16),
                    height: mask_h.div_ceil(16),
                    depth: 1,
                };

                let blur_h_enc = command_buffer.new_compute_command_encoder();
                blur_h_enc.set_compute_pipeline_state(blur_h);
                blur_h_enc.set_texture(0, Some(mask_texture));
                blur_h_enc.set_texture(1, Some(blur_temp));
                blur_h_enc.dispatch_thread_groups(thread_groups, thread_group_size);
                blur_h_enc.end_encoding();

                let blur_v_enc = command_buffer.new_compute_command_encoder();
                blur_v_enc.set_compute_pipeline_state(blur_v);
                blur_v_enc.set_texture(0, Some(blur_temp));
                blur_v_enc.set_texture(1, Some(mask_texture));
                blur_v_enc.dispatch_thread_groups(thread_groups, thread_group_size);
                blur_v_enc.end_encoding();
            }
        }

        // Acquire output buffer
        if self.output_pool.is_none() || self.output_pool_dimensions != Some((width, height)) {
            let output_desc = PixelBufferDescriptor::new(width, height, PixelFormat::Bgra32);
            self.output_pool = Some(RhiPixelBufferPool::new_with_descriptor(&output_desc)?);
            self.output_pool_dimensions = Some((width, height));
        }
        let output_buffer = self.output_pool.as_ref().unwrap().acquire()?;
        let output_view = texture_cache.create_view(&output_buffer)?;
        let output_metal = output_view.as_metal_texture();

        // Update uniforms
        unsafe {
            let ptr = self.uniforms_buffer.as_ref().unwrap().contents() as *mut CompositorUniforms;
            (*ptr).time = elapsed;
            (*ptr).mask_threshold = self.config.mask_threshold;
            (*ptr).edge_feather = self.config.edge_feather;
            (*ptr)._padding = 0.0;
        }

        // Render
        let render_pass_desc = metal::RenderPassDescriptor::new();
        let color_attachment = render_pass_desc.color_attachments().object_at(0).unwrap();
        color_attachment.set_texture(Some(output_metal));
        color_attachment.set_load_action(metal::MTLLoadAction::Clear);
        color_attachment.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 1.0));
        color_attachment.set_store_action(metal::MTLStoreAction::Store);

        let render_enc = command_buffer.new_render_command_encoder(render_pass_desc);
        render_enc.set_render_pipeline_state(self.render_pipeline.as_ref().unwrap());
        render_enc.set_fragment_texture(0, Some(input_metal));

        let mask_ref = self
            .current_mask_texture
            .as_ref()
            .map(|t| t.as_ref())
            .unwrap_or_else(|| self.default_mask_texture.as_ref().unwrap());
        render_enc.set_fragment_texture(1, Some(mask_ref));
        render_enc.set_fragment_sampler_state(0, Some(self.sampler.as_ref().unwrap()));
        render_enc.set_fragment_buffer(0, Some(self.uniforms_buffer.as_ref().unwrap()), 0);
        render_enc.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
        render_enc.end_encoding();

        command_buffer.commit();

        // Output frame
        let output_frame = VideoFrame::from_buffer(output_buffer, timestamp_ns, frame_number);
        self.video_out.write(output_frame);

        self.frame_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
