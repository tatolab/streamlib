
use crate::core::{
    WindowId, StreamInput, VideoFrame,
    Result, StreamError,
    ProcessorDescriptor, PortDescriptor, SCHEMA_VIDEO_FRAME,
};
use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::apple::{metal::MetalDevice, WgpuBridge, main_thread::execute_on_main_thread};
use objc2::{rc::Retained, MainThreadMarker};
use objc2_foundation::{NSString, NSPoint, NSSize, NSRect};
use objc2_app_kit::{NSWindow, NSBackingStoreType, NSWindowStyleMask, NSApplication, NSApplicationActivationPolicy};
use objc2_quartz_core::{CAMetalLayer, CAMetalDrawable};
use objc2_metal::MTLPixelFormat;
use std::sync::{atomic::{AtomicU64, AtomicUsize, Ordering}, Arc};
use metal;
use streamlib_macros::StreamProcessor;

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(1);

#[derive(StreamProcessor)]
pub struct AppleDisplayProcessor {
    // Port field - annotated!
    #[input]
    video: StreamInput<VideoFrame>,

    // Config fields
    window: Option<Retained<NSWindow>>,
    metal_layer: Option<Retained<CAMetalLayer>>,

    layer_addr: Arc<AtomicUsize>,
    #[allow(dead_code)] // Stored for potential future direct Metal API usage
    metal_device: MetalDevice,
    metal_command_queue: metal::CommandQueue,

    gpu_context: Option<crate::core::GpuContext>,

    wgpu_bridge: Option<Arc<WgpuBridge>>,

    window_id: WindowId,
    window_title: String,
    width: u32,
    height: u32,
    frames_rendered: u64,
    window_creation_dispatched: bool,
}

// SAFETY: NSWindow, CAMetalLayer, and Metal objects are Objective-C objects that can be sent between threads.
unsafe impl Send for AppleDisplayProcessor {}

impl AppleDisplayProcessor {
    pub fn new() -> Result<Self> {
        Self::with_size(1920, 1080)
    }

    pub fn with_size(width: u32, height: u32) -> Result<Self> {
        let window_id = WindowId(NEXT_WINDOW_ID.fetch_add(1, Ordering::SeqCst));
        let metal_device = MetalDevice::new()?;

        let metal_command_queue = {
            use metal::foreign_types::ForeignTypeRef;
            let device_ptr = metal_device.device() as *const _ as *mut std::ffi::c_void;
            let metal_device_ref = unsafe {
                metal::DeviceRef::from_ptr(device_ptr as *mut _)
            };
            metal_device_ref.new_command_queue()
        };

        let window_title = "streamlib Display".to_string();

        Ok(Self {
            // Port
            video: StreamInput::new("video"),

            // Config fields
            window: None,
            metal_layer: None,
            layer_addr: Arc::new(AtomicUsize::new(0)),
            metal_device,
            metal_command_queue,
            gpu_context: None,
            wgpu_bridge: None,
            window_id,
            window_title,
            width,
            height,
            frames_rendered: 0,
            window_creation_dispatched: false,
        })
    }
}


impl StreamElement for AppleDisplayProcessor {
    fn name(&self) -> &str {
        &self.window_title
    }

    fn element_type(&self) -> ElementType {
        ElementType::Sink
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <AppleDisplayProcessor as StreamProcessor>::descriptor()
    }

    fn input_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "video".to_string(),
            schema: SCHEMA_VIDEO_FRAME.clone(),
            required: true,
            description: "Video frames to display".to_string(),
        }]
    }

    fn start(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());

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


impl StreamProcessor for AppleDisplayProcessor {
    type Config = crate::core::DisplayConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        let mut processor = Self::with_size(config.width, config.height)?;
        if let Some(title) = config.title {
            processor.window_title = title;
        }
        Ok(processor)
    }

    fn process(&mut self) -> Result<()> {
        // Direct field access - no nested ports!
        if let Some(frame) = self.video.read_latest() {
            let wgpu_texture = &frame.texture;

        let wgpu_bridge = self.wgpu_bridge.as_ref()
            .ok_or_else(|| StreamError::Configuration("WgpuBridge not initialized".into()))?;

        let source_metal = unsafe {
            wgpu_bridge.unwrap_to_metal_texture(wgpu_texture)
        }?;

        let layer_addr = self.layer_addr.load(Ordering::Acquire);
        if layer_addr == 0 {
            return Ok(());
        }

        // SAFETY: Layer was created on main thread and address stored atomically
        let metal_layer = unsafe {
            let ptr = layer_addr as *const CAMetalLayer;
            &*ptr
        };

        unsafe {
            if let Some(drawable) = metal_layer.nextDrawable() {
                let drawable_metal = drawable.texture();

                use metal::foreign_types::ForeignTypeRef;
                let drawable_texture_ptr = &*drawable_metal as *const _ as *mut std::ffi::c_void;
                let drawable_metal_ref = metal::TextureRef::from_ptr(drawable_texture_ptr as *mut _);

                let drawable_ptr = &*drawable as *const _ as *mut std::ffi::c_void;
                let drawable_ref = metal::DrawableRef::from_ptr(drawable_ptr as *mut _);

                let command_buffer = self.metal_command_queue.new_command_buffer();
                let blit_encoder = command_buffer.new_blit_command_encoder();

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

                command_buffer.present_drawable(drawable_ref);
                command_buffer.commit();

                self.frames_rendered += 1;

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
        }  // Close if let Some(frame)

        Ok(())
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

impl crate::core::DisplayProcessor for AppleDisplayProcessor {
    fn set_window_title(&mut self, title: &str) {
        self.window_title = title.to_string();
    }

    fn window_id(&self) -> Option<WindowId> {
        Some(self.window_id)
    }
}

impl AppleDisplayProcessor {
    pub fn initialize_gpu(&mut self, gpu_context: &crate::core::GpuContext) -> Result<()> {

        self.gpu_context = Some(gpu_context.clone());

        tracing::info!(
            "Display {}: Received GPU context - device: {:p}, queue: {:p}",
            self.window_id.0,
            gpu_context.device().as_ref(),
            gpu_context.queue().as_ref()
        );

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

        tracing::info!("Display {}: Dispatching window creation to main thread (async)", self.window_id.0);

        let width = self.width;
        let height = self.height;
        let window_title = self.window_title.clone();
        let metal_device = self.metal_device.clone_device();
        let window_id = self.window_id;
        let layer_addr = Arc::clone(&self.layer_addr);

        use dispatch2::DispatchQueue;

        DispatchQueue::main().exec_async(move || {
            // SAFETY: This closure executes on the main thread via GCD
            let mtm = unsafe { MainThreadMarker::new_unchecked() };

            tracing::info!("Display {}: Creating window on main thread...", window_id.0);

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

            window.setTitle(&NSString::from_str(&window_title));

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

            if let Some(content_view) = window.contentView() {
                content_view.setWantsLayer(true);
                content_view.setLayer(Some(&metal_layer));
            }

            window.makeKeyAndOrderFront(None);

            let app = NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
            app.activateIgnoringOtherApps(true);


            let _ = Retained::into_raw(window);  // Leak window
            let addr = Retained::into_raw(metal_layer) as usize;
            layer_addr.store(addr, Ordering::Release);

        });

        self.window_creation_dispatched = true;

        tracing::info!("Display {}: Window creation dispatched, processor ready", self.window_id.0);

        Ok(())
    }

}
