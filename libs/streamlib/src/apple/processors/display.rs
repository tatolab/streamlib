use crate::apple::{display_link::DisplayLink, metal::MetalDevice, WgpuBridge};
use crate::core::{LinkInput, Result, RuntimeContext, StreamError, VideoFrame};
use metal;
use objc2::{rc::Retained, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSBackingStoreType, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_metal::MTLPixelFormat;
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc,
};

// Scaling mode for video content in the display window
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum ScalingMode {
    /// Stretch video to fill window (ignores aspect ratio)
    #[default]
    Stretch,
    /// Scale video to fit window while maintaining aspect ratio (letterbox/pillarbox)
    Letterbox,
    /// Scale video to fill window while maintaining aspect ratio (crops edges)
    Crop,
}

// Apple-specific configuration and types
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppleDisplayConfig {
    pub width: u32,
    pub height: u32,
    pub title: Option<String>,
    pub scaling_mode: ScalingMode,
}

impl Default for AppleDisplayConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            title: None,
            scaling_mode: ScalingMode::default(),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Default)]
pub struct AppleWindowId(pub u64);

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(1);

#[crate::processor(
    execution = Manual,
    description = "Displays video frames in a window using Metal with vsync",
    unsafe_send
)]
pub struct AppleDisplayProcessor {
    #[crate::input(description = "Video frames to display in the window")]
    video: LinkInput<VideoFrame>,

    #[crate::config]
    config: AppleDisplayConfig,

    window: Option<Retained<NSWindow>>,
    #[allow(dead_code)]
    metal_layer: Option<Retained<CAMetalLayer>>,
    layer_addr: Arc<AtomicUsize>,
    metal_device: Option<MetalDevice>,
    metal_command_queue: Option<metal::CommandQueue>,
    gpu_context: Option<crate::core::GpuContext>,
    wgpu_bridge: Option<Arc<WgpuBridge>>,
    window_id: AppleWindowId,
    window_title: String,
    width: u32,
    height: u32,
    frames_rendered: u64,
    window_creation_dispatched: bool,
    display_link: Option<DisplayLink>,
    metal_render_pipeline: Option<metal::RenderPipelineState>,
    metal_sampler: Option<metal::SamplerState>,
    format_buffer_rgba: Option<metal::Buffer>,
    format_buffer_bgra: Option<metal::Buffer>,
}

impl AppleDisplayProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
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

        tracing::trace!("Display {}: Creating MetalDevice...", self.window_id.0);
        let metal_device = MetalDevice::new()?;
        tracing::trace!("Display {}: MetalDevice created", self.window_id.0);

        // Create metal crate command queue from objc2 Metal device
        tracing::trace!(
            "Display {}: Creating Metal command queue...",
            self.window_id.0
        );
        let metal_command_queue = {
            use metal::foreign_types::ForeignTypeRef;
            let device_ptr = metal_device.device() as *const _ as *mut std::ffi::c_void;
            let metal_device_ref = unsafe { metal::DeviceRef::from_ptr(device_ptr as *mut _) };
            metal_device_ref.new_command_queue()
        };
        tracing::trace!("Display {}: Metal command queue created", self.window_id.0);

        // Create wgpu bridge from shared device
        tracing::trace!("Display {}: Creating WgpuBridge...", self.window_id.0);
        let wgpu_bridge = Arc::new(WgpuBridge::from_shared_device(
            metal_device.clone_device(),
            ctx.gpu.device().as_ref().clone(),
            ctx.gpu.queue().as_ref().clone(),
        ));
        tracing::trace!("Display {}: WgpuBridge created", self.window_id.0);

        self.wgpu_bridge = Some(wgpu_bridge);
        self.metal_command_queue = Some(metal_command_queue);

        // Initialize CVDisplayLink for vsync
        tracing::trace!("Display {}: Creating CVDisplayLink...", self.window_id.0);
        let display_link = DisplayLink::new()?;
        tracing::trace!("Display {}: Starting CVDisplayLink...", self.window_id.0);
        display_link.start()?;
        tracing::trace!("Display {}: CVDisplayLink started", self.window_id.0);

        if let Ok(period) = display_link.get_nominal_output_video_refresh_period() {
            tracing::info!(
                "Display {}: Vsync enabled (refresh period: {:?})",
                self.window_title,
                period
            );
        } else {
            tracing::info!("Display {}: Vsync enabled", self.window_title);
        }

        self.display_link = Some(display_link);

        // Create Metal render pipeline for scaling
        use metal::foreign_types::ForeignTypeRef;
        let device_ptr = metal_device.device() as *const _ as *mut std::ffi::c_void;
        let metal_device_ref = unsafe { metal::DeviceRef::from_ptr(device_ptr as *mut _) };

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
        self.metal_device = Some(metal_device);

        tracing::info!(
            "Display {}: Initialized ({}x{}) with Metal render pipeline",
            self.window_title,
            self.width,
            self.height
        );

        // EAGER WINDOW CREATION: Create window immediately instead of waiting for first frame
        self.initialize_window()?;

        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            "Display {}: Stopping (rendered {} frames)",
            self.window_title,
            self.frames_rendered
        );
        Ok(())
    }

    // Business logic - called once by macro-generated process() in Pull mode
    // Sets up vsync-driven rendering loop
    fn process(&mut self) -> Result<()> {
        tracing::trace!(
            "Display {}: process() called - entering processor main function",
            self.window_id.0
        );

        // Pull mode: process() is called once to set up the loop
        // Loop continuously, waiting for vsync to drive frame presentation - with shutdown awareness
        use crate::core::{shutdown_aware_loop, LoopControl};

        tracing::trace!(
            "Display {}: Entering shutdown_aware_loop for vsync rendering",
            self.window_id.0
        );
        let mut loop_iteration = 0u64;
        let mut frames_received = 0u64;

        shutdown_aware_loop(|| {
            loop_iteration += 1;
            if loop_iteration == 1 {
                tracing::trace!(
                    "Display {}: First iteration of vsync loop",
                    self.window_id.0
                );
            } else if loop_iteration.is_multiple_of(1000) {
                tracing::trace!(
                    "Display {}: Vsync loop iteration #{}, frames_received={}",
                    self.window_id.0,
                    loop_iteration,
                    frames_received
                );
            }

            // Wait for vsync signal from CVDisplayLink
            if let Some(ref display_link) = self.display_link {
                if loop_iteration == 1 {
                    tracing::trace!(
                        "Display {}: Waiting for first vsync signal...",
                        self.window_id.0
                    );
                }
                display_link.wait_for_frame();
                if loop_iteration == 1 {
                    tracing::trace!("Display {}: First vsync signal received!", self.window_id.0);
                }
            } else {
                tracing::warn!("Display {}: No display_link available!", self.window_id.0);
            }

            // Render the latest available frame (if any)
            if let Some(frame) = self.video.read() {
                frames_received += 1;
                if frames_received == 1 {
                    tracing::trace!(
                        "Display {}: Received FIRST frame from input link!",
                        self.window_id.0
                    );
                } else if frames_received.is_multiple_of(60) {
                    tracing::trace!(
                        "Display {}: Received frame #{} from input",
                        self.window_id.0,
                        frames_received
                    );
                }
                self.render_frame(frame)?;
            } else if loop_iteration.is_multiple_of(1000) {
                tracing::trace!(
                    "Display {}: No frame available at iteration #{}",
                    self.window_id.0,
                    loop_iteration
                );
            }

            Ok(LoopControl::Continue)
        })
    }

    // Helper methods
    pub fn window_id(&self) -> AppleWindowId {
        self.window_id
    }

    pub fn set_window_title(&mut self, title: &str) {
        self.window_title = title.to_string();
        if let Some(window) = &self.window {
            let title_string = NSString::from_str(title);
            window.setTitle(&title_string);
        }
    }

    fn render_frame(&mut self, frame: VideoFrame) -> Result<()> {
        if self.frames_rendered == 0 {
            tracing::trace!(
                "Display {}: render_frame() called for FIRST frame",
                self.window_id.0
            );
        }

        let layer_addr = self.layer_addr.load(Ordering::Acquire);
        if layer_addr == 0 {
            if self.frames_rendered == 0 {
                tracing::trace!(
                    "Display {}: layer_addr is 0, window not ready yet",
                    self.window_id.0
                );
            }
            return Ok(());
        }

        if self.frames_rendered == 0 {
            tracing::trace!(
                "Display {}: layer_addr={:#x}, proceeding to render",
                self.window_id.0,
                layer_addr
            );
        }

        // SAFETY: Layer was created on main thread and address stored atomically
        let metal_layer = unsafe {
            let ptr = layer_addr as *const CAMetalLayer;
            &*ptr
        };

        let metal_command_queue = self.metal_command_queue.as_ref().ok_or_else(|| {
            StreamError::Configuration("Metal command queue not initialized".into())
        })?;

        let wgpu_bridge = self
            .wgpu_bridge
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("WgpuBridge not initialized".into()))?;

        let render_pipeline = self.metal_render_pipeline.as_ref().ok_or_else(|| {
            StreamError::Configuration("Metal render pipeline not initialized".into())
        })?;

        let sampler = self
            .metal_sampler
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("Metal sampler not initialized".into()))?;

        unsafe {
            if let Some(drawable) = metal_layer.nextDrawable() {
                let drawable_texture = drawable.texture();

                // Get Metal texture from wgpu VideoFrame
                let source_metal = wgpu_bridge.unwrap_to_metal_texture(&frame.texture)?;

                // Create command buffer and render pass
                let command_buffer = metal_command_queue.new_command_buffer();

                let render_pass_descriptor = metal::RenderPassDescriptor::new();
                let color_attachment = render_pass_descriptor
                    .color_attachments()
                    .object_at(0)
                    .unwrap();

                // Convert objc2 texture to metal-rs texture reference
                use metal::foreign_types::ForeignTypeRef;
                let drawable_texture_ptr = &*drawable_texture as *const _ as *mut std::ffi::c_void;
                let drawable_texture_ref =
                    metal::TextureRef::from_ptr(drawable_texture_ptr as *mut _);

                color_attachment.set_texture(Some(drawable_texture_ref));
                color_attachment.set_load_action(metal::MTLLoadAction::Clear);
                color_attachment.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 1.0));
                color_attachment.set_store_action(metal::MTLStoreAction::Store);

                let render_encoder =
                    command_buffer.new_render_command_encoder(render_pass_descriptor);

                // Set pipeline and resources
                render_encoder.set_render_pipeline_state(render_pipeline);
                render_encoder.set_fragment_texture(0, Some(&source_metal));
                render_encoder.set_fragment_sampler_state(0, Some(sampler));

                // Set format buffer based on VideoFrame format
                let format_buffer = if frame.format == wgpu::TextureFormat::Rgba8Unorm {
                    self.format_buffer_rgba.as_ref().unwrap()
                } else {
                    self.format_buffer_bgra.as_ref().unwrap()
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

                self.frames_rendered += 1;

                if self.frames_rendered.is_multiple_of(60) {
                    tracing::info!(
                        "Display {}: Rendered frame {} ({}x{} → {}x{}) via Metal scaled render",
                        self.window_id.0,
                        frame.frame_number,
                        frame.width,
                        frame.height,
                        self.width,
                        self.height,
                    );
                }
            }
        }

        Ok(())
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

        match self.config.scaling_mode {
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
        let metal_device = self
            .metal_device
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("Metal device not initialized".into()))?
            .clone_device();
        let window_id = self.window_id;
        let layer_addr = Arc::clone(&self.layer_addr);

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
            eprintln!(
                "[TRACE] Display {}: CAMetalLayer configured, attaching to window...",
                window_id.0
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

crate::register_processor_type!(AppleDisplayProcessor::Processor);
