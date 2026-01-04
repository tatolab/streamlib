// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::apple::{iosurface, MetalDevice};
use crate::core::{
    LinkOutput, Result, RuntimeContext, StreamError, TexturePool, TexturePoolDescriptor, VideoFrame,
};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send};
use objc2_av_foundation::{
    AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureSession,
    AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
};
use objc2_core_video::CVPixelBuffer;
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};
use objc2_io_surface::IOSurface;
use parking_lot::Mutex;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// Apple-specific configuration and device types
#[derive(
    Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default, crate::ConfigDescriptor,
)]
pub struct AppleCameraConfig {
    pub device_id: Option<String>,
}

impl From<()> for AppleCameraConfig {
    fn from(_: ()) -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone)]
pub struct AppleCameraDevice {
    pub id: String,
    pub name: String,
}

type CMSampleBufferRef = *mut c_void;

#[link(name = "CoreMedia", kind = "framework")]
#[allow(clashing_extern_declarations)]
extern "C" {
    fn CMSampleBufferGetImageBuffer(sbuf: CMSampleBufferRef) -> *mut CVPixelBuffer;
    fn CVPixelBufferGetIOSurface(pixelBuffer: *const CVPixelBuffer) -> *mut IOSurface;
    fn CVPixelBufferGetWidth(pixelBuffer: *const CVPixelBuffer) -> usize;
    fn CVPixelBufferGetHeight(pixelBuffer: *const CVPixelBuffer) -> usize;
}

/// Shared state for AVFoundation initialization (async pattern).
struct CaptureSessionInitState {
    /// Set to true when AVFoundation init completes successfully.
    ready: AtomicBool,
    /// Set to true if AVFoundation init failed.
    failed: AtomicBool,
    /// Camera name, populated on success.
    camera_name: Mutex<Option<String>>,
    /// Error message if init failed.
    error_message: Mutex<Option<String>>,
}

#[allow(dead_code)]
impl CaptureSessionInitState {
    fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            camera_name: Mutex::new(None),
            error_message: Mutex::new(None),
        }
    }

    fn mark_ready(&self, camera_name: String) {
        *self.camera_name.lock() = Some(camera_name);
        self.ready.store(true, Ordering::Release);
    }

    fn mark_failed(&self, error: String) {
        *self.error_message.lock() = Some(error);
        self.failed.store(true, Ordering::Release);
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    fn take_camera_name(&self) -> Option<String> {
        self.camera_name.lock().take()
    }

    fn take_error(&self) -> Option<String> {
        self.error_message.lock().take()
    }
}

/// Callback context for processing frames directly in AVFoundation callback
struct CameraCallbackContext {
    metal_device: MetalDevice,
    metal_command_queue: metal::CommandQueue,
    video_output: LinkOutput<VideoFrame>,
    frame_count: std::sync::atomic::AtomicU64,
    /// Texture pool for acquiring IOSurface-backed output textures.
    texture_pool: TexturePool,
}

/// Global callback context - set by start(), used by AVFoundation callback
static CAMERA_CALLBACK_CONTEXT: std::sync::OnceLock<Arc<CameraCallbackContext>> =
    std::sync::OnceLock::new();

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
            // NOTE: Cannot use tracing here - this runs on AVFoundation's dispatch queue
            // Get callback context
            let Some(ctx) = CAMERA_CALLBACK_CONTEXT.get() else {
                return;
            };

            let pixel_buffer_ref = CMSampleBufferGetImageBuffer(sample_buffer);
            if pixel_buffer_ref.is_null() {
                return;
            }

            // Get IOSurface from pixel buffer
            let iosurface_ref = CVPixelBufferGetIOSurface(pixel_buffer_ref);
            if iosurface_ref.is_null() {
                return;
            }

            let iosurface = match Retained::retain(iosurface_ref) {
                Some(s) => s,
                None => return,
            };

            let width = CVPixelBufferGetWidth(pixel_buffer_ref);
            let height = CVPixelBufferGetHeight(pixel_buffer_ref);

            // Create Metal texture from IOSurface (BGRA format)
            let metal_texture = match iosurface::create_metal_texture_from_iosurface(
                ctx.metal_device.device(),
                &iosurface,
                0,
            ) {
                Ok(tex) => tex,
                Err(_) => return,
            };

            // Acquire RGBA output texture from pool (IOSurface-backed)
            let pooled_handle = match ctx.texture_pool.acquire(&TexturePoolDescriptor {
                width: width as u32,
                height: height as u32,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                label: Some("camera_output"),
            }) {
                Ok(handle) => handle,
                Err(_) => return,
            };

            // Blit BGRA → RGBA (to pool's Metal texture)
            use metal::foreign_types::ForeignTypeRef;

            let source_texture_ptr = &*metal_texture as *const _ as *mut std::ffi::c_void;
            let source_texture_ref = metal::TextureRef::from_ptr(source_texture_ptr as *mut _);

            let pool_metal_texture = pooled_handle.metal_texture();
            let dest_texture_ptr = pool_metal_texture as *const _ as *mut std::ffi::c_void;
            let dest_texture_ref = metal::TextureRef::from_ptr(dest_texture_ptr as *mut _);

            let command_buffer = ctx.metal_command_queue.new_command_buffer();
            let blit_encoder = command_buffer.new_blit_command_encoder();

            use metal::MTLOrigin;
            use metal::MTLSize;

            let origin = MTLOrigin { x: 0, y: 0, z: 0 };
            let size = MTLSize {
                width: width as u64,
                height: height as u64,
                depth: 1,
            };

            blit_encoder.copy_from_texture(
                source_texture_ref,
                0,
                0,
                origin,
                size,
                dest_texture_ref,
                0,
                0,
                origin,
            );

            blit_encoder.end_encoding();
            command_buffer.commit();
            command_buffer.wait_until_completed();

            let timestamp_ns = crate::core::media_clock::MediaClock::now().as_nanos() as i64;
            let frame_num = ctx.frame_count.fetch_add(1, Ordering::Relaxed);

            let frame = VideoFrame::from_pooled(pooled_handle, timestamp_ns, frame_num);

            // Write frame to output
            ctx.video_output.write(frame);

            if frame_num == 0 {
                eprintln!("[Camera] AVFoundation: First frame processed");
            } else if frame_num % 60 == 0 {
                eprintln!("[Camera] AVFoundation: Frame #{}", frame_num);
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

#[crate::processor(
    name = "CameraProcessor",
    execution = Manual,
    description = "Captures video from macOS cameras using AVFoundation"
)]
pub struct AppleCameraProcessor {
    #[crate::output(description = "Live video frames from the camera")]
    video: LinkOutput<VideoFrame>,

    #[crate::config]
    config: AppleCameraConfig,

    gpu_context: Option<crate::core::GpuContext>,
    camera_name: String,
    /// Async init state - None means init not started yet.
    capture_init_state: Option<Arc<CaptureSessionInitState>>,
}

impl crate::core::ManualProcessor for AppleCameraProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.gpu_context = Some(ctx.gpu.clone());
        tracing::info!("Camera: setup() complete");
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        let frame_count = CAMERA_CALLBACK_CONTEXT
            .get()
            .map(|ctx| ctx.frame_count.load(Ordering::Relaxed))
            .unwrap_or(0);
        tracing::info!(
            "Camera {}: Teardown (generated {} frames)",
            self.camera_name,
            frame_count
        );
        std::future::ready(Ok(()))
    }

    // Callback-driven start - initializes Metal and AVFoundation, returns immediately
    fn start(&mut self) -> Result<()> {
        tracing::trace!("Camera: start() called - setting up callback-driven capture");

        // Step 1: Initialize Metal resources
        let metal_device = MetalDevice::new()?;
        let metal_command_queue = {
            use metal::foreign_types::ForeignTypeRef;
            let device_ptr = metal_device.device() as *const _ as *mut std::ffi::c_void;
            let metal_device_ref = unsafe { metal::DeviceRef::from_ptr(device_ptr as *mut _) };
            metal_device_ref.new_command_queue()
        };

        let gpu_context = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;

        // Step 2: Create and set callback context
        let callback_context = Arc::new(CameraCallbackContext {
            metal_device,
            metal_command_queue,
            video_output: self.video.clone(),
            frame_count: std::sync::atomic::AtomicU64::new(0),
            texture_pool: gpu_context.texture_pool().clone(),
        });

        // Store in global - callback will read from here
        let _ = CAMERA_CALLBACK_CONTEXT.set(callback_context);

        // Step 3: Dispatch AVFoundation init to main thread
        let init_state = Arc::new(CaptureSessionInitState::new());
        self.capture_init_state = Some(Arc::clone(&init_state));

        let config = self.config.clone();

        use dispatch2::DispatchQueue;
        DispatchQueue::main().exec_async(move || {
            // SAFETY: This closure executes on the main thread via GCD
            let mtm = unsafe { MainThreadMarker::new_unchecked() };
            Self::initialize_capture_session_on_main_thread(mtm, &config, init_state);
        });

        tracing::info!("Camera: Callback-driven capture started");
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        tracing::trace!("Camera: stop() called");

        // TODO: Stop AVCaptureSession - requires keeping a reference to it
        // For now, the session continues until process exit

        // Brief wait for in-flight callbacks
        std::thread::sleep(std::time::Duration::from_millis(50));

        let frame_count = CAMERA_CALLBACK_CONTEXT
            .get()
            .map(|ctx| ctx.frame_count.load(Ordering::Relaxed))
            .unwrap_or(0);

        tracing::info!(
            "Camera {}: Stopped ({} frames)",
            self.camera_name,
            frame_count
        );
        Ok(())
    }
}

impl AppleCameraProcessor::Processor {
    /// Initialize AVFoundation capture session on main thread.
    /// Called via dispatch to main queue - MUST NOT block or use tracing.
    fn initialize_capture_session_on_main_thread(
        mtm: MainThreadMarker,
        config: &AppleCameraConfig,
        init_state: Arc<CaptureSessionInitState>,
    ) {
        // All errors are reported via init_state, not returned
        let result = Self::do_initialize_capture_session(mtm, config);
        match result {
            Ok(camera_name) => {
                eprintln!("[Camera] AVFoundation session started: {}", camera_name);
                init_state.mark_ready(camera_name);
            }
            Err(e) => {
                eprintln!("[Camera] AVFoundation init FAILED: {}", e);
                init_state.mark_failed(e.to_string());
            }
        }
    }

    /// Internal init logic, returns Result for cleaner error handling.
    fn do_initialize_capture_session(
        mtm: MainThreadMarker,
        config: &AppleCameraConfig,
    ) -> Result<String> {
        let session = unsafe { AVCaptureSession::new() };

        unsafe {
            session.beginConfiguration();
        }

        let device = unsafe {
            if let Some(ref id) = config.device_id {
                let id_str = NSString::from_str(id);
                let dev = AVCaptureDevice::deviceWithUniqueID(&id_str);
                if dev.is_none() {
                    return Err(StreamError::Configuration(format!(
                        "Camera not found with ID: {}",
                        id
                    )));
                }
                dev.unwrap()
            } else {
                let media_type = AVMediaTypeVideo.ok_or_else(|| {
                    StreamError::Configuration("AVMediaTypeVideo not available".into())
                })?;

                AVCaptureDevice::defaultDeviceWithMediaType(media_type)
                    .ok_or_else(|| StreamError::Configuration("No camera found".into()))?
            }
        };

        unsafe {
            if let Err(e) = device.lockForConfiguration() {
                return Err(StreamError::Configuration(format!(
                    "Failed to lock camera device: {:?}",
                    e
                )));
            }
            device.unlockForConfiguration();
        }

        let input = unsafe {
            AVCaptureDeviceInput::deviceInputWithDevice_error(&device).map_err(|e| {
                StreamError::Configuration(format!("Failed to create camera input: {:?}", e))
            })?
        };

        let can_add = unsafe { session.canAddInput(&input) };
        if !can_add {
            return Err(StreamError::Configuration(
                "Session cannot add camera input. Camera may be in use.".into(),
            ));
        }

        unsafe {
            session.addInput(&input);
        }

        let output = unsafe { AVCaptureVideoDataOutput::new() };

        use objc2_foundation::NSNumber;

        let pixel_format_key = unsafe { objc2_core_video::kCVPixelBufferPixelFormatTypeKey };
        let pixel_format_value = NSNumber::new_u32(0x42475241); // BGRA

        use objc2::runtime::AnyClass;
        use objc2::ClassType;
        let dict_cls: &AnyClass = objc2_foundation::NSDictionary::<
            objc2::runtime::AnyObject,
            objc2::runtime::AnyObject,
        >::class();

        let key_ptr = pixel_format_key as *const _ as *const objc2::runtime::AnyObject;
        let value_ptr = &*pixel_format_value as *const _ as *const objc2::runtime::AnyObject;

        let video_settings_ptr: *mut objc2::runtime::AnyObject =
            unsafe { msg_send![dict_cls, dictionaryWithObject: value_ptr, forKey: key_ptr] };

        unsafe {
            let _: () = msg_send![&output, setVideoSettings: video_settings_ptr];
        }

        let delegate = CameraDelegate::new(mtm);

        unsafe {
            use dispatch2::{DispatchQueue, DispatchQueueAttr};
            let queue = DispatchQueue::new("com.streamlib.camera.video", DispatchQueueAttr::SERIAL);

            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&queue),
            );
        }

        let can_add_output = unsafe { session.canAddOutput(&output) };
        if !can_add_output {
            return Err(StreamError::Configuration(
                "Cannot add camera output".into(),
            ));
        }

        unsafe {
            session.addOutput(&output);
            session.commitConfiguration();
        }

        let camera_name = unsafe { device.localizedName().to_string() };

        unsafe {
            session.startRunning();
        }

        // Leak ObjC objects to keep them alive
        let _ = Retained::into_raw(session);
        let _ = Retained::into_raw(device);
        let _ = Retained::into_raw(delegate);

        Ok(camera_name)
    }

    // Helper methods
    pub fn list_devices() -> Result<Vec<AppleCameraDevice>> {
        unsafe {
            use objc2_av_foundation::AVCaptureDeviceDiscoverySession;
            use objc2_foundation::NSArray;

            let media_type = AVMediaTypeVideo.ok_or_else(|| {
                StreamError::Configuration("AVMediaTypeVideo not available".into())
            })?;

            let builtin_wide =
                objc2_foundation::ns_string!("AVCaptureDeviceTypeBuiltInWideAngleCamera");
            let continuity = objc2_foundation::ns_string!("AVCaptureDeviceTypeContinuityCamera");

            let device_types = NSArray::from_slice(&[builtin_wide, continuity]);

            let session =
                AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
                    &device_types,
                    Some(media_type),
                    objc2_av_foundation::AVCaptureDevicePosition::Unspecified,
                );

            let devices = session.devices();
            let mut result = Vec::new();
            for i in 0..devices.count() {
                let device = devices.objectAtIndex(i);
                result.push(AppleCameraDevice {
                    id: device.uniqueID().to_string(),
                    name: device.localizedName().to_string(),
                });
            }

            Ok(result)
        }
    }
}
