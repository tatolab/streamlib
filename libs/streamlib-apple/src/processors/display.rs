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
use objc2_metal::{MTLPixelFormat, MTLDrawable};
use std::sync::{atomic::{AtomicU64, Ordering}, Arc};

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
    metal_device: MetalDevice,
    wgpu_bridge: Arc<WgpuBridge>,

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

        // Create WgpuBridge for WebGPU → Metal conversion
        let wgpu_bridge = pollster::block_on(async {
            WgpuBridge::new(metal_device.clone_device()).await
        })?;

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
            wgpu_bridge: Arc::new(wgpu_bridge),
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

            // Get the next drawable from CAMetalLayer
            let metal_layer = self.metal_layer.as_ref()
                .ok_or_else(|| StreamError::Configuration("No Metal layer".into()))?;

            unsafe {
                if let Some(drawable) = metal_layer.nextDrawable() {
                    let metal_texture = drawable.texture();

                    // Use wgpu command encoder to blit from WebGPU texture to drawable
                    let (device, queue) = self.wgpu_bridge.wgpu();
                    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Display Blit"),
                    });

                    // Wrap the Metal drawable as WebGPU texture
                    let drawable_wgpu = self.wgpu_bridge.wrap_metal_texture(
                        &metal_texture,
                        wgpu::TextureFormat::Bgra8Unorm,
                        wgpu::TextureUsages::COPY_DST,
                    )?;

                    // Blit the wgpu texture to the drawable texture
                    encoder.copy_texture_to_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: wgpu_texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyTextureInfo {
                            texture: &drawable_wgpu,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d {
                            width: frame.width,
                            height: frame.height,
                            depth_or_array_layers: 1,
                        },
                    );

                    queue.submit(Some(encoder.finish()));

                    // Present the drawable
                    drawable.present();

                    self.frames_rendered += 1;

                    // Log every 60 frames (once per second at 60fps)
                    if self.frames_rendered % 60 == 0 {
                        tracing::info!(
                            "Display {}: Rendered frame {} ({}x{})",
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

    fn on_start(&mut self) -> Result<()> {
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
