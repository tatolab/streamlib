// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::apple::corevideo_ffi::{
    CVPixelBufferGetHeight, CVPixelBufferGetIOSurface, CVPixelBufferGetWidth, IOSurfaceGetID,
};
use crate::core::rhi::{PixelFormat, RhiPixelBuffer, RhiPixelBufferRef};
use crate::core::{GpuContext, Result, RuntimeContext, StreamError};
use crate::iceoryx2::OutputWriter;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send};
use objc2_av_foundation::{
    AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureSession,
    AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
};
use objc2_core_video::CVPixelBuffer;
use objc2_foundation::{MainThreadMarker, NSArray, NSObject, NSObjectProtocol, NSString};
use parking_lot::Mutex;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// Config type is generated from JTD schema
pub use crate::_generated_::CameraConfig;

#[derive(Debug, Clone)]
pub struct AppleCameraDevice {
    pub id: String,
    pub name: String,
}

type CMSampleBufferRef = *mut c_void;

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetImageBuffer(sbuf: CMSampleBufferRef) -> *mut CVPixelBuffer;
}

/// CMTime structure for setting frame duration on AVCaptureDevice.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

// Safety: CMTime is a plain C struct with known layout, matching Apple's CMTime definition.
unsafe impl objc2::Encode for CMTime {
    const ENCODING: objc2::Encoding = objc2::Encoding::Struct(
        "?",
        &[
            objc2::Encoding::LongLong, // value: i64
            objc2::Encoding::Int,      // timescale: i32
            objc2::Encoding::UInt,     // flags: u32
            objc2::Encoding::LongLong, // epoch: i64
        ],
    );
}

// Safety: CMTime contains only primitive types, safe to pass via FFI.
unsafe impl objc2::RefEncode for CMTime {
    const ENCODING_REF: objc2::Encoding =
        objc2::Encoding::Pointer(&<Self as objc2::Encode>::ENCODING);
}

impl CMTime {
    /// Create a CMTime representing 1/fps seconds (frame duration for given fps).
    fn from_fps(fps: f64) -> Self {
        // Use timescale of 600 for smooth representation of common frame rates
        let timescale: i32 = 600;
        let value = (timescale as f64 / fps).round() as i64;
        Self {
            value,
            timescale,
            flags: 1, // kCMTimeFlags_Valid
            epoch: 0,
        }
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGDisplayCopyDisplayMode(display: u32) -> *const c_void;
    fn CGDisplayModeGetRefreshRate(mode: *const c_void) -> f64;
    fn CGDisplayModeRelease(mode: *const c_void);
}

/// Get the refresh rate of the main display in Hz.
/// Returns 60.0 as fallback if detection fails.
fn get_main_display_refresh_rate() -> f64 {
    unsafe {
        let display_id = CGMainDisplayID();
        let mode = CGDisplayCopyDisplayMode(display_id);
        if mode.is_null() {
            return 60.0;
        }
        let rate = CGDisplayModeGetRefreshRate(mode);
        CGDisplayModeRelease(mode);
        // Some displays report 0 for "as fast as possible" - default to 60
        if rate <= 0.0 {
            60.0
        } else {
            rate
        }
    }
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

/// Callback context for processing frames directly in AVFoundation callback.
///
/// # Safety
/// The `output_writer` pointer must remain valid for the lifetime of this context.
/// This is guaranteed by holding `_outputs_arc` which keeps the Arc alive.
struct CameraCallbackContext {
    output_writer: *const OutputWriter,
    gpu_context: crate::core::GpuContext,
    frame_count: std::sync::atomic::AtomicU64,
    /// Holds the Arc to keep the OutputWriter alive while the pointer is in use.
    _outputs_arc: Arc<OutputWriter>,
}

// SAFETY: OutputWriter is Sync, and the pointer is only dereferenced while valid
unsafe impl Send for CameraCallbackContext {}
unsafe impl Sync for CameraCallbackContext {}

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

            let pixel_buffer_ptr = CMSampleBufferGetImageBuffer(sample_buffer);
            if pixel_buffer_ptr.is_null() {
                return;
            }

            // Get IOSurface for cross-process sharing
            let camera_iosurface =
                CVPixelBufferGetIOSurface(pixel_buffer_ptr as *mut std::ffi::c_void);
            if camera_iosurface.is_null() {
                eprintln!("[Camera] CVPixelBuffer not backed by IOSurface");
                return;
            }

            // Get dimensions from CVPixelBuffer
            let width = CVPixelBufferGetWidth(pixel_buffer_ptr as *mut _) as u32;
            let height = CVPixelBufferGetHeight(pixel_buffer_ptr as *mut _) as u32;

            let frame_num = ctx.frame_count.fetch_add(1, Ordering::Relaxed);

            // Acquire pooled buffer from GpuContext (pool is managed centrally)
            let surface_id_str =
                match ctx
                    .gpu_context
                    .acquire_pixel_buffer(width, height, PixelFormat::Bgra32)
                {
                    Ok((pool_id, pooled_buffer)) => {
                        // GPU blit from camera IOSurface to pooled IOSurface
                        match blit_iosurface_to_pooled_buffer(
                            ctx,
                            camera_iosurface,
                            &pooled_buffer,
                            width,
                            height,
                        ) {
                            Ok(()) => {
                                // Use the PixelBufferPoolId directly - no need to extract IOSurfaceID
                                // pooled_buffer is dropped here, releasing it back to the pool
                                pool_id.to_string()
                            }
                            Err(e) => {
                                if frame_num == 0 {
                                    eprintln!("[Camera] Blit failed: {}, falling back", e);
                                }
                                // Blit failed, fall back to direct forwarding
                                forward_camera_iosurface_directly(ctx, camera_iosurface)
                            }
                        }
                    }
                    Err(e) => {
                        if frame_num == 0 {
                            eprintln!("[Camera] Pool acquire failed: {}, falling back", e);
                        }
                        // Pool exhausted or error, fall back to direct forwarding
                        forward_camera_iosurface_directly(ctx, camera_iosurface)
                    }
                };

            let timestamp_ns = crate::core::media_clock::MediaClock::now().as_nanos() as i64;

            // Create IPC frame with surface_id as string
            // The receiving process will use check_out_surface() or IOSurfaceLookup(id) to access the surface
            let ipc_frame = crate::_generated_::Videoframe {
                surface_id: surface_id_str,
                width,
                height,
                timestamp_ns: timestamp_ns.to_string(),
                frame_index: frame_num.to_string(),
            };

            // Write IPC frame to output via iceoryx2
            // SAFETY: output_writer pointer is valid while callback context exists
            let outputs = &*ctx.output_writer;
            if let Err(e) = outputs.write("video", &ipc_frame) {
                eprintln!("[Camera] Failed to write frame: {}", e);
                return;
            }

            if frame_num == 0 {
                eprintln!("[Camera] AVFoundation: First frame processed (pooled buffers)");
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

/// GPU blit from camera IOSurface to pooled buffer's IOSurface.
///
/// This copies the camera frame data to a pooled buffer with a stable IOSurface ID,
/// avoiding the creation of thousands of unique IOSurfaces over time.
///
/// # Safety
/// - `camera_iosurface` must be a valid IOSurfaceRef
/// - Called from AVFoundation callback context
unsafe fn blit_iosurface_to_pooled_buffer(
    ctx: &CameraCallbackContext,
    camera_iosurface: crate::apple::corevideo_ffi::IOSurfaceRef,
    pooled_buffer: &RhiPixelBuffer,
    width: u32,
    height: u32,
) -> crate::core::Result<()> {
    ctx.gpu_context
        .blit_copy_iosurface(camera_iosurface, pooled_buffer, width, height)
}

/// Fall back to direct IOSurface forwarding (old behavior).
///
/// Used when pool is not available or blit fails.
unsafe fn forward_camera_iosurface_directly(
    ctx: &CameraCallbackContext,
    camera_iosurface: crate::apple::corevideo_ffi::IOSurfaceRef,
) -> String {
    match RhiPixelBufferRef::from_iosurface_ref(camera_iosurface) {
        Ok(pixel_buffer_ref) => {
            let pixel_buffer = RhiPixelBuffer::new(pixel_buffer_ref);
            match ctx.gpu_context.check_in_surface(&pixel_buffer) {
                Ok(id) => id,
                Err(_) => {
                    // Surface store not available, fall back to raw IOSurface ID
                    IOSurfaceGetID(camera_iosurface).to_string()
                }
            }
        }
        Err(_) => {
            // Failed to create pixel buffer, use raw IOSurface ID
            IOSurfaceGetID(camera_iosurface).to_string()
        }
    }
}

#[crate::processor("src/apple/processors/camera.yaml")]
pub struct AppleCameraProcessor {
    camera_name: String,
    /// Async init state - None means init not started yet.
    capture_init_state: Option<Arc<CaptureSessionInitState>>,
    /// GPU context for surface store access (set in setup, used in start).
    gpu_context: Option<GpuContext>,
}

impl crate::core::ManualProcessor for AppleCameraProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        // Store GPU context for surface store access in start()
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

    // Callback-driven start - initializes AVFoundation, returns immediately
    fn start(&mut self) -> Result<()> {
        tracing::trace!("Camera: start() called - setting up callback-driven capture");

        // Get GPU context (set during setup)
        let gpu_context = self.gpu_context.clone().ok_or_else(|| {
            StreamError::Configuration("GPU context not initialized. Call setup() first.".into())
        })?;

        // Create callback context with pointer to OutputWriter and GPU context
        // SAFETY: The processor outlives the callback context (stop() clears it before drop)
        // Clone the Arc to get a reference we can convert to a pointer
        let outputs_arc: Arc<OutputWriter> = self.outputs.clone();
        let output_writer_ptr = Arc::as_ptr(&outputs_arc);
        let callback_context = Arc::new(CameraCallbackContext {
            output_writer: output_writer_ptr,
            gpu_context,
            frame_count: std::sync::atomic::AtomicU64::new(0),
            // Keep the Arc alive to ensure the pointer remains valid
            _outputs_arc: outputs_arc,
        });

        // Store in global - callback will read from here
        let _ = CAMERA_CALLBACK_CONTEXT.set(callback_context);

        // Dispatch AVFoundation init to main thread
        let init_state = Arc::new(CaptureSessionInitState::new());
        self.capture_init_state = Some(Arc::clone(&init_state));

        let config = self.config.clone();

        use dispatch2::DispatchQueue;
        DispatchQueue::main().exec_async(move || {
            // SAFETY: This closure executes on the main thread via GCD
            let mtm = unsafe { MainThreadMarker::new_unchecked() };
            Self::initialize_capture_session_on_main_thread(mtm, &config, init_state);
        });

        tracing::info!("Camera: Callback-driven capture started (buffer-centric)");
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
        config: &CameraConfig,
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
        config: &CameraConfig,
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

        // Configure frame rate based on config, display refresh rate, and camera capabilities
        unsafe {
            if let Err(e) = device.lockForConfiguration() {
                return Err(StreamError::Configuration(format!(
                    "Failed to lock camera device: {:?}",
                    e
                )));
            }

            // Get frame rate settings from config (default: 60fps min, display refresh rate max)
            let requested_min_fps = config.min_fps.unwrap_or(60.0);
            let requested_max_fps = config.max_fps.unwrap_or_else(get_main_display_refresh_rate);

            // Query camera's supported frame rate range from active format
            let active_format = device.activeFormat();
            let frame_rate_ranges: Retained<NSArray<AnyObject>> =
                msg_send![&active_format, videoSupportedFrameRateRanges];

            // Find the maximum supported fps from all ranges
            let mut camera_max_fps: f64 = 30.0; // Default fallback
            let mut camera_min_fps: f64 = 1.0;
            for i in 0..frame_rate_ranges.count() {
                let range = frame_rate_ranges.objectAtIndex(i);
                let range_max: f64 = msg_send![&range, maxFrameRate];
                let range_min: f64 = msg_send![&range, minFrameRate];
                if range_max > camera_max_fps {
                    camera_max_fps = range_max;
                }
                if range_min < camera_min_fps || i == 0 {
                    camera_min_fps = range_min;
                }
            }

            // Clamp requested fps to camera's supported range
            let min_fps = requested_min_fps.max(camera_min_fps).min(camera_max_fps);
            let max_fps = requested_max_fps.max(camera_min_fps).min(camera_max_fps);

            eprintln!(
                "[Camera] Frame rate: requested {:.0}-{:.0} fps, camera supports {:.0}-{:.0} fps, using {:.0}-{:.0} fps",
                requested_min_fps, requested_max_fps,
                camera_min_fps, camera_max_fps,
                min_fps, max_fps
            );

            // Min frame duration = max fps (shorter duration = more frames)
            // Max frame duration = min fps (longer duration = fewer frames)
            let min_duration = CMTime::from_fps(max_fps);
            let max_duration = CMTime::from_fps(min_fps);

            // Set frame duration range on device
            let _: () = msg_send![&device, setActiveVideoMinFrameDuration: min_duration];
            let _: () = msg_send![&device, setActiveVideoMaxFrameDuration: max_duration];

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

        // Pool creation is deferred to first frame callback where we have CVPixelBuffer dimensions

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
