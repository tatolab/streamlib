//! Apple Display Processor
//!
//! StreamProcessor implementation that renders VideoFrames to an NSWindow.
//! Each DisplayProcessor instance manages its own window.

use streamlib_core::{
    StreamProcessor, DisplayProcessor, DisplayInputPorts, WindowId,
    TimedTick, Result, StreamError,
};
use crate::{metal::MetalDevice, WgpuBridge};
use objc2::{rc::Retained, MainThreadOnly, MainThreadMarker};
use objc2_foundation::{NSString, NSPoint, NSSize, NSRect};
use objc2_app_kit::{NSWindow, NSBackingStoreType, NSWindowStyleMask, NSApplication, NSApplicationActivationPolicy};
use objc2_quartz_core::{CAMetalLayer, CAMetalDrawable};
use objc2_metal::MTLPixelFormat;
use std::sync::{atomic::{AtomicU64, Ordering}, Arc};
use metal;

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(1);

/// Apple-specific display processor
///
/// Each instance manages one NSWindow with a CAMetalLayer for Metal rendering.
/// Accepts VideoFrame input and renders to screen.
///
/// NOTE: Currently this is a stub that doesn't actually render.
/// TODO: Implement actual rendering from VideoFrame GPU textures.
pub struct AppleDisplayProcessor {
    // Window components (will be created on start)
    window: Option<Retained<NSWindow>>,
    metal_layer: Option<Retained<CAMetalLayer>>,
    #[allow(dead_code)] // Stored for potential future direct Metal API usage
    metal_device: MetalDevice,
    metal_command_queue: metal::CommandQueue,

    // GPU context (shared with all processors via runtime)
    gpu_context: Option<streamlib_core::GpuContext>,

    // WebGPU bridge (created from shared device in on_start)
    wgpu_bridge: Option<Arc<WgpuBridge>>,

    // Processor state
    ports: DisplayInputPorts,
    window_id: WindowId,
    window_title: String,
    width: u32,
    height: u32,
    frames_rendered: u64,
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

        // WgpuBridge will be created from runtime's shared device in on_start()

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

        // Create window and Metal layer on construction (main thread)
        let (window, metal_layer) = unsafe {
            let mtm = MainThreadMarker::new().expect("DisplayProcessor must be created on main thread");

            // Initialize NSApplication (required for windows to show)
            let app = NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

            // Create CAMetalLayer
            let metal_layer = CAMetalLayer::new();
            metal_layer.setDevice(Some(metal_device.device()));
            metal_layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
            metal_layer.setFramebufferOnly(false);

            let drawable_size = NSSize::new(width as f64, height as f64);
            metal_layer.setDrawableSize(drawable_size);

            // Create NSWindow
            let content_rect = NSRect::new(
                NSPoint::new(100.0, 100.0),
                NSSize::new(width as f64, height as f64),
            );

            let style_mask = NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Resizable;

            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                content_rect,
                style_mask,
                NSBackingStoreType::Buffered,
                false,
            );

            window.setTitle(&NSString::from_str(&window_title));

            // Set metal layer as content view's layer
            let content_view = window.contentView()
                .ok_or_else(|| StreamError::Configuration("No content view".into()))?;
            content_view.setWantsLayer(true);
            content_view.setLayer(Some(&metal_layer));

            // Show window immediately (must be done on main thread)
            window.makeKeyAndOrderFront(None);

            (window, metal_layer)
        };

        Ok(Self {
            window: Some(window),
            metal_layer: Some(metal_layer),
            metal_device,
            metal_command_queue,
            gpu_context: None,  // Will be set by runtime in on_start()
            wgpu_bridge: None,  // Will be created from shared device in on_start()
            ports: DisplayInputPorts {
                video: streamlib_core::StreamInput::new("video"),
            },
            window_id,
            window_title,
            width,
            height,
            frames_rendered: 0,
        })
    }
}

impl DisplayProcessor for AppleDisplayProcessor {
    fn set_window_title(&mut self, title: &str) {
        self.window_title = title.to_string();

        // If window already exists, update it
        if let Some(ref window) = self.window {
            window.setTitle(&NSString::from_str(title));
        }
    }

    fn window_id(&self) -> Option<WindowId> {
        Some(self.window_id)
    }

    fn input_ports(&mut self) -> &mut DisplayInputPorts {
        &mut self.ports
    }
}

impl StreamProcessor for AppleDisplayProcessor {
    fn process(&mut self, _tick: TimedTick) -> Result<()> {
        // Read latest video frame
        if let Some(frame) = self.ports.video.read_latest() {
            // Get the WebGPU texture from the frame
            let wgpu_texture = &frame.texture;

            // Unwrap WebGPU texture to Metal texture (zero-copy!)
            let wgpu_bridge = self.wgpu_bridge.as_ref()
                .ok_or_else(|| StreamError::Configuration("WgpuBridge not initialized".into()))?;

            let source_metal = unsafe {
                wgpu_bridge.unwrap_to_metal_texture(wgpu_texture)
            }?;

            // Get the next drawable from CAMetalLayer
            let metal_layer = self.metal_layer.as_ref()
                .ok_or_else(|| StreamError::Configuration("No Metal layer".into()))?;

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

                    // Blit from source texture to drawable texture (fastest path!)
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
                    if self.frames_rendered % 60 == 0 {
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
        }

        Ok(())
    }

    fn on_start(&mut self, gpu_context: &streamlib_core::GpuContext) -> Result<()> {
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

        // Window was already shown in constructor (on main thread)
        tracing::info!("Display {}: Ready to process frames", self.window_id.0);
        Ok(())
    }

    fn on_stop(&mut self) -> Result<()> {
        tracing::info!(
            "Display {}: Stopped (rendered {} frames)",
            self.window_id.0,
            self.frames_rendered
        );

        // Close window
        if let Some(ref window) = self.window {
            window.close();
        }

        self.window = None;
        self.metal_layer = None;

        Ok(())
    }
}
