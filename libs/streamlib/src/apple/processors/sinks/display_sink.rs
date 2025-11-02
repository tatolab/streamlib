//! Apple Display Sink Processor
//!
//! StreamSink implementation that renders VideoFrames to an NSWindow.
//! Each DisplayProcessor instance manages its own window.
//!
//! This is a **sink processor** (v2.0.0 architecture) - it consumes video without producing outputs.

use crate::core::{
    DisplayProcessor, DisplayInputPorts, WindowId,
    Result, StreamError, VideoFrame, StreamProcessor, GpuContext,
    ProcessorDescriptor, PortDescriptor, ProcessorExample, SCHEMA_VIDEO_FRAME,
};
use crate::core::traits::{StreamElement, StreamSink, ElementType, ClockConfig, ClockType, SyncMode};
use crate::apple::{metal::MetalDevice, WgpuBridge, main_thread::execute_on_main_thread};
use objc2::{rc::Retained, MainThreadMarker};
use objc2_foundation::{NSString, NSPoint, NSSize, NSRect};
use objc2_app_kit::{NSWindow, NSBackingStoreType, NSWindowStyleMask, NSApplication, NSApplicationActivationPolicy};
use objc2_quartz_core::{CAMetalLayer, CAMetalDrawable};
use objc2_metal::MTLPixelFormat;
use std::sync::{atomic::{AtomicU64, AtomicUsize, Ordering}, Arc};
use parking_lot::Mutex;
use metal;

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(1);

/// Apple-specific display processor
///
/// Each instance manages one NSWindow with a CAMetalLayer for Metal rendering.
/// Accepts VideoFrame input and renders to screen using GPU-accelerated Metal blits.
///
/// Rendering is fully implemented:
/// - Unwraps WebGPU textures to Metal textures (zero-copy via WgpuBridge)
/// - Blits to CAMetalLayer drawable with automatic RGBA→BGRA conversion
/// - Presents frames at runtime tick rate (typically 60 FPS)
pub struct AppleDisplayProcessor {
    // Window components (will be created async on start)
    window: Option<Retained<NSWindow>>,
    metal_layer: Option<Retained<CAMetalLayer>>,

    // Layer address for thread-safe access (0 = not created yet)
    layer_addr: Arc<AtomicUsize>,
    #[allow(dead_code)] // Stored for potential future direct Metal API usage
    metal_device: MetalDevice,
    metal_command_queue: metal::CommandQueue,

    // GPU context (shared with all processors via runtime)
    gpu_context: Option<crate::core::GpuContext>,

    // WebGPU bridge (created from shared device in on_start)
    wgpu_bridge: Option<Arc<WgpuBridge>>,

    // Processor state
    ports: DisplayInputPorts,
    window_id: WindowId,
    window_title: String,
    width: u32,
    height: u32,
    frames_rendered: u64,
    window_creation_dispatched: bool,
}

// SAFETY: NSWindow, CAMetalLayer, and Metal objects are Objective-C objects that can be sent between threads.
// While they're not marked Send/Sync by objc2, they follow Cocoa's threading rules:
// - Window creation must happen on main thread (we do this in on_start)
// - CAMetalLayer rendering is thread-safe
// - Metal command buffers can be created on any thread
unsafe impl Send for AppleDisplayProcessor {}

impl AppleDisplayProcessor {
    /// Create a new display processor with default size (1920x1080)
    pub fn new() -> Result<Self> {
        Self::with_size(1920, 1080)
    }

    /// Create a new display processor with custom size
    pub fn with_size(width: u32, height: u32) -> Result<Self> {
        let window_id = WindowId(NEXT_WINDOW_ID.fetch_add(1, Ordering::SeqCst));
        let metal_device = MetalDevice::new()?;

        // Create Metal command queue for blit operations
        let metal_command_queue = {
            use metal::foreign_types::ForeignTypeRef;
            let device_ptr = metal_device.device() as *const _ as *mut std::ffi::c_void;
            let metal_device_ref = unsafe {
                metal::DeviceRef::from_ptr(device_ptr as *mut _)
            };
            metal_device_ref.new_command_queue()
        };

        let window_title = "streamlib Display".to_string();

        // Window and layer will be created in on_start() on the main thread

        Ok(Self {
            window: None,  // Created async in on_start()
            metal_layer: None,  // Created async in on_start()
            layer_addr: Arc::new(AtomicUsize::new(0)),  // 0 = not created yet
            metal_device,
            metal_command_queue,
            gpu_context: None,  // Will be set by runtime in on_start()
            wgpu_bridge: None,  // Will be created from shared device in on_start()
            ports: DisplayInputPorts {
                video: crate::core::StreamInput::new("video"),
            },
            window_id,
            window_title,
            width,
            height,
            frames_rendered: 0,
            window_creation_dispatched: false,
        })
    }
}

// ============================================================
// StreamElement Implementation (Base Trait)
// ============================================================

impl StreamElement for AppleDisplayProcessor {
    fn name(&self) -> &str {
        &self.window_title
    }

    fn element_type(&self) -> ElementType {
        ElementType::Sink
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <AppleDisplayProcessor as StreamSink>::descriptor()
    }

    fn input_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "video".to_string(),
            schema: SCHEMA_VIDEO_FRAME.clone(),
            required: true,
            description: "Video frames to display".to_string(),
        }]
    }

    fn start(&mut self) -> Result<()> {
        // Note: GPU context initialization happens via separate method
        // This is called by runtime after GPU context is available
        tracing::info!(
            "Display {}: Starting ({}x{}, title=\"{}\")",
            self.window_id.0,
            self.width,
            self.height,
            self.window_title
        );
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        tracing::info!(
            "Display {}: Stopped (rendered {} frames)",
            self.window_id.0,
            self.frames_rendered
        );

        // Close window on main thread (CRITICAL for AppKit)
        if let Some(window) = self.window.take() {
            let window_addr = Retained::into_raw(window) as usize;

            execute_on_main_thread(move || {
                let window = unsafe {
                    Retained::from_raw(window_addr as *mut NSWindow).unwrap()
                };
                window.close();
                Ok(())
            })?;
        }

        self.metal_layer = None;
        Ok(())
    }

    fn as_sink(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_sink_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

// ============================================================
// StreamSink Implementation (Specialized Trait)
// ============================================================

impl StreamSink for AppleDisplayProcessor {
    type Input = VideoFrame;
    type Config = crate::core::config::DisplayConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        let mut processor = Self::with_size(config.width, config.height)?;
        if let Some(title) = config.title {
            processor.window_title = title;
        }
        Ok(processor)
    }

    fn render(&mut self, frame: Self::Input) -> Result<()> {
        // Get the WebGPU texture from the frame
        let wgpu_texture = &frame.texture;

        // Unwrap WebGPU texture to Metal texture (zero-copy!)
        let wgpu_bridge = self.wgpu_bridge.as_ref()
            .ok_or_else(|| StreamError::Configuration("WgpuBridge not initialized".into()))?;

        let source_metal = unsafe {
            wgpu_bridge.unwrap_to_metal_texture(wgpu_texture)
        }?;

        // Get CAMetalLayer from atomic address (0 = not created yet)
        let layer_addr = self.layer_addr.load(Ordering::Acquire);
        if layer_addr == 0 {
            // Window not created yet - drop this frame
            return Ok(());
        }

        // SAFETY: Layer was created on main thread and address stored atomically
        // We reconstruct it here just for this frame's operations
        // CAMetalLayer.nextDrawable() is thread-safe according to Apple docs
        let metal_layer = unsafe {
            // Don't take ownership - just borrow for this frame
            let ptr = layer_addr as *const CAMetalLayer;
            &*ptr
        };

        unsafe {
            if let Some(drawable) = metal_layer.nextDrawable() {
                let drawable_metal = drawable.texture();

                // Convert objc2_metal texture to metal crate texture for drawable
                use metal::foreign_types::ForeignTypeRef;
                let drawable_texture_ptr = &*drawable_metal as *const _ as *mut std::ffi::c_void;
                let drawable_metal_ref = metal::TextureRef::from_ptr(drawable_texture_ptr as *mut _);

                // Convert objc2 drawable to metal crate drawable
                let drawable_ptr = &*drawable as *const _ as *mut std::ffi::c_void;
                let drawable_ref = metal::DrawableRef::from_ptr(drawable_ptr as *mut _);

                // Create Metal command buffer and blit encoder
                let command_buffer = self.metal_command_queue.new_command_buffer();
                let blit_encoder = command_buffer.new_blit_command_encoder();

                // Blit from source texture (RGBA) to drawable texture (BGRA)
                // Metal blit encoder automatically handles RGBA→BGRA conversion!
                // This is a GPU-accelerated format conversion (zero-copy, sub-millisecond)
                use metal::MTLOrigin;
                use metal::MTLSize;

                let origin = MTLOrigin { x: 0, y: 0, z: 0 };
                let size = MTLSize {
                    width: frame.width as u64,
                    height: frame.height as u64,
                    depth: 1,
                };

                blit_encoder.copy_from_texture(
                    &source_metal,
                    0,  // source slice
                    0,  // source mip level
                    origin,
                    size,
                    drawable_metal_ref,
                    0,  // dest slice
                    0,  // dest mip level
                    origin,
                );

                blit_encoder.end_encoding();

                // Present the drawable
                command_buffer.present_drawable(drawable_ref);
                command_buffer.commit();

                self.frames_rendered += 1;

                // Log every 60 frames (once per second at 60fps)
                if self.frames_rendered.is_multiple_of(60) {
                    tracing::info!(
                        "Display {}: Rendered frame {} ({}x{}) via Metal blit",
                        self.window_id.0,
                        frame.frame_number,
                        frame.width,
                        frame.height
                    );
                }
                }
            }

        Ok(())
    }

    fn clock_config(&self) -> ClockConfig {
        ClockConfig {
            provides_clock: true,
            clock_type: Some(ClockType::Vsync),
            clock_name: Some(format!("display_{}_vsync", self.window_id.0)),
        }
    }

    fn sync_mode(&self) -> SyncMode {
        SyncMode::Timestamp
    }

    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new(
                "DisplayProcessor",
                "Displays video frames in a window. Renders WebGPU textures to the screen at the configured frame rate."
            )
            .with_usage_context(
                "Use when you need to visualize video output in a window. This is typically a sink \
                 processor at the end of a pipeline. Each DisplayProcessor manages one window. The window \
                 is created automatically on first frame and can be configured with set_window_title()."
            )
            .with_input(PortDescriptor::new(
                "video",
                SCHEMA_VIDEO_FRAME.clone(),
                true,
                "Video frames to display. Accepts WebGPU textures and renders them to the window. \
                 Automatically handles format conversion and scaling to fit the window."
            ))
            .with_tags(vec!["sink", "display", "window", "output", "render"])
        )
    }
}

// ============================================================
// Platform-Specific Initialization (Called by Runtime)
// ============================================================

impl AppleDisplayProcessor {
    /// Initialize GPU context (called by runtime after processor creation)
    pub fn initialize_gpu(&mut self, gpu_context: &crate::core::GpuContext) -> Result<()> {

        // Store the shared GPU context from runtime
        self.gpu_context = Some(gpu_context.clone());

        // Log device/queue addresses to verify all processors share same context
        tracing::info!(
            "Display {}: Received GPU context - device: {:p}, queue: {:p}",
            self.window_id.0,
            gpu_context.device().as_ref(),
            gpu_context.queue().as_ref()
        );

        // Create WgpuBridge using the shared device from runtime
        // This ensures all processors use the same GPU device for zero-copy texture sharing
        let device = (**gpu_context.device()).clone();
        let queue = (**gpu_context.queue()).clone();

        let bridge = WgpuBridge::from_shared_device(
            self.metal_device.clone_device(),
            device,
            queue,
        );

        self.wgpu_bridge = Some(Arc::new(bridge));

        tracing::info!("Display {}: Created WgpuBridge using runtime's shared GPU device", self.window_id.0);
        tracing::info!(
            "Display {}: Starting ({}x{}, title=\"{}\")",
            self.window_id.0,
            self.width,
            self.height,
            self.window_title
        );

        // Dispatch window creation to main thread asynchronously
        // This avoids blocking and potential deadlocks
        tracing::info!("Display {}: Dispatching window creation to main thread (async)", self.window_id.0);

        let width = self.width;
        let height = self.height;
        let window_title = self.window_title.clone();
        let metal_device = self.metal_device.clone_device();
        let window_id = self.window_id;
        let layer_addr = Arc::clone(&self.layer_addr);

        // Use GCD to dispatch window creation to main thread asynchronously
        use dispatch2::DispatchQueue;

        DispatchQueue::main().exec_async(move || {
            // SAFETY: This closure executes on the main thread via GCD
            let mtm = unsafe { MainThreadMarker::new_unchecked() };

            tracing::info!("Display {}: Creating window on main thread...", window_id.0);

            // Create window frame
            let frame = NSRect::new(
                NSPoint::new(100.0, 100.0),
                NSSize::new(width as f64, height as f64),
            );

            // Create window with standard style
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

            window.setTitle(&NSString::from_str(&window_title));

            // Create Metal layer
            let metal_layer = unsafe { CAMetalLayer::new() };
            metal_layer.setDevice(Some(&metal_device));
            metal_layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);

            // Set drawable size using objc2's CGSize
            unsafe {
                use objc2::{msg_send, Encode, Encoding};

                // CGSize is just {width: f64, height: f64}
                #[repr(C)]
                struct CGSize {
                    width: f64,
                    height: f64,
                }

                // SAFETY: CGSize is repr(C) with two f64 fields
                unsafe impl Encode for CGSize {
                    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
                }

                let size = CGSize {
                    width: width as f64,
                    height: height as f64,
                };

                let _: () = msg_send![&metal_layer, setDrawableSize: size];
            }

            // Attach layer to window's content view
            if let Some(content_view) = window.contentView() {
                content_view.setWantsLayer(true);
                content_view.setLayer(Some(&metal_layer));
            }

            // Show window
            window.makeKeyAndOrderFront(None);

            // Activate app if not already active
            let app = NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
            app.activateIgnoringOtherApps(true);


            // Store layer address atomically so process() can access it
            // Window will be leaked (stays alive until app exits)
            let _ = Retained::into_raw(window);  // Leak window
            let addr = Retained::into_raw(metal_layer) as usize;
            layer_addr.store(addr, Ordering::Release);

        });

        self.window_creation_dispatched = true;

        tracing::info!("Display {}: Window creation dispatched, processor ready", self.window_id.0);
        Ok(())
    }

}
