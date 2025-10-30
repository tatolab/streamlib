//! Apple Camera Processor - Real AVFoundation Capture
//!
//! Zero-copy pipeline: CVPixelBuffer → IOSurface → Metal Texture → WebGPU Texture

use crate::core::{
    StreamProcessor, CameraProcessor, CameraOutputPorts, CameraDevice,
    VideoFrame, Result, StreamError,
    ProcessorDescriptor, PortDescriptor, ProcessorExample, SCHEMA_VIDEO_FRAME,
};
use std::sync::Arc;
use parking_lot::Mutex;
use std::ffi::c_void;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{msg_send, define_class};
use objc2_foundation::{MainThreadMarker, NSString, NSObject, NSObjectProtocol};
use objc2_av_foundation::{
    AVCaptureDevice, AVCaptureSession, AVCaptureDeviceInput, AVCaptureVideoDataOutput,
    AVMediaTypeVideo, AVCaptureVideoDataOutputSampleBufferDelegate, AVCaptureConnection,
};
use objc2_core_video::CVPixelBuffer;
use objc2_io_surface::IOSurface;
use crate::apple::{WgpuBridge, MetalDevice, iosurface};

// Core Media and Core Video C APIs
type CMSampleBufferRef = *mut c_void;

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetImageBuffer(sbuf: CMSampleBufferRef) -> *mut CVPixelBuffer;
    fn CVPixelBufferGetIOSurface(pixelBuffer: *const CVPixelBuffer) -> *mut IOSurface;
    fn CVPixelBufferGetWidth(pixelBuffer: *const CVPixelBuffer) -> usize;
    fn CVPixelBufferGetHeight(pixelBuffer: *const CVPixelBuffer) -> usize;
}

// No more GpuData trait - we use wgpu::Texture directly

// Thread-safe frame holder
struct FrameHolder {
    pixel_buffer: Retained<CVPixelBuffer>,
}

unsafe impl Send for FrameHolder {}
unsafe impl Sync for FrameHolder {}

// Global storage for frame holders (one per delegate instance)
// In practice, we only have one camera processor at a time
static FRAME_STORAGE: std::sync::OnceLock<Arc<Mutex<Option<FrameHolder>>>> = std::sync::OnceLock::new();

// Global wakeup channel (shared between delegate and processor)
// Phase 3: Camera wakeup on frame arrival
static WAKEUP_CHANNEL: std::sync::OnceLock<Arc<Mutex<Option<crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>>>>> = std::sync::OnceLock::new();

// Define the delegate class that receives camera frames
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "StreamlibCameraDelegate"]
    pub struct CameraDelegate;

    unsafe impl NSObjectProtocol for CameraDelegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for CameraDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        unsafe fn capture_output_did_output_sample_buffer_from_connection(
            &self,
            _output: *mut AnyObject,
            sample_buffer: CMSampleBufferRef,
            _connection: *mut AVCaptureConnection,
        ) {
            // Extract CVPixelBuffer from sample buffer
            let pixel_buffer_ref = CMSampleBufferGetImageBuffer(sample_buffer);
            if pixel_buffer_ref.is_null() {
                return;
            }

            // Retain the pixel_buffer
            let pixel_buffer = Retained::retain(pixel_buffer_ref as *mut CVPixelBuffer)
                .expect("Failed to retain pixel buffer");

            // Store in global frame holder
            if let Some(storage) = FRAME_STORAGE.get() {
                let frame_holder = FrameHolder { pixel_buffer };
                let mut latest = storage.lock();
                *latest = Some(frame_holder);

                // Phase 3: Trigger push-based wakeup when frame arrives
                if let Some(wakeup_storage) = WAKEUP_CHANNEL.get() {
                    if let Some(tx) = wakeup_storage.lock().as_ref() {
                        // Non-blocking send (unbounded channel)
                        let _ = tx.send(crate::core::runtime::WakeupEvent::DataAvailable);
                    }
                }
            }
        }
    }
);

impl CameraDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe {
            let this: Retained<Self> = msg_send![mtm.alloc::<Self>(), init];
            this
        }
    }
}

/// Apple camera processor with real AVFoundation capture
///
/// Note: AVCaptureSession runs on main thread independently.
/// This processor only reads frames from the shared Arc<Mutex>.
pub struct AppleCameraProcessor {
    #[allow(dead_code)] // Stored for future device management features
    device_id: Option<String>,
    ports: CameraOutputPorts,
    frame_count: u64,

    // Latest frame (thread-safe, shared with main thread capture)
    latest_frame: Arc<Mutex<Option<FrameHolder>>>,

    // GPU context (shared with all processors via runtime)
    gpu_context: Option<crate::core::GpuContext>,

    // Metal device (for IOSurface → Metal texture conversion)
    metal_device: Option<MetalDevice>,

    // WebGPU bridge (created from shared device in on_start)
    wgpu_bridge: Option<Arc<WgpuBridge>>,

    // Capture session info (for logging)
    #[allow(dead_code)] // Stored for future logging/diagnostics
    camera_name: String,

    // Delegate (must be kept alive to prevent deallocation)
    #[allow(dead_code)]
    delegate: Option<Retained<CameraDelegate>>,

    // Metal command queue for BGRA→RGBA blit conversion
    metal_command_queue: Option<metal::CommandQueue>,
}

impl AppleCameraProcessor {
    /// Create camera processor
    pub fn new() -> Result<Self> {
        Self::with_device_id_opt(None)
    }

    /// Create with specific device ID
    pub fn with_device_id(device_id: &str) -> Result<Self> {
        Self::with_device_id_opt(Some(device_id))
    }

    fn with_device_id_opt(device_id: Option<&str>) -> Result<Self> {
        eprintln!("Camera: Starting with_device_id_opt");

        // Must be on main thread for AVFoundation
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| StreamError::Configuration(
                "CameraProcessor must be created on main thread".into()
            ))?;

        eprintln!("Camera: Have main thread marker");
        tracing::info!("Camera: Initializing AVFoundation capture session");

        // Create Metal device (for IOSurface → Metal texture conversion)
        // WebGPU device/queue will be provided by runtime via on_start()
        let metal_device = MetalDevice::new()?;
        eprintln!("Camera: Created Metal device (WebGPU context will be provided by runtime)");

        let latest_frame = Arc::new(Mutex::new(None));

        // Create capture session
        let session = unsafe { AVCaptureSession::new() };
        eprintln!("Camera: Created capture session");

        // Configure session (must be done before adding inputs/outputs)
        unsafe {
            session.beginConfiguration();
        }
        eprintln!("Camera: Began configuration");

        // Get camera device
        eprintln!("Camera: About to get camera device");
        let device = unsafe {
            if let Some(id) = device_id {
                eprintln!("Camera: Looking for device with ID: {}", id);
                let id_str = NSString::from_str(id);
                let dev = AVCaptureDevice::deviceWithUniqueID(&id_str);
                if dev.is_none() {
                    eprintln!("Camera: Device with ID {} not found!", id);
                    return Err(StreamError::Configuration(
                        format!("Camera not found with ID: {}. The device may have been disconnected or the ID changed.", id)
                    ));
                }
                eprintln!("Camera: Found device by ID");
                dev.unwrap()
            } else {
                // Just use the default device - accessing device list can crash on Continuity Cameras
                let media_type = AVMediaTypeVideo.ok_or_else(|| StreamError::Configuration(
                    "AVMediaTypeVideo not available".into()
                ))?;

                eprintln!("Camera: Using default device");
                AVCaptureDevice::defaultDeviceWithMediaType(media_type)
                    .ok_or_else(|| StreamError::Configuration(
                        "No camera found".into()
                    ))?
            }
        };

        let device_name = unsafe { device.localizedName().to_string() };
        let device_unique_id = unsafe { device.uniqueID().to_string() };
        let device_model = unsafe { device.modelID().to_string() };
        let device_manufacturer = unsafe { device.manufacturer().to_string() };
        eprintln!("Camera: Got camera device: {} (ID: {})", device_name, device_unique_id);
        eprintln!("Camera: Model: {}, Manufacturer: {}", device_model, device_manufacturer);

        tracing::info!("Camera: Found device: {} ({})", device_name, device_model);

        // Check camera permission status
        eprintln!("Camera: Checking camera permission...");
        let media_type = unsafe {
            AVMediaTypeVideo.ok_or_else(|| StreamError::Configuration(
                "AVMediaTypeVideo not available".into()
            ))
        }?;

        // Note: We can't easily request permission here because we need async/callbacks.
        // The first time this runs, it will fail, but macOS will automatically prompt for permission.
        // On subsequent runs, it will work if permission was granted.
        let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };
        eprintln!("Camera: Authorization status = {:?}", status);

        // If not determined yet, macOS will prompt when we try to create the input
        // We'll let the deviceInputWithDevice_error call handle the permission prompt

        // Lock device for configuration
        eprintln!("Camera: Attempting to lock device for configuration");
        unsafe {
            if let Err(e) = device.lockForConfiguration() {
                eprintln!("Camera: Failed to lock device: {:?}", e);
                return Err(StreamError::Configuration(
                    format!("Failed to lock camera device: {:?}", e)
                ));
            }
            eprintln!("Camera: Device locked successfully");
            device.unlockForConfiguration();
            eprintln!("Camera: Device unlocked");
        }

        // Create input
        eprintln!("Camera: About to create input");
        let input = unsafe {
            AVCaptureDeviceInput::deviceInputWithDevice_error(&device)
                .map_err(|e| StreamError::Configuration(
                    format!("Failed to create camera input: {:?}", e)
                ))?
        };
        eprintln!("Camera: Created input successfully!");

        eprintln!("Camera: Checking if session can add input");
        let can_add = unsafe { session.canAddInput(&input) };
        eprintln!("Camera: canAddInput returned: {}", can_add);

        if !can_add {
            eprintln!("Camera: Session cannot add input!");
            return Err(StreamError::Configuration(
                "Session cannot add camera input. The camera may be in use by another application.".into()
            ));
        }

        eprintln!("Camera: About to call session.addInput");
        unsafe {
            // This is where the crash happens - AVFoundation throws an Objective-C exception
            // when trying to add certain USB cameras (especially on macOS 15.6+)
            session.addInput(&input);
        }
        eprintln!("Camera: Input added successfully");

        // Initialize global frame storage
        eprintln!("Camera: Initializing frame storage");
        let _ = FRAME_STORAGE.set(latest_frame.clone());

        // Initialize global wakeup channel storage (Phase 3: push-based operation)
        let wakeup_holder: Arc<Mutex<Option<crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>>>> =
            Arc::new(Mutex::new(None));
        let _ = WAKEUP_CHANNEL.set(wakeup_holder.clone());

        eprintln!("Camera: Creating AVCaptureVideoDataOutput");
        // Create output
        let output = unsafe { AVCaptureVideoDataOutput::new() };
        eprintln!("Camera: Created output");

        // NOTE: AVFoundation on macOS does NOT provide IOSurface-backed CVPixelBuffers
        // by default from USB cameras. This is a known limitation.
        //
        // For zero-copy GPU access, we have two options:
        // 1. Use built-in cameras (which DO provide IOSurface backing)
        // 2. Copy data from CVPixelBuffer to our own IOSurface (one copy, still fast)
        //
        // For now, we'll attempt to use the buffers as-is and provide a helpful
        // error message if IOSurface backing is missing.

        // Request BGRA format explicitly (AVFoundation defaults to YUV which requires
        // special handling with multiple textures)
        eprintln!("Camera: Setting pixel format to BGRA");
        use objc2_foundation::NSNumber;

        // kCVPixelFormatType_32BGRA = 'BGRA' = 0x42475241
        let pixel_format_key = unsafe {
            objc2_core_video::kCVPixelBufferPixelFormatTypeKey
        };
        let pixel_format_value = NSNumber::new_u32(0x42475241); // BGRA

        // Create NSDictionary with key-value pair using msg_send
        use objc2::ClassType;
        use objc2::runtime::AnyClass;
        let dict_cls: &AnyClass = objc2_foundation::NSDictionary::<objc2::runtime::AnyObject, objc2::runtime::AnyObject>::class();

        // Cast CFString key to id (AnyObject pointer)
        let key_ptr = pixel_format_key as *const _ as *const objc2::runtime::AnyObject;
        let value_ptr = &*pixel_format_value as *const _ as *const objc2::runtime::AnyObject;

        let video_settings_ptr: *mut objc2::runtime::AnyObject = unsafe {
            msg_send![dict_cls, dictionaryWithObject: value_ptr, forKey: key_ptr]
        };

        // Use msg_send directly to set video settings (bypassing type check)
        unsafe {
            let _: () = msg_send![&output, setVideoSettings: video_settings_ptr];
            eprintln!("Camera: Pixel format set to BGRA");
        }

        eprintln!("Camera: Creating delegate");
        // Create delegate to receive frames
        let delegate = CameraDelegate::new(mtm);
        eprintln!("Camera: Delegate created");

        eprintln!("Camera: Setting delegate on output with dispatch2::DispatchQueue");
        // Use dispatch2 to create a proper queue for the delegate
        // Using None for queue was causing crashes on macOS 15.6+
        unsafe {
            use dispatch2::{DispatchQueue, DispatchQueueAttr};
            let queue = DispatchQueue::new(
                "com.streamlib.camera.video",
                DispatchQueueAttr::SERIAL,
            );

            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&queue),
            );
        }
        eprintln!("Camera: Delegate set successfully");

        eprintln!("Camera: Checking if can add output");
        let can_add_output = unsafe { session.canAddOutput(&output) };
        eprintln!("Camera: canAddOutput returned: {}", can_add_output);

        if !can_add_output {
            eprintln!("Camera: Session cannot add output!");
            return Err(StreamError::Configuration("Cannot add camera output".into()));
        }

        eprintln!("Camera: About to call session.addOutput");
        unsafe {
            session.addOutput(&output);
        }
        eprintln!("Camera: Output added successfully");

        let camera_name = unsafe { device.localizedName().to_string() };

        // Commit configuration changes
        unsafe {
            session.commitConfiguration();
        }
        eprintln!("Camera: Committed configuration");

        // Start session
        eprintln!("Camera: About to start AVFoundation session");
        tracing::info!("Camera: Starting capture session");
        unsafe { session.startRunning(); }
        eprintln!("Camera: AVFoundation session.startRunning() called");

        // Session runs independently on main thread
        // We intentionally leak it so it stays alive
        // TODO: Properly manage session lifecycle
        std::mem::forget(session);
        eprintln!("Camera: Session leaked (will continue running)");

        tracing::info!("Camera: AVFoundation session running (will capture frames)");

        Ok(Self {
            device_id: device_id.map(String::from),
            ports: CameraOutputPorts {
                video: crate::core::StreamOutput::new("video"),
            },
            frame_count: 0,
            latest_frame,
            gpu_context: None,  // Will be set by runtime in on_start()
            metal_device: Some(metal_device),
            wgpu_bridge: None,  // Will be created from shared device in on_start()
            camera_name,
            delegate: Some(delegate),
            metal_command_queue: None,  // Will be created in on_start()
        })
    }
}

impl CameraProcessor for AppleCameraProcessor {
    fn set_device_id(&mut self, _device_id: &str) -> Result<()> {
        Err(StreamError::Configuration(
            "Cannot change device after creation. Use with_device_id()".into()
        ))
    }

    fn list_devices() -> Result<Vec<CameraDevice>> {
        unsafe {
            // Use AVCaptureDeviceDiscoverySession (modern API)
            use objc2_av_foundation::AVCaptureDeviceDiscoverySession;
            use objc2_foundation::NSArray;

            let media_type = AVMediaTypeVideo.ok_or_else(|| StreamError::Configuration(
                "AVMediaTypeVideo not available".into()
            ))?;

            // Include both built-in cameras and Continuity Cameras
            let builtin_wide = objc2_foundation::ns_string!("AVCaptureDeviceTypeBuiltInWideAngleCamera");
            let continuity = objc2_foundation::ns_string!("AVCaptureDeviceTypeContinuityCamera");

            // Create array with both device types
            let device_types = NSArray::from_slice(&[builtin_wide, continuity]);

            // Create discovery session
            let session = AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
                &device_types,
                Some(media_type),
                objc2_av_foundation::AVCaptureDevicePosition::Unspecified,
            );

            let devices = session.devices();
            let mut result = Vec::new();
            for i in 0..devices.count() {
                let device = devices.objectAtIndex(i);
                result.push(CameraDevice {
                    id: device.uniqueID().to_string(),
                    name: device.localizedName().to_string(),
                });
            }

            Ok(result)
        }
    }

    fn output_ports(&mut self) -> &mut CameraOutputPorts {
        &mut self.ports
    }
}

impl StreamProcessor for AppleCameraProcessor {
    fn process(&mut self) -> Result<()> {
        // Try to get the latest frame from the delegate
        let frame_holder = {
            let mut latest = self.latest_frame.lock();
            latest.take() // Take ownership, leaving None
        };

        if let Some(holder) = frame_holder {
            // Convert CVPixelBuffer → IOSurface → Metal Texture → WebGPU Texture
            let result: Result<()> = unsafe {
                let pixel_buffer_ref = &*holder.pixel_buffer as *const CVPixelBuffer;

                // Get IOSurface from CVPixelBuffer
                let iosurface_ref = CVPixelBufferGetIOSurface(pixel_buffer_ref);
                if iosurface_ref.is_null() {
                    // USB cameras on macOS don't provide IOSurface-backed buffers
                    // We need to copy the data to our own IOSurface for GPU access
                    tracing::warn!("Camera: Skipping frame {} (no IOSurface backing)", self.frame_count);
                    return Ok(());
                }

                let iosurface = Retained::retain(iosurface_ref)
                    .expect("Failed to retain IOSurface");

                // Get dimensions from CVPixelBuffer
                let width = CVPixelBufferGetWidth(pixel_buffer_ref);
                let height = CVPixelBufferGetHeight(pixel_buffer_ref);

                // Create Metal texture from IOSurface (zero-copy)
                let metal_device = self.metal_device.as_ref()
                    .ok_or_else(|| StreamError::Configuration("No Metal device".into()))?;

                let metal_texture = iosurface::create_metal_texture_from_iosurface(
                    metal_device.device(),
                    &iosurface,
                    0, // plane 0 for BGRA
                )?;

                // Convert IOSurface Metal texture to pure WebGPU-owned texture
                // This ensures downstream processors can use it with any usage flags (including compute shaders)
                let wgpu_bridge = self.wgpu_bridge.as_ref()
                    .ok_or_else(|| StreamError::Configuration("No WebGPU bridge".into()))?;

                // Step 1: Wrap IOSurface as temporary WebGPU texture (for reading only)
                let _iosurface_texture = wgpu_bridge.wrap_metal_texture(
                    &metal_texture,
                    wgpu::TextureFormat::Bgra8Unorm,
                    wgpu::TextureUsages::COPY_SRC,  // Only need COPY_SRC for reading
                )?;

                // Step 2: Convert BGRA→RGBA using Metal blit encoder (GPU-accelerated format conversion)
                // First, create Metal RGBA texture for the destination (using objc2_metal)
                let metal_device = self.metal_device.as_ref()
                    .ok_or_else(|| StreamError::Configuration("No Metal device".into()))?;

                let metal_rgba_texture = {
                    use objc2_metal::{MTLTextureDescriptor, MTLPixelFormat, MTLTextureUsage, MTLDevice};

                    let desc = MTLTextureDescriptor::new();
                    desc.setPixelFormat(MTLPixelFormat::RGBA8Unorm);
                    desc.setWidth(width);  // width is already usize
                    desc.setHeight(height);  // height is already usize
                    desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget);

                    metal_device.device().newTextureWithDescriptor(&desc)
                        .ok_or_else(|| StreamError::GpuError("Failed to create RGBA texture".into()))?
                };

                // Use Metal blit encoder to convert BGRA→RGBA (GPU-accelerated, sub-millisecond)
                // Convert objc2_metal types to metal crate types for blit encoder
                let command_queue = self.metal_command_queue.as_ref()
                    .ok_or_else(|| StreamError::Configuration("Metal command queue not initialized".into()))?;

                use metal::foreign_types::ForeignTypeRef;

                // Convert source texture (objc2_metal → metal crate)
                let source_texture_ptr = &*metal_texture as *const _ as *mut std::ffi::c_void;
                let source_texture_ref = metal::TextureRef::from_ptr(source_texture_ptr as *mut _);

                // Convert destination texture (objc2_metal → metal crate)
                let dest_texture_ptr = &*metal_rgba_texture as *const _ as *mut std::ffi::c_void;
                let dest_texture_ref = metal::TextureRef::from_ptr(dest_texture_ptr as *mut _);

                let command_buffer = command_queue.new_command_buffer();
                let blit_encoder = command_buffer.new_blit_command_encoder();

                use metal::MTLOrigin;
                use metal::MTLSize;

                let origin = MTLOrigin { x: 0, y: 0, z: 0 };
                let size = MTLSize {
                    width: width as u64,
                    height: height as u64,
                    depth: 1,
                };

                // Blit from BGRA IOSurface texture to RGBA texture
                // Metal blit encoder automatically handles BGRA→RGBA conversion!
                blit_encoder.copy_from_texture(
                    source_texture_ref,
                    0,  // source slice
                    0,  // source mip level
                    origin,
                    size,
                    dest_texture_ref,
                    0,  // dest slice
                    0,  // dest mip level
                    origin,
                );

                blit_encoder.end_encoding();
                command_buffer.commit();
                command_buffer.wait_until_completed();

                // Step 3: Wrap Metal RGBA texture as WebGPU texture
                let output_texture = wgpu_bridge.wrap_metal_texture(
                    &metal_rgba_texture,
                    wgpu::TextureFormat::Rgba8Unorm,
                    wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                )?;

                // Step 4: Create VideoFrame with RGBA texture
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs_f64();

                let frame = VideoFrame::new(
                    Arc::new(output_texture),
                    wgpu::TextureFormat::Rgba8Unorm,  // Explicit format
                    timestamp,
                    self.frame_count,
                    width as u32,
                    height as u32,
                );

                self.ports.video.write(frame);
                self.frame_count += 1;

                // Debug: Log every 60 frames (once per second at 60fps)
                if self.frame_count.is_multiple_of(60) {
                    tracing::info!(
                        "Camera: Wrote frame {} ({}x{}) to output port (WebGPU-owned texture, format=Rgba8Unorm)",
                        self.frame_count,
                        width,
                        height
                    );
                }

                Ok(())
            };

            if let Err(e) = result {
                tracing::error!("Camera: Error processing frame: {:?}", e);
                return Err(e);
            }
        }

        Ok(())
    }

    fn on_start(&mut self, gpu_context: &crate::core::GpuContext) -> Result<()> {
        // Store the shared GPU context from runtime
        self.gpu_context = Some(gpu_context.clone());

        // Log device/queue addresses to verify all processors share same context
        tracing::info!(
            "Camera: Received GPU context - device: {:p}, queue: {:p}",
            gpu_context.device().as_ref(),
            gpu_context.queue().as_ref()
        );

        // Create WgpuBridge using the shared device from runtime
        // This ensures all processors use the same GPU device for zero-copy texture sharing
        let metal_device = self.metal_device.as_ref()
            .ok_or_else(|| StreamError::Configuration("No Metal device".into()))?;

        // Clone the device and queue from Arc (wgpu types are cheaply cloneable)
        let device = (**gpu_context.device()).clone();
        let queue = (**gpu_context.queue()).clone();

        let bridge = WgpuBridge::from_shared_device(
            metal_device.clone_device(),
            device,
            queue,
        );

        self.wgpu_bridge = Some(Arc::new(bridge));

        tracing::info!("Camera: Created WgpuBridge using runtime's shared GPU device");

        // Create Metal command queue for BGRA→RGBA blit conversion
        use metal::foreign_types::ForeignTypeRef;
        let device_ptr = metal_device.device() as *const _ as *mut std::ffi::c_void;
        let metal_device_ref = unsafe {
            metal::DeviceRef::from_ptr(device_ptr as *mut _)
        };
        let command_queue = metal_device_ref.new_command_queue();
        self.metal_command_queue = Some(command_queue);

        tracing::info!("Camera: Created Metal command queue for BGRA→RGBA blit conversion");
        tracing::info!("Camera: Processor started (capture session already running)");
        Ok(())
    }

    fn on_stop(&mut self) -> Result<()> {
        tracing::info!("Camera: Processor stopped (generated {} frames)", self.frame_count);
        // Note: AVCaptureSession continues running on main thread
        // TODO: Implement proper session lifecycle management
        Ok(())
    }

    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new(
                "CameraProcessor",
                "Captures video frames from a camera device. Outputs WebGPU textures at the configured frame rate."
            )
            .with_usage_context(
                "Use when you need live video input from a camera. This is typically the source \
                 processor in a pipeline. Supports multiple camera devices - use set_device_id() \
                 to select a specific camera, or use 'default' for the system default camera."
            )
            .with_output(PortDescriptor::new(
                "video",
                Arc::clone(&SCHEMA_VIDEO_FRAME),
                true,
                "Live video frames from the camera. Each frame is a WebGPU texture with timestamp \
                 and metadata. Frames are produced at the camera's native frame rate (typically 30 or 60 FPS)."
            ))
            .with_example(ProcessorExample::new(
                "Basic camera capture (default device)",
                serde_json::json!({
                    "code": "from streamlib import camera_processor\n\n@camera_processor()\ndef camera():\n    \"\"\"Zero-copy camera source - no code needed!\"\"\"\n    pass",
                    "language": "python"
                }),
                serde_json::json!({})
            ))
            .with_example(ProcessorExample::new(
                "Specific camera device",
                serde_json::json!({
                    "code": "from streamlib import camera_processor\n\n@camera_processor(device_id=\"0x1424001bcf2284\")\ndef camera():\n    \"\"\"Zero-copy camera source with specific device\"\"\"\n    pass",
                    "language": "python"
                }),
                serde_json::json!({})
            ))
            .with_example(ProcessorExample::new(
                "Complete pipeline: Camera → Display (MCP workflow)",
                serde_json::json!({
                    "steps": [
                        {
                            "action": "add_processor",
                            "language": "python",
                            "code": "from streamlib import camera_processor\n\n@camera_processor(device_id=\"0x1424001bcf2284\")\ndef camera():\n    pass",
                            "result": "processor_0"
                        },
                        {
                            "action": "add_processor",
                            "language": "python",
                            "code": "from streamlib import display_processor\n\n@display_processor()\ndef display():\n    pass",
                            "result": "processor_1"
                        },
                        {
                            "action": "connect_processors",
                            "source": "processor_0.video",
                            "destination": "processor_1.video",
                            "note": "Connect camera OUTPUT port to display INPUT port. Ports are compatible because both use VideoFrame schema."
                        }
                    ]
                }),
                serde_json::json!({})
            ))
            .with_tags(vec!["source", "camera", "video", "input", "capture"])
        )
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn set_wakeup_channel(&mut self, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        // Store in global wakeup channel (shared with AVFoundation delegate)
        if let Some(wakeup_storage) = WAKEUP_CHANNEL.get() {
            *wakeup_storage.lock() = Some(wakeup_tx);
            tracing::debug!("CameraProcessor: Push-based wakeup enabled (AVFoundation callback will trigger processing)");
        }
    }
}

// Auto-register CameraProcessor with global registry
crate::register_processor_type!(AppleCameraProcessor);
