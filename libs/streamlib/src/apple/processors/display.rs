// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::rhi::{PixelFormat, RhiTextureCache};
use crate::core::{Result, RuntimeContext, StreamError};
use crossbeam_channel::{Receiver, Sender};
use metal;
use objc2::{rc::Retained, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSBackingStoreType, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_metal::MTLPixelFormat;
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc,
};
use std::thread::JoinHandle;
use std::time::Duration;

// Re-export ScalingMode from generated config for external use
pub type ScalingMode = crate::_generated_::com_tatolab_display_config::ScalingMode;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Default)]
pub struct AppleWindowId(pub u64);

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(1);

#[crate::processor("com.tatolab.display")]
pub struct AppleDisplayProcessor {
    /// Window address stored as usize (NSWindow is !Send, but we leak it anyway)
    window_addr: AtomicUsize,
    /// Metal layer address stored as usize for sharing with render thread
    layer_addr: Arc<AtomicUsize>,
    gpu_context: Option<crate::core::GpuContext>,
    window_id: AppleWindowId,
    window_title: String,
    width: u32,
    height: u32,
    window_creation_dispatched: bool,
    metal_render_pipeline: Option<metal::RenderPipelineState>,
    metal_sampler: Option<metal::SamplerState>,
    format_buffer_rgba: Option<metal::Buffer>,
    format_buffer_bgra: Option<metal::Buffer>,
    /// Flag to signal render thread to stop
    running: Arc<AtomicBool>,
    /// Handle to render thread (for join on stop)
    render_thread: Option<JoinHandle<()>>,
    /// Handle to poller thread (receives from inputs, sends to channel)
    poller_thread: Option<JoinHandle<()>>,
    /// Channel sender for passing frames from poller to render thread
    frame_sender: Option<Sender<crate::_generated_::Videoframe>>,
    /// Channel receiver for render thread to receive frames
    frame_receiver: Option<Receiver<crate::_generated_::Videoframe>>,
}

impl crate::core::ManualProcessor for AppleDisplayProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let result = (|| {
            tracing::trace!("Display: setup() called");
            self.gpu_context = Some(ctx.gpu.clone());
            self.window_id = AppleWindowId(NEXT_WINDOW_ID.fetch_add(1, Ordering::SeqCst));
            self.width = self.config.width;
            self.height = self.config.height;
            self.window_title = self
                .config
                .title
                .clone()
                .unwrap_or_else(|| "streamlib Display".to_string());

            // Initialize state for game loop rendering
            self.running = Arc::new(AtomicBool::new(false));

            // Create bounded channel for passing frames from poller to render thread
            // Capacity of 2 allows for one frame being rendered and one queued
            let (sender, receiver) = crossbeam_channel::bounded(2);
            self.frame_sender = Some(sender);
            self.frame_receiver = Some(receiver);

            tracing::info!(
                "Display {}: Game loop mode (vsync={}, drawable_count={})",
                self.window_id.0,
                self.config.vsync.unwrap_or(true),
                self.config.drawable_count.unwrap_or(2)
            );

            // Use shared Metal device from GpuContext
            let metal_device_ref = ctx.gpu.device().metal_device_ref();

            // Compile Metal shader
            let shader_source = include_str!("shaders/fullscreen.metal");
            let library = metal_device_ref
                .new_library_with_source(shader_source, &metal::CompileOptions::new())
                .map_err(|e| {
                    StreamError::Configuration(format!("Failed to compile Metal shader: {}", e))
                })?;

            let vertex_function = library.get_function("vertex_main", None).map_err(|e| {
                StreamError::Configuration(format!("vertex_main function not found: {}", e))
            })?;
            let fragment_function = library.get_function("fragment_main", None).map_err(|e| {
                StreamError::Configuration(format!("fragment_main function not found: {}", e))
            })?;

            // Create render pipeline descriptor
            let pipeline_descriptor = metal::RenderPipelineDescriptor::new();
            pipeline_descriptor.set_vertex_function(Some(&vertex_function));
            pipeline_descriptor.set_fragment_function(Some(&fragment_function));

            let color_attachment = pipeline_descriptor
                .color_attachments()
                .object_at(0)
                .unwrap();
            color_attachment.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);

            // Create pipeline state
            let pipeline_state = metal_device_ref
                .new_render_pipeline_state(&pipeline_descriptor)
                .map_err(|e| {
                    StreamError::Configuration(format!("Failed to create render pipeline: {}", e))
                })?;

            // Create sampler for texture sampling with linear filtering
            let sampler_descriptor = metal::SamplerDescriptor::new();
            sampler_descriptor.set_min_filter(metal::MTLSamplerMinMagFilter::Linear);
            sampler_descriptor.set_mag_filter(metal::MTLSamplerMinMagFilter::Linear);
            sampler_descriptor.set_address_mode_s(metal::MTLSamplerAddressMode::ClampToEdge);
            sampler_descriptor.set_address_mode_t(metal::MTLSamplerAddressMode::ClampToEdge);

            let sampler_state = metal_device_ref.new_sampler(&sampler_descriptor);

            // Create format flag buffers (0 = BGRA, 1 = RGBA)
            let format_buffer_rgba = metal_device_ref.new_buffer(
                std::mem::size_of::<i32>() as u64,
                metal::MTLResourceOptions::CPUCacheModeDefaultCache,
            );
            unsafe {
                let ptr = format_buffer_rgba.contents() as *mut i32;
                *ptr = 1; // RGBA flag
            }

            let format_buffer_bgra = metal_device_ref.new_buffer(
                std::mem::size_of::<i32>() as u64,
                metal::MTLResourceOptions::CPUCacheModeDefaultCache,
            );
            unsafe {
                let ptr = format_buffer_bgra.contents() as *mut i32;
                *ptr = 0; // BGRA flag
            }

            self.metal_render_pipeline = Some(pipeline_state);
            self.metal_sampler = Some(sampler_state);
            self.format_buffer_rgba = Some(format_buffer_rgba);
            self.format_buffer_bgra = Some(format_buffer_bgra);

            tracing::info!(
                "Display {}: Initialized ({}x{}) with Metal render pipeline",
                self.window_title,
                self.width,
                self.height
            );

            // EAGER WINDOW CREATION: Create window immediately instead of waiting for first frame
            self.initialize_window()?;

            Ok(())
        })();
        std::future::ready(result)
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!("Display {}: Teardown", self.window_title);
        std::future::ready(Ok(()))
    }

    // Game loop start - spawns a dedicated render thread that runs at native display refresh rate
    fn start(&mut self) -> Result<()> {
        tracing::trace!(
            "Display {}: start() called - spawning game loop render thread",
            self.window_id.0
        );

        // Move inputs to render thread (InputMailboxes is Send, subscriber stays with owner thread)
        // After this, self.inputs is replaced with an empty default instance
        let inputs = std::mem::take(&mut self.inputs);
        let layer_addr = Arc::clone(&self.layer_addr);
        let running = Arc::clone(&self.running);
        let window_id = self.window_id.0;

        // Clone gpu context for creating cache texture in render thread
        let gpu_context = self
            .gpu_context
            .clone()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;

        // Clone shared command queue from GpuContext for render thread
        let command_queue = gpu_context.command_queue().clone();

        let render_pipeline = self
            .metal_render_pipeline
            .as_ref()
            .ok_or_else(|| {
                StreamError::Configuration("Metal render pipeline not initialized".into())
            })?
            .clone();

        let sampler = self
            .metal_sampler
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("Metal sampler not initialized".into()))?
            .clone();

        let format_buffer_rgba = self
            .format_buffer_rgba
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("Format buffer RGBA not initialized".into()))?
            .clone();

        let format_buffer_bgra = self
            .format_buffer_bgra
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("Format buffer BGRA not initialized".into()))?
            .clone();

        // Signal that the render thread should run
        running.store(true, Ordering::Release);

        // Spawn dedicated render thread with game loop
        let render_thread = std::thread::Builder::new()
            .name(format!("display-{}-render", window_id))
            .spawn(move || {
                tracing::debug!("Display {}: Render thread started", window_id);

                // Create texture cache for converting buffer-backed frames to textures
                let texture_cache: Option<RhiTextureCache> = match gpu_context.create_texture_cache() {
                    Ok(cache) => Some(cache),
                    Err(e) => {
                        tracing::warn!(
                            "Display {}: Failed to create texture cache, buffer frames won't work: {}",
                            window_id,
                            e
                        );
                        None
                    }
                };

                // Create render pass descriptor (reused each frame, only texture attachment updated)
                let render_pass_descriptor = metal::RenderPassDescriptor::new().to_owned();
                {
                    let color_attachment = render_pass_descriptor.color_attachments().object_at(0).unwrap();
                    color_attachment.set_load_action(metal::MTLLoadAction::Clear);
                    color_attachment.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 1.0));
                    color_attachment.set_store_action(metal::MTLStoreAction::Store);
                }

                while running.load(Ordering::Acquire) {
                    // Get layer address (window may not be ready yet)
                    let addr = layer_addr.load(Ordering::Acquire);
                    if addr == 0 {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }

                    // SAFETY: Layer was created on main thread and address stored atomically
                    let metal_layer = unsafe {
                        let ptr = addr as *const CAMetalLayer;
                        &*ptr
                    };

                    // Read IPC frame from inputs and convert to VideoFrame
                    // Check if data available, then read
                    if !inputs.has_data("video") {
                        // No frame - sleep briefly and check again
                        // Don't call nextDrawable() as that blocks for vsync
                        std::thread::sleep(Duration::from_micros(500));
                        continue;
                    }

                    let ipc_frame: crate::_generated_::Videoframe = match inputs.read("video") {
                        Ok(frame) => frame,
                        Err(e) => {
                            tracing::warn!("Display {}: Failed to read frame: {}", window_id, e);
                            continue;
                        }
                    };

                    // Resolve buffer from surface_id using GpuContext
                    let buffer = match gpu_context.resolve_videoframe_buffer(&ipc_frame) {
                        Ok(buf) => buf,
                        Err(e) => {
                            tracing::warn!(
                                "Display {}: Failed to resolve buffer for '{}': {}",
                                window_id,
                                ipc_frame.surface_id,
                                e
                            );
                            continue;
                        }
                    };

                    // Now get drawable - this blocks for vsync when displaySyncEnabled=true
                    let Some(drawable) = metal_layer.nextDrawable() else {
                        continue;
                    };

                    // Get Metal texture from buffer via texture cache
                    let Some(ref cache) = texture_cache else {
                        tracing::warn!("Display {}: No texture cache available", window_id);
                        continue;
                    };
                    let texture_view = match cache.create_view(&buffer) {
                        Ok(view) => view,
                        Err(e) => {
                            tracing::warn!("Display {}: Failed to create texture view: {}", window_id, e);
                            continue;
                        }
                    };
                    let is_rgba_format = buffer.format() == PixelFormat::Rgba32;
                    let source_metal = texture_view.as_metal_texture();

                    // Render frame directly to drawable
                    unsafe {
                        let drawable_texture = drawable.texture();

                        // Create command buffer from shared queue
                        let command_buffer = command_queue.metal_queue_ref().new_command_buffer();

                        // Convert objc2 texture to metal-rs texture reference
                        use metal::foreign_types::ForeignTypeRef;
                        let drawable_texture_ptr =
                            &*drawable_texture as *const _ as *mut std::ffi::c_void;
                        let drawable_texture_ref =
                            metal::TextureRef::from_ptr(drawable_texture_ptr as *mut _);

                        // Update cached descriptor's texture attachment
                        render_pass_descriptor
                            .color_attachments()
                            .object_at(0)
                            .unwrap()
                            .set_texture(Some(drawable_texture_ref));

                        let render_encoder =
                            command_buffer.new_render_command_encoder(&render_pass_descriptor);

                        // Set pipeline and resources
                        render_encoder.set_render_pipeline_state(&render_pipeline);
                        render_encoder.set_fragment_texture(0, Some(source_metal));
                        render_encoder.set_fragment_sampler_state(0, Some(&sampler));

                        // Set format buffer based on frame format
                        let format_buffer = if is_rgba_format {
                            &format_buffer_rgba
                        } else {
                            &format_buffer_bgra
                        };
                        render_encoder.set_fragment_buffer(0, Some(format_buffer), 0);

                        // Draw fullscreen triangle (3 vertices, no vertex buffer needed)
                        render_encoder.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);

                        render_encoder.end_encoding();

                        // Present and commit
                        let drawable_ptr = &*drawable as *const _ as *mut std::ffi::c_void;
                        let drawable_ref = metal::DrawableRef::from_ptr(drawable_ptr as *mut _);

                        command_buffer.present_drawable(drawable_ref);
                        command_buffer.commit();

                        // Wait for GPU to finish reading the source texture before releasing the buffer.
                        // Without this, the camera can reacquire and overwrite the buffer while
                        // the GPU is still reading from it, causing strobe/corruption artifacts.
                        command_buffer.wait_until_completed();
                    }
                }

                tracing::debug!("Display {}: Render thread exiting", window_id);
            })
            .map_err(|e| StreamError::Runtime(format!("Failed to spawn render thread: {}", e)))?;

        self.render_thread = Some(render_thread);

        tracing::info!("Display {}: Game loop rendering started", self.window_id.0);

        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        tracing::trace!("Display {}: stop() called", self.window_id.0);

        // Signal render thread to stop
        self.running.store(false, Ordering::Release);

        // Wait for render thread to finish
        if let Some(handle) = self.render_thread.take() {
            handle
                .join()
                .map_err(|_| StreamError::Runtime("Render thread panicked".into()))?;
        }

        tracing::info!("Display {}: Stopped", self.window_id.0);

        Ok(())
    }
}

impl AppleDisplayProcessor::Processor {
    // Helper methods
    pub fn window_id(&self) -> AppleWindowId {
        self.window_id
    }

    pub fn set_window_title(&mut self, title: &str) {
        self.window_title = title.to_string();
        let window_addr = self.window_addr.load(Ordering::Acquire);
        if window_addr != 0 {
            // SAFETY: Window was created on main thread and address stored atomically
            // Title change must be dispatched to main thread
            let title_owned = title.to_string();
            use dispatch2::DispatchQueue;
            DispatchQueue::main().exec_async(move || unsafe {
                let window = &*(window_addr as *const NSWindow);
                let title_string = NSString::from_str(&title_owned);
                window.setTitle(&title_string);
            });
        }
    }

    /// Compute destination rectangle for scaled blit based on scaling mode
    #[allow(dead_code)]
    fn compute_destination_rect(
        &self,
        src_width: u32,
        src_height: u32,
        dst_width: u32,
        dst_height: u32,
    ) -> (metal::MTLOrigin, metal::MTLSize) {
        use metal::{MTLOrigin, MTLSize};

        match self
            .config
            .scaling_mode
            .clone()
            .unwrap_or(ScalingMode::Letterbox)
        {
            ScalingMode::Stretch => {
                // Stretch to fill entire window (ignore aspect ratio)
                (
                    MTLOrigin { x: 0, y: 0, z: 0 },
                    MTLSize {
                        width: dst_width as u64,
                        height: dst_height as u64,
                        depth: 1,
                    },
                )
            }
            ScalingMode::Letterbox => {
                // Maintain aspect ratio, add letterbox/pillarbox bars
                let src_aspect = src_width as f64 / src_height as f64;
                let dst_aspect = dst_width as f64 / dst_height as f64;

                let (scaled_width, scaled_height) = if src_aspect > dst_aspect {
                    // Source is wider - fit to width, add letterbox bars (top/bottom)
                    let scaled_height = (dst_width as f64 / src_aspect) as u64;
                    (dst_width as u64, scaled_height)
                } else {
                    // Source is taller - fit to height, add pillarbox bars (left/right)
                    let scaled_width = (dst_height as f64 * src_aspect) as u64;
                    (scaled_width, dst_height as u64)
                };

                // Center in window
                let x_offset = (dst_width as u64 - scaled_width) / 2;
                let y_offset = (dst_height as u64 - scaled_height) / 2;

                (
                    MTLOrigin {
                        x: x_offset,
                        y: y_offset,
                        z: 0,
                    },
                    MTLSize {
                        width: scaled_width,
                        height: scaled_height,
                        depth: 1,
                    },
                )
            }
            ScalingMode::Crop => {
                // Maintain aspect ratio, crop edges to fill window
                let src_aspect = src_width as f64 / src_height as f64;
                let dst_aspect = dst_width as f64 / dst_height as f64;

                let (scaled_width, scaled_height) = if src_aspect > dst_aspect {
                    // Source is wider - fit to height, crop left/right
                    let scaled_width = (dst_height as f64 * src_aspect) as u64;
                    (scaled_width, dst_height as u64)
                } else {
                    // Source is taller - fit to width, crop top/bottom
                    let scaled_height = (dst_width as f64 / src_aspect) as u64;
                    (dst_width as u64, scaled_height)
                };

                // Center (will be clipped)
                let x_offset = (dst_width as u64).saturating_sub(scaled_width) / 2;
                let y_offset = (dst_height as u64).saturating_sub(scaled_height) / 2;

                (
                    MTLOrigin {
                        x: x_offset,
                        y: y_offset,
                        z: 0,
                    },
                    MTLSize {
                        width: scaled_width,
                        height: scaled_height,
                        depth: 1,
                    },
                )
            }
        }
    }

    fn initialize_window(&mut self) -> Result<()> {
        tracing::trace!("Display {}: initialize_window() called", self.window_id.0);
        let width = self.width;
        let height = self.height;
        let window_title = self.window_title.clone();

        // Get objc2 device handle from shared GpuContext for CAMetalLayer
        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;
        let metal_device = gpu_ctx.metal_device().clone_device();
        let window_id = self.window_id;
        let layer_addr = Arc::clone(&self.layer_addr);
        let vsync = self.config.vsync.unwrap_or(true);
        let drawable_count = self.config.drawable_count.unwrap_or(2);

        use dispatch2::DispatchQueue;

        tracing::trace!(
            "Display {}: Dispatching window creation to main queue...",
            window_id.0
        );
        DispatchQueue::main().exec_async(move || {
            // NOTE: Cannot use tracing reliably in dispatch queue - use eprintln
            eprintln!(
                "[TRACE] Display {}: Main thread dispatch EXECUTING - creating window",
                window_id.0
            );
            // SAFETY: This closure executes on the main thread via GCD
            let mtm = unsafe { MainThreadMarker::new_unchecked() };

            eprintln!("[TRACE] Display {}: Creating NSWindow...", window_id.0);

            let frame = NSRect::new(
                NSPoint::new(100.0, 100.0),
                NSSize::new(width as f64, height as f64),
            );

            let style_mask = NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable
                | NSWindowStyleMask::Resizable;

            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    mtm.alloc(),
                    frame,
                    style_mask,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            eprintln!(
                "[TRACE] Display {}: NSWindow created, setting title...",
                window_id.0
            );

            window.setTitle(&NSString::from_str(&window_title));

            eprintln!("[TRACE] Display {}: Creating CAMetalLayer...", window_id.0);
            let metal_layer = CAMetalLayer::new();
            metal_layer.setDevice(Some(&metal_device));
            metal_layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);

            // Configure layer properties using native objc2 methods
            unsafe {
                use objc2::{msg_send, Encode, Encoding};

                #[repr(C)]
                struct CGSize {
                    width: f64,
                    height: f64,
                }

                unsafe impl Encode for CGSize {
                    const ENCODING: Encoding =
                        Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
                }

                let size = CGSize {
                    width: width as f64,
                    height: height as f64,
                };

                let _: () = msg_send![&metal_layer, setDrawableSize: size];
            }

            // Use native objc2 methods for proper type conversion
            // displaySyncEnabled controls vsync - when true, nextDrawable() blocks for vsync
            metal_layer.setDisplaySyncEnabled(vsync);

            // maximumDrawableCount controls buffer count (2 = double, 3 = triple)
            metal_layer.setMaximumDrawableCount(drawable_count as usize);

            // Verify settings were applied
            let actual_vsync = metal_layer.displaySyncEnabled();
            let actual_drawables = metal_layer.maximumDrawableCount();
            eprintln!(
                "[TRACE] Display {}: CAMetalLayer configured (vsync={} -> {}, drawables={} -> {}), attaching to window...",
                window_id.0,
                vsync,
                actual_vsync,
                drawable_count,
                actual_drawables
            );

            if let Some(content_view) = window.contentView() {
                content_view.setWantsLayer(true);
                content_view.setLayer(Some(&metal_layer));
                eprintln!(
                    "[TRACE] Display {}: Metal layer attached to content view",
                    window_id.0
                );
            } else {
                eprintln!(
                    "[TRACE] Display {}: WARNING - no content view!",
                    window_id.0
                );
            }

            eprintln!(
                "[TRACE] Display {}: Making window key and ordering front...",
                window_id.0
            );
            window.makeKeyAndOrderFront(None);

            let app = NSApplication::sharedApplication(mtm);
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
            eprintln!(
                "[TRACE] Display {}: App activated, window should be visible",
                window_id.0
            );

            let _ = Retained::into_raw(window); // Leak window
            let addr = Retained::into_raw(metal_layer) as usize;
            layer_addr.store(addr, Ordering::Release);
            eprintln!(
                "[TRACE] Display {}: Window creation complete, layer_addr stored",
                window_id.0
            );
        });

        self.window_creation_dispatched = true;
        tracing::info!(
            "Display {}: Window creation dispatched, processor ready",
            self.window_id.0
        );

        Ok(())
    }
}
