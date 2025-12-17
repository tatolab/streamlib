// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::apple::{iosurface, MetalDevice, WgpuBridge};
use crate::core::{LinkOutput, Result, RuntimeContext, StreamError, VideoFrame};
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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
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

struct FrameHolder {
    pixel_buffer: Retained<CVPixelBuffer>,
}

unsafe impl Send for FrameHolder {}
unsafe impl Sync for FrameHolder {}

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

static FRAME_STORAGE: std::sync::OnceLock<Arc<Mutex<Option<FrameHolder>>>> =
    std::sync::OnceLock::new();

static LINK_OUTPUT_TO_PROCESSOR_WRITER_AND_READER: std::sync::OnceLock<
    Arc<Mutex<Option<crossbeam_channel::Sender<crate::core::links::LinkOutputToProcessorMessage>>>>,
> = std::sync::OnceLock::new();

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
            use std::sync::atomic::{AtomicU64, Ordering};
            static CALLBACK_COUNT: AtomicU64 = AtomicU64::new(0);
            let count = CALLBACK_COUNT.fetch_add(1, Ordering::Relaxed);

            if count == 0 {
                eprintln!("[Camera] AVFoundation: First frame received");
            } else if count.is_multiple_of(300) {
                eprintln!("[Camera] AVFoundation: Frame #{}", count);
            }

            let pixel_buffer_ref = CMSampleBufferGetImageBuffer(sample_buffer);
            if pixel_buffer_ref.is_null() {
                return;
            }

            let pixel_buffer = Retained::retain(pixel_buffer_ref as *mut CVPixelBuffer)
                .expect("Failed to retain pixel buffer");

            if let Some(storage) = FRAME_STORAGE.get() {
                let frame_holder = FrameHolder {
                    pixel_buffer: pixel_buffer.clone(),
                };
                let mut latest = storage.lock();
                *latest = Some(frame_holder);

                if let Some(message_writer_storage) =
                    LINK_OUTPUT_TO_PROCESSOR_WRITER_AND_READER.get()
                {
                    if let Some(writer) = message_writer_storage.lock().as_ref() {
                        let _ = writer.send(
                            crate::core::links::LinkOutputToProcessorMessage::InvokeProcessingNow,
                        );
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

    frame_count: u64,
    latest_frame: Arc<Mutex<Option<FrameHolder>>>,
    gpu_context: Option<crate::core::GpuContext>,
    metal_device: Option<MetalDevice>,
    wgpu_bridge: Option<Arc<WgpuBridge>>,
    camera_name: String,
    metal_command_queue: Option<metal::CommandQueue>,
    /// Async init state - None means init not started yet.
    capture_init_state: Option<Arc<CaptureSessionInitState>>,
    /// Whether we've dispatched the AVFoundation init to main thread.
    avfoundation_init_dispatched: bool,
}

impl AppleCameraProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());
        tracing::info!("Camera: setup() complete, will initialize AVFoundation in process()");
        Ok(())
    }

    /// Initialize AVFoundation capture session on main thread.
    /// Called via dispatch to main queue - MUST NOT block or use tracing.
    fn initialize_capture_session_on_main_thread(
        mtm: MainThreadMarker,
        config: &AppleCameraConfig,
        latest_frame: Arc<Mutex<Option<FrameHolder>>>,
        init_state: Arc<CaptureSessionInitState>,
    ) {
        // All errors are reported via init_state, not returned
        let result = Self::do_initialize_capture_session(mtm, config, latest_frame);
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
        latest_frame: Arc<Mutex<Option<FrameHolder>>>,
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

        let _ = FRAME_STORAGE.set(latest_frame);

        let message_writer_holder: Arc<
            Mutex<
                Option<crossbeam_channel::Sender<crate::core::links::LinkOutputToProcessorMessage>>,
            >,
        > = Arc::new(Mutex::new(None));
        let _ = LINK_OUTPUT_TO_PROCESSOR_WRITER_AND_READER.set(message_writer_holder.clone());

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

    fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            "Camera {}: Stopping (generated {} frames)",
            self.camera_name,
            self.frame_count
        );
        Ok(())
    }

    /// Initialize Metal resources (can run on any thread).
    fn initialize_metal_resources(&mut self) -> Result<()> {
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

        let wgpu_bridge = Arc::new(WgpuBridge::from_shared_device(
            metal_device.clone_device(),
            gpu_context.device().as_ref().clone(),
            gpu_context.queue().as_ref().clone(),
        ));

        self.wgpu_bridge = Some(wgpu_bridge);
        self.metal_command_queue = Some(metal_command_queue);
        self.metal_device = Some(metal_device);

        Ok(())
    }

    // Business logic - called by macro-generated process()
    // Manual mode: called once, sets up camera and enters frame processing loop
    fn process(&mut self) -> Result<()> {
        // Step 1: Dispatch AVFoundation init to main queue (non-blocking)
        if !self.avfoundation_init_dispatched {
            tracing::info!("Camera: Dispatching AVFoundation init to main thread (non-blocking)");

            let init_state = Arc::new(CaptureSessionInitState::new());
            self.capture_init_state = Some(Arc::clone(&init_state));
            self.avfoundation_init_dispatched = true;

            let config = self.config.clone();
            let latest_frame = self.latest_frame.clone();

            use dispatch2::DispatchQueue;
            DispatchQueue::main().exec_async(move || {
                // SAFETY: This closure executes on the main thread via GCD
                let mtm = unsafe { MainThreadMarker::new_unchecked() };
                Self::initialize_capture_session_on_main_thread(
                    mtm,
                    &config,
                    latest_frame,
                    init_state,
                );
            });

            tracing::info!("Camera: AVFoundation init dispatched, continuing without blocking");
        }

        // Step 2: Initialize Metal resources (can happen in parallel, doesn't need main thread)
        if self.metal_device.is_none() {
            tracing::info!("Camera: Initializing Metal resources");
            self.initialize_metal_resources()?;
            tracing::info!("Camera: Metal resources initialized");
        }

        // Step 3: Enter frame processing loop
        use crate::core::{shutdown_aware_loop, LoopControl};

        let mut loop_iteration = 0u64;
        let mut avfoundation_ready = false;

        shutdown_aware_loop(|| {
            loop_iteration += 1;

            // Check if AVFoundation init completed (only until it's ready)
            if !avfoundation_ready {
                if let Some(ref init_state) = self.capture_init_state {
                    if init_state.is_failed() {
                        let error = init_state
                            .take_error()
                            .unwrap_or_else(|| "Unknown error".into());
                        return Err(StreamError::Runtime(format!(
                            "Camera AVFoundation init failed: {}",
                            error
                        )));
                    }
                    if init_state.is_ready() {
                        if let Some(name) = init_state.take_camera_name() {
                            self.camera_name = name;
                        }
                        avfoundation_ready = true;
                        tracing::info!(
                            "Camera {}: AVFoundation ready, starting frame capture",
                            self.camera_name
                        );
                    }
                }

                // AVFoundation not ready yet - sleep briefly and retry
                if !avfoundation_ready {
                    if loop_iteration == 1 {
                        tracing::debug!(
                            "Camera: Waiting for AVFoundation init to complete on main thread..."
                        );
                    } else if loop_iteration.is_multiple_of(100) {
                        tracing::trace!(
                            "Camera: Still waiting for AVFoundation (iteration {})",
                            loop_iteration
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    return Ok(LoopControl::Continue);
                }
            }

            // AVFoundation is ready - process frames
            let frame_holder = {
                let mut latest = self.latest_frame.lock();
                latest.take()
            };

            let Some(holder) = frame_holder else {
                // No frame available yet
                std::thread::sleep(std::time::Duration::from_millis(1));
                return Ok(LoopControl::Continue);
            };

            // Process the frame
            unsafe {
                let pixel_buffer_ref = &*holder.pixel_buffer as *const CVPixelBuffer;

                let iosurface_ref = CVPixelBufferGetIOSurface(pixel_buffer_ref);
                if iosurface_ref.is_null() {
                    tracing::warn!("Camera: Frame has no IOSurface backing, skipping");
                    return Ok(LoopControl::Continue);
                }

                let iosurface =
                    Retained::retain(iosurface_ref).expect("Failed to retain IOSurface");

                let width = CVPixelBufferGetWidth(pixel_buffer_ref);
                let height = CVPixelBufferGetHeight(pixel_buffer_ref);

                let metal_device = self
                    .metal_device
                    .as_ref()
                    .expect("Metal device should be initialized");

                let metal_texture = match iosurface::create_metal_texture_from_iosurface(
                    metal_device.device(),
                    &iosurface,
                    0,
                ) {
                    Ok(tex) => tex,
                    Err(e) => {
                        tracing::warn!("Camera: Failed to create metal texture: {}", e);
                        return Ok(LoopControl::Continue);
                    }
                };

                let wgpu_bridge = self
                    .wgpu_bridge
                    .as_ref()
                    .expect("WebGPU bridge should be initialized");

                let _iosurface_texture = match wgpu_bridge.wrap_metal_texture(
                    &metal_texture,
                    wgpu::TextureFormat::Bgra8Unorm,
                    wgpu::TextureUsages::COPY_SRC,
                ) {
                    Ok(tex) => tex,
                    Err(e) => {
                        tracing::warn!("Camera: Failed to wrap iosurface texture: {}", e);
                        return Ok(LoopControl::Continue);
                    }
                };

                let metal_rgba_texture = {
                    use objc2_metal::{
                        MTLDevice, MTLPixelFormat, MTLTextureDescriptor, MTLTextureUsage,
                    };

                    let desc = MTLTextureDescriptor::new();
                    desc.setPixelFormat(MTLPixelFormat::RGBA8Unorm);
                    desc.setWidth(width);
                    desc.setHeight(height);
                    desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget);

                    match metal_device.device().newTextureWithDescriptor(&desc) {
                        Some(tex) => tex,
                        None => {
                            tracing::warn!("Camera: Failed to create RGBA texture");
                            return Ok(LoopControl::Continue);
                        }
                    }
                };

                let command_queue = self
                    .metal_command_queue
                    .as_ref()
                    .expect("Metal command queue should be initialized");

                use metal::foreign_types::ForeignTypeRef;

                let source_texture_ptr = &*metal_texture as *const _ as *mut std::ffi::c_void;
                let source_texture_ref = metal::TextureRef::from_ptr(source_texture_ptr as *mut _);

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

                let output_texture = match wgpu_bridge.wrap_metal_texture(
                    &metal_rgba_texture,
                    wgpu::TextureFormat::Rgba8Unorm,
                    wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                ) {
                    Ok(tex) => tex,
                    Err(e) => {
                        tracing::warn!("Camera: Failed to wrap output texture: {}", e);
                        return Ok(LoopControl::Continue);
                    }
                };

                let timestamp_ns = crate::core::media_clock::MediaClock::now().as_nanos() as i64;

                let frame = VideoFrame::new(
                    Arc::new(output_texture),
                    wgpu::TextureFormat::Rgba8Unorm,
                    timestamp_ns,
                    self.frame_count,
                    width as u32,
                    height as u32,
                );

                self.frame_count += 1;

                if self.frame_count.is_multiple_of(60) {
                    tracing::info!(
                        "Camera {}: Frame {} ({}x{})",
                        self.camera_name,
                        self.frame_count,
                        width,
                        height
                    );
                }

                self.video.write(frame);
            }

            Ok(LoopControl::Continue)
        })
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
