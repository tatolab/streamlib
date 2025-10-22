//! CAMetalLayer display window for zero-copy rendering
//!
//! Provides direct Metal rendering to macOS/iOS windows using CAMetalLayer.
//! This is a truly zero-copy path: GpuTexture → CAMetalDrawable → screen.
//!
//! Architecture:
//! - NSWindow + NSView → CAMetalLayer (swapchain)
//! - CAMetalDrawable.nextDrawable() → MTLTexture
//! - Blit GpuTexture → drawable texture with MSL shader
//! - drawable.present() → display
//!
//! No WebGPU abstraction, pure Metal API for maximum performance.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::msg_send;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSRect, NSPoint, NSSize, NSString};
use objc2_metal::{
    MTLDevice, MTLTexture, MTLLibrary, MTLRenderPipelineState,
    MTLRenderPipelineDescriptor, MTLPixelFormat, MTLLoadAction, MTLStoreAction,
    MTLClearColor, MTLRenderPassDescriptor, MTLPrimitiveType, MTLSamplerState,
    MTLSamplerDescriptor, MTLSamplerMinMagFilter, MTLSamplerMipFilter,
    MTLSamplerAddressMode, MTLCommandBuffer, MTLRenderCommandEncoder,
};
use streamlib_core::{GpuTexture, Result, StreamError};
use std::time::Instant;

use crate::metal::MetalDevice;
use crate::texture::metal_texture_from_gpu_texture;

#[cfg(target_os = "macos")]
use objc2_app_kit::{NSWindow, NSView, NSApplication, NSBackingStoreType, NSWindowStyleMask};

#[cfg(target_os = "macos")]
use objc2_quartz_core::CAMetalLayer;

/// MSL shader for fullscreen blit (vertex + fragment in one source)
const BLIT_SHADER: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct VertexOut {
    float4 position [[position]];
    float2 texCoord;
};

vertex VertexOut vertex_main(uint vertexID [[vertex_id]]) {
    // Fullscreen triangle (covers entire viewport)
    float2 positions[3] = {
        float2(-1.0, -1.0),
        float2( 3.0, -1.0),
        float2(-1.0,  3.0)
    };

    // Texture coordinates (flip Y for Metal's coordinate system)
    float2 texCoords[3] = {
        float2(0.0, 1.0),
        float2(2.0, 1.0),
        float2(0.0, -1.0)
    };

    VertexOut out;
    out.position = float4(positions[vertexID], 0.0, 1.0);
    out.texCoord = texCoords[vertexID];
    return out;
}

fragment float4 fragment_main(
    VertexOut in [[stage_in]],
    texture2d<float> inputTexture [[texture(0)]],
    sampler inputSampler [[sampler(0)]]
) {
    return inputTexture.sample(inputSampler, in.texCoord);
}
"#;

/// Blit rendering pipeline for GpuTexture → swapchain
struct BlitPipeline {
    pipeline_state: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    sampler: Retained<ProtocolObject<dyn MTLSamplerState>>,
}

impl BlitPipeline {
    fn create(device: &ProtocolObject<dyn MTLDevice>) -> Result<Self> {
        unsafe {
            // Use combined shader source
            let shader_source = BLIT_SHADER;

            // Create MTLLibrary from source with error capture
            let ns_source = NSString::from_str(&shader_source);
            let mut error: *mut objc2::runtime::AnyObject = std::ptr::null_mut();
            let library: Option<Retained<ProtocolObject<dyn MTLLibrary>>> = msg_send![
                device,
                newLibraryWithSource: &*ns_source,
                options: std::ptr::null::<objc2::runtime::AnyObject>(),
                error: &mut error
            ];
            let library = library.ok_or_else(|| {
                let error_msg = if !error.is_null() {
                    let desc: Retained<NSString> = msg_send![error, localizedDescription];
                    format!("Shader compilation failed: {}", desc.to_string())
                } else {
                    "Failed to compile blit shader (no error details)".to_string()
                };
                StreamError::ShaderCompilation(error_msg)
            })?;

            // Get shader functions
            let name = NSString::from_str("vertex_main");
            let vertex_fn = library.newFunctionWithName(&name)
                .ok_or_else(|| StreamError::ShaderCompilation("Vertex function not found".into()))?;

            let name = NSString::from_str("fragment_main");
            let fragment_fn = library.newFunctionWithName(&name)
                .ok_or_else(|| StreamError::ShaderCompilation("Fragment function not found".into()))?;

            // Create render pipeline descriptor
            let descriptor = MTLRenderPipelineDescriptor::new();
            descriptor.setVertexFunction(Some(&vertex_fn));
            descriptor.setFragmentFunction(Some(&fragment_fn));

            // Set color attachment format (BGRA8Unorm for CAMetalLayer)
            let color_attachments = descriptor.colorAttachments();
            let attachment_0 = color_attachments.objectAtIndexedSubscript(0);
            attachment_0.setPixelFormat(MTLPixelFormat::BGRA8Unorm);

            // Create pipeline state
            let state: Option<Retained<ProtocolObject<dyn MTLRenderPipelineState>>> = msg_send![
                device,
                newRenderPipelineStateWithDescriptor: &*descriptor,
                error: std::ptr::null_mut::<*mut objc2::runtime::AnyObject>()
            ];
            let pipeline_state = state.ok_or_else(|| {
                StreamError::ShaderCompilation("Failed to create render pipeline state".into())
            })?;

            // Create linear sampler for texture filtering
            let sampler_desc = MTLSamplerDescriptor::new();
            sampler_desc.setMinFilter(MTLSamplerMinMagFilter::Linear);
            sampler_desc.setMagFilter(MTLSamplerMinMagFilter::Linear);
            sampler_desc.setMipFilter(MTLSamplerMipFilter::Linear);
            sampler_desc.setSAddressMode(MTLSamplerAddressMode::ClampToEdge);
            sampler_desc.setTAddressMode(MTLSamplerAddressMode::ClampToEdge);

            let sampler = device.newSamplerStateWithDescriptor(&sampler_desc)
                .ok_or_else(|| StreamError::GpuError("Failed to create sampler".into()))?;

            Ok(Self {
                pipeline_state,
                sampler,
            })
        }
    }
}

/// FPS tracking for performance monitoring
struct FpsTracker {
    frame_count: u32,
    last_update: Instant,
    current_fps: f32,
    update_interval: f32,
}

impl FpsTracker {
    fn new(update_interval: f32) -> Self {
        Self {
            frame_count: 0,
            last_update: Instant::now(),
            current_fps: 0.0,
            update_interval,
        }
    }

    fn update(&mut self) {
        self.frame_count += 1;

        let elapsed = self.last_update.elapsed().as_secs_f32();
        if elapsed >= self.update_interval {
            self.current_fps = self.frame_count as f32 / elapsed;
            self.frame_count = 0;
            self.last_update = Instant::now();
        }
    }

    fn current_fps(&self) -> f32 {
        self.current_fps
    }

    fn should_update_title(&self) -> bool {
        self.frame_count == 0 // Just reset, so FPS was updated
    }
}

/// Display window using CAMetalLayer for zero-copy rendering
#[cfg(target_os = "macos")]
pub struct DisplayWindow {
    ns_window: Retained<NSWindow>,
    #[allow(dead_code)] // Kept to maintain view lifetime
    content_view: Retained<NSView>,
    metal_layer: Retained<CAMetalLayer>,
    metal_device: Retained<ProtocolObject<dyn MTLDevice>>,
    command_queue: Retained<ProtocolObject<dyn objc2_metal::MTLCommandQueue>>,
    blit_pipeline: Option<BlitPipeline>,
    fps_tracker: Option<FpsTracker>,
    text_renderer: Option<crate::text::TextRenderer>,
    fps_texture: Option<Retained<ProtocolObject<dyn MTLTexture>>>,
    title: String,
}

#[cfg(target_os = "macos")]
impl DisplayWindow {
    /// Create a new display window
    ///
    /// # Arguments
    /// * `metal_device` - Metal device for rendering
    /// * `width` - Window width in pixels
    /// * `height` - Window height in pixels
    /// * `title` - Window title
    /// * `show_fps` - Enable FPS counter in title
    ///
    /// # Returns
    /// DisplayWindow instance ready for rendering
    pub fn new(
        metal_device: &MetalDevice,
        width: u32,
        height: u32,
        title: &str,
        show_fps: bool,
    ) -> Result<Self> {
        unsafe {
            // Get main thread marker (required for AppKit)
            let mtm = MainThreadMarker::new().ok_or_else(|| {
                StreamError::GpuError("DisplayWindow must be created on main thread".into())
            })?;

            // Initialize NSApplication
            let app = NSApplication::sharedApplication(mtm);
            use objc2_app_kit::NSApplicationActivationPolicy;
            app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

            // Finish launching if needed (required for proper app initialization)
            if !app.isRunning() {
                app.finishLaunching();
            }

            // Activate the application (required for window to appear)
            // Use activateIgnoringOtherApps: which is the correct selector
            let _: () = msg_send![
                &*app,
                activateIgnoringOtherApps: true
            ];

            // Create window frame
            let frame = NSRect::new(
                NSPoint::new(100.0, 100.0),
                NSSize::new(width as f64, height as f64),
            );

            // Create NSWindow with style mask
            let style_mask = NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable
                | NSWindowStyleMask::Resizable;

            let ns_window = NSWindow::alloc(mtm);
            let ns_window: Retained<NSWindow> = msg_send![
                ns_window,
                initWithContentRect: frame,
                styleMask: style_mask,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ];

            // Set window title
            let ns_title = NSString::from_str(title);
            ns_window.setTitle(&ns_title);

            // Create CAMetalLayer
            let metal_layer = CAMetalLayer::new();
            metal_layer.setDevice(Some(metal_device.device()));
            metal_layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);

            // Set drawable size
            let drawable_size = NSSize::new(width as f64, height as f64);
            metal_layer.setDrawableSize(drawable_size);

            // Create NSView
            let content_view = NSView::alloc(mtm);
            let content_view: Retained<NSView> = msg_send![
                content_view,
                initWithFrame: frame
            ];

            // Attach metal layer to view
            content_view.setWantsLayer(true);
            content_view.setLayer(Some(&metal_layer));

            // Set view as window content
            ns_window.setContentView(Some(&content_view));

            // Show window
            ns_window.makeKeyAndOrderFront(None);

            // Clone Metal device and command queue (increment retain counts for owned references)
            let metal_dev = metal_device.clone_device();
            let cmd_queue = metal_device.clone_command_queue();

            // Create FPS tracker and text renderer if requested
            let (fps_tracker, text_renderer) = if show_fps {
                let tracker = FpsTracker::new(0.5); // Update every 0.5 seconds
                let renderer = crate::text::TextRenderer::new(&metal_dev)?;
                (Some(tracker), Some(renderer))
            } else {
                (None, None)
            };

            Ok(Self {
                ns_window,
                content_view,
                metal_layer,
                metal_device: metal_dev,
                command_queue: cmd_queue,
                blit_pipeline: None,
                fps_tracker,
                text_renderer,
                fps_texture: None,
                title: title.to_string(),
            })
        }
    }

    /// Render a GpuTexture to the display
    ///
    /// This performs zero-copy rendering:
    /// 1. Get next drawable from CAMetalLayer
    /// 2. Blit GpuTexture → drawable texture
    /// 3. Present drawable to screen
    ///
    /// # Arguments
    /// * `gpu_texture` - The texture to display
    pub fn render(&mut self, gpu_texture: &GpuTexture) -> Result<()> {
        unsafe {
            // Get next drawable
            let drawable: Option<Retained<ProtocolObject<dyn objc2_quartz_core::CAMetalDrawable>>> = msg_send![
                &*self.metal_layer,
                nextDrawable
            ];

            let drawable = drawable.ok_or_else(|| {
                StreamError::TextureError("No drawable available from CAMetalLayer".into())
            })?;

            let drawable_texture: Retained<ProtocolObject<dyn MTLTexture>> = msg_send![
                &*drawable,
                texture
            ];

            // Extract Metal texture from GpuTexture
            let source_texture = metal_texture_from_gpu_texture(gpu_texture)?;

            // Create blit pipeline if needed
            if self.blit_pipeline.is_none() {
                self.blit_pipeline = Some(BlitPipeline::create(&self.metal_device)?);
            }
            let blit = self.blit_pipeline.as_ref().unwrap();

            // Create command buffer using msg_send (method not exposed in trait)
            let command_buffer: Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>> = msg_send![
                &*self.command_queue,
                commandBuffer
            ];
            let command_buffer = command_buffer
                .ok_or_else(|| StreamError::GpuError("Failed to create command buffer".into()))?;

            // Create render pass descriptor
            let render_pass_desc = MTLRenderPassDescriptor::new();
            let color_attachments = render_pass_desc.colorAttachments();
            let attachment_0 = color_attachments.objectAtIndexedSubscript(0);

            attachment_0.setTexture(Some(&drawable_texture));
            attachment_0.setLoadAction(MTLLoadAction::Clear);
            attachment_0.setStoreAction(MTLStoreAction::Store);

            // Set clear color manually
            let clear_color = MTLClearColor { red: 0.0, green: 0.0, blue: 0.0, alpha: 1.0 };
            attachment_0.setClearColor(clear_color);

            // Create render command encoder
            let encoder = command_buffer.renderCommandEncoderWithDescriptor(&render_pass_desc)
                .ok_or_else(|| StreamError::GpuError("Failed to create render encoder".into()))?;

            // Set pipeline and resources
            encoder.setRenderPipelineState(&blit.pipeline_state);
            encoder.setFragmentTexture_atIndex(Some(source_texture), 0);
            encoder.setFragmentSamplerState_atIndex(Some(&blit.sampler), 0);

            // Draw fullscreen triangle
            encoder.drawPrimitives_vertexStart_vertexCount(
                MTLPrimitiveType::Triangle,
                0,
                3
            );

            // End encoding using msg_send (method not exposed in trait)
            let _: () = msg_send![&*encoder, endEncoding];

            // Render FPS overlay if enabled
            if let (Some(fps_tracker), Some(text_renderer)) = (&mut self.fps_tracker, &self.text_renderer) {
                fps_tracker.update();

                // Regenerate FPS texture every update
                if fps_tracker.should_update_title() {
                    let fps_text = format!("FPS: {:.1}", fps_tracker.current_fps());
                    // Create text texture in top-left corner - bigger size for readability
                    self.fps_texture = text_renderer.render_text(&fps_text, 48.0, 400, 100).ok();
                }

                // Composite FPS texture if it exists
                if let Some(fps_tex) = &self.fps_texture {
                    self.composite_text_overlay(&command_buffer, &drawable_texture, fps_tex)?;
                }
            }

            // Present drawable using msg_send
            let _: () = msg_send![&*command_buffer, presentDrawable: &*drawable];
            command_buffer.commit();

            Ok(())
        }
    }

    /// Process window events (non-blocking)
    pub fn process_events(&self) {
        unsafe {
            let mtm = MainThreadMarker::new().expect("Must call process_events on main thread");
            let app = NSApplication::sharedApplication(mtm);

            // Import NSEvent types
            use objc2_app_kit::{NSEvent, NSEventMask};
            use objc2_foundation::NSDate;

            // Process all pending events (non-blocking)
            loop {
                let distant_past = NSDate::distantPast();
                let event: Option<Retained<NSEvent>> = msg_send![
                    &*app,
                    nextEventMatchingMask: NSEventMask::Any,
                    untilDate: &*distant_past,
                    inMode: objc2_foundation::NSDefaultRunLoopMode,
                    dequeue: true
                ];

                match event {
                    Some(evt) => {
                        app.sendEvent(&evt);
                    }
                    None => break,
                }
            }
        }
    }

    /// Update window title with current FPS
    ///
    /// Call this from the main thread periodically to update the FPS display.
    pub fn update_title_with_fps(&self) {
        if let Some(fps) = &self.fps_tracker {
            let title_with_fps = format!("{} - {:.1} FPS", self.title, fps.current_fps());
            let ns_title = NSString::from_str(&title_with_fps);
            self.ns_window.setTitle(&ns_title);
        }
    }

    /// Get current FPS (if tracking enabled)
    pub fn get_fps(&self) -> Option<f32> {
        self.fps_tracker.as_ref().map(|fps| fps.current_fps())
    }

    /// Check if window is still open
    pub fn is_open(&self) -> bool {
        self.ns_window.isVisible()
    }

    /// Close the window
    pub fn close(&self) {
        self.ns_window.close();
    }

    /// Get access to the underlying CAMetalLayer for custom rendering
    pub fn metal_layer(&self) -> &CAMetalLayer {
        &self.metal_layer
    }

    /// Composite text overlay onto the drawable texture
    fn composite_text_overlay(
        &self,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
        drawable_texture: &ProtocolObject<dyn MTLTexture>,
        text_texture: &ProtocolObject<dyn MTLTexture>,
    ) -> Result<()> {
        unsafe {
            use objc2_metal::{MTLRenderPassDescriptor, MTLLoadAction, MTLStoreAction, MTLBlitCommandEncoder};

            // Use blit encoder to copy the text texture region to drawable
            // This is simpler and more efficient than a render pass
            let blit_encoder: Option<Retained<ProtocolObject<dyn MTLBlitCommandEncoder>>> = msg_send![
                &*command_buffer,
                blitCommandEncoder
            ];

            if let Some(encoder) = blit_encoder {
                // Get text texture dimensions
                let text_width = text_texture.width();
                let text_height = text_texture.height();

                // Copy from text texture to drawable at (0, 0)
                use objc2_metal::{MTLOrigin, MTLSize};
                let origin = MTLOrigin { x: 0, y: 0, z: 0 };
                let size = MTLSize {
                    width: text_width,
                    height: text_height,
                    depth: 1,
                };

                encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                    text_texture,
                    0,
                    0,
                    origin,
                    size,
                    drawable_texture,
                    0,
                    0,
                    origin,
                );

                let _: () = msg_send![&*encoder, endEncoding];
            }

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iosurface::{create_iosurface, create_metal_texture_from_iosurface};
    use crate::texture::gpu_texture_from_metal;
    use streamlib_core::PixelFormat;

    #[test]
    #[ignore] // Requires display/GUI
    fn test_display_window_creation() {
        let device = MetalDevice::new().expect("Metal device");

        let window = DisplayWindow::new(
            &device,
            800,
            600,
            "Test Window",
            false
        );

        assert!(window.is_ok());
    }

    #[test]
    #[ignore] // Requires display/GUI
    fn test_display_render() {
        let device = MetalDevice::new().expect("Metal device");

        // Create test texture
        let surface = create_iosurface(800, 600, PixelFormat::Bgra8Unorm)
            .expect("IOSurface");
        let metal_texture = create_metal_texture_from_iosurface(device.device(), &surface, 0)
            .expect("Metal texture");
        let gpu_texture = gpu_texture_from_metal(metal_texture)
            .expect("GpuTexture");

        // Create window
        let mut window = DisplayWindow::new(
            &device,
            800,
            600,
            "Test Render",
            true
        ).expect("Display window");

        // Render one frame
        let result = window.render(&gpu_texture);
        assert!(result.is_ok());

        window.close();
    }
}
