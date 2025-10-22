//! Apple Camera Processor - Real AVFoundation Capture
//!
//! Zero-copy pipeline: CVPixelBuffer → IOSurface → Metal Texture → WebGPU Texture

use streamlib_core::{
    StreamProcessor, CameraProcessor, CameraOutputPorts, CameraDevice,
    VideoFrame, TimedTick, Result, StreamError,
};
use std::sync::{Arc, Mutex};
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
use crate::{WgpuBridge, MetalDevice, iosurface};

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
                let mut latest = storage.lock().unwrap();
                *latest = Some(frame_holder);
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

    // GPU bridge and device
    wgpu_bridge: Option<Arc<WgpuBridge>>,
    metal_device: Option<MetalDevice>,

    // Capture session info (for logging)
    #[allow(dead_code)] // Stored for future logging/diagnostics
    camera_name: String,

    // Delegate (must be kept alive to prevent deallocation)
    #[allow(dead_code)]
    delegate: Option<Retained<CameraDelegate>>,
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

        // Create Metal device and WebGPU bridge
        let metal_device = MetalDevice::new()?;
        eprintln!("Camera: Created Metal device");

        // Create WgpuBridge for zero-copy Metal → WebGPU conversion
        let wgpu_bridge = pollster::block_on(async {
            WgpuBridge::new(metal_device.clone_device()).await
        })?;
        eprintln!("Camera: Created WebGPU bridge");

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
                video: streamlib_core::StreamOutput::new("video"),
            },
            frame_count: 0,
            latest_frame,
            wgpu_bridge: Some(Arc::new(wgpu_bridge)),
            metal_device: Some(metal_device),
            camera_name,
            delegate: Some(delegate),
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
    fn process(&mut self, tick: TimedTick) -> Result<()> {
        // Try to get the latest frame from the delegate
        let frame_holder = {
            let mut latest = self.latest_frame.lock().unwrap();
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

                // Convert Metal texture to WebGPU texture (zero-copy via wgpu-hal)
                let wgpu_bridge = self.wgpu_bridge.as_ref()
                    .ok_or_else(|| StreamError::Configuration("No WebGPU bridge".into()))?;

                let wgpu_texture = wgpu_bridge.wrap_metal_texture(
                    &metal_texture,
                    wgpu::TextureFormat::Bgra8Unorm,
                    wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                )?;

                // Create VideoFrame with WebGPU texture
                let frame = VideoFrame::new(
                    Arc::new(wgpu_texture),
                    tick.timestamp,
                    self.frame_count,
                    width as u32,
                    height as u32,
                );

                self.ports.video.write(frame);
                self.frame_count += 1;

                Ok(())
            };

            if let Err(e) = result {
                tracing::error!("Camera: Error processing frame: {:?}", e);
                return Err(e);
            }
        }

        Ok(())
    }

    fn on_start(&mut self) -> Result<()> {
        tracing::info!("Camera: Processor started (capture session already running)");
        Ok(())
    }

    fn on_stop(&mut self) -> Result<()> {
        tracing::info!("Camera: Processor stopped (generated {} frames)", self.frame_count);
        // Note: AVCaptureSession continues running on main thread
        // TODO: Implement proper session lifecycle management
        Ok(())
    }
}
