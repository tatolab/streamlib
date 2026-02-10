// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::apple::corevideo_ffi::{
    CVPixelBufferGetHeight, CVPixelBufferGetIOSurface, CVPixelBufferGetWidth, IOSurfaceGetID,
};
use crate::core::rhi::{PixelFormat, RhiPixelBuffer, RhiPixelBufferRef};
use crate::core::{GpuContext, Result, RuntimeContext, StreamError};
use crate::iceoryx2::OutputWriter;
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread, ClassType};
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::CVPixelBuffer;
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCRunningApplication, SCShareableContent, SCStream,
    SCStreamConfiguration, SCStreamOutput, SCStreamOutputType, SCWindow,
};
use parking_lot::Mutex;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

// Config type is generated from JTD schema
pub use crate::_generated_::com_tatolab_screen_capture_config::{ScreenCaptureConfig, TargetType};

type CMSampleBufferRef = *mut c_void;

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetImageBuffer(sbuf: CMSampleBufferRef) -> *mut CVPixelBuffer;
}

/// CMTime structure for setting frame interval on SCStreamConfiguration.
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
    /// Create a CMTime representing the frame interval for given fps.
    fn from_fps(fps: f64) -> Self {
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

/// Shared state for ScreenCaptureKit initialization (async pattern).
struct ScreenCaptureInitState {
    ready: AtomicBool,
    failed: AtomicBool,
    error_message: Mutex<Option<String>>,
}

impl ScreenCaptureInitState {
    fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            error_message: Mutex::new(None),
        }
    }

    fn mark_ready(&self) {
        self.ready.store(true, Ordering::Release);
    }

    fn mark_failed(&self, error: String) {
        *self.error_message.lock() = Some(error);
        self.failed.store(true, Ordering::Release);
    }
}

/// Callback context for processing frames in ScreenCaptureKit callback.
struct ScreenCaptureCallbackContext {
    output_writer: *const OutputWriter,
    gpu_context: GpuContext,
    frame_count: AtomicU64,
    _outputs_arc: Arc<OutputWriter>,
}

// SAFETY: OutputWriter is Sync, and the pointer is only dereferenced while valid
unsafe impl Send for ScreenCaptureCallbackContext {}
unsafe impl Sync for ScreenCaptureCallbackContext {}

/// Global callback context - set by start(), used by ScreenCaptureKit callback
static SCREEN_CAPTURE_CALLBACK_CONTEXT: std::sync::OnceLock<Arc<ScreenCaptureCallbackContext>> =
    std::sync::OnceLock::new();

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "StreamlibScreenCaptureDelegate"]
    pub struct ScreenCaptureDelegate;

    unsafe impl NSObjectProtocol for ScreenCaptureDelegate {}

    unsafe impl SCStreamOutput for ScreenCaptureDelegate {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_did_output_sample_buffer_of_type(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            output_type: SCStreamOutputType,
        ) {
            // Only process screen content, not audio
            if output_type != SCStreamOutputType::Screen {
                return;
            }

            let Some(ctx) = SCREEN_CAPTURE_CALLBACK_CONTEXT.get() else {
                return;
            };

            let sample_buffer_ptr = sample_buffer as *const CMSampleBuffer as *mut c_void;
            let pixel_buffer_ptr = CMSampleBufferGetImageBuffer(sample_buffer_ptr);
            if pixel_buffer_ptr.is_null() {
                return;
            }

            // Get IOSurface for cross-process sharing
            let screen_iosurface =
                CVPixelBufferGetIOSurface(pixel_buffer_ptr as *mut std::ffi::c_void);
            if screen_iosurface.is_null() {
                eprintln!("[ScreenCapture] CVPixelBuffer not backed by IOSurface");
                return;
            }

            let width = CVPixelBufferGetWidth(pixel_buffer_ptr as *mut _) as u32;
            let height = CVPixelBufferGetHeight(pixel_buffer_ptr as *mut _) as u32;

            let frame_num = ctx.frame_count.fetch_add(1, Ordering::Relaxed);

            // Acquire pooled buffer from GpuContext
            let surface_id_str =
                match ctx
                    .gpu_context
                    .acquire_pixel_buffer(width, height, PixelFormat::Bgra32)
                {
                    Ok((pool_id, pooled_buffer)) => {
                        match blit_iosurface_to_pooled_buffer(
                            ctx,
                            screen_iosurface,
                            &pooled_buffer,
                            width,
                            height,
                        ) {
                            Ok(()) => pool_id.to_string(),
                            Err(e) => {
                                if frame_num == 0 {
                                    eprintln!("[ScreenCapture] Blit failed: {}, falling back", e);
                                }
                                forward_iosurface_directly(ctx, screen_iosurface)
                            }
                        }
                    }
                    Err(e) => {
                        if frame_num == 0 {
                            eprintln!("[ScreenCapture] Pool acquire failed: {}, falling back", e);
                        }
                        forward_iosurface_directly(ctx, screen_iosurface)
                    }
                };

            let timestamp_ns = crate::core::media_clock::MediaClock::now().as_nanos() as i64;

            let ipc_frame = crate::_generated_::Videoframe {
                surface_id: surface_id_str,
                width,
                height,
                timestamp_ns: timestamp_ns.to_string(),
                frame_index: frame_num.to_string(),
            };

            let outputs = &*ctx.output_writer;
            if let Err(e) = outputs.write("video", &ipc_frame) {
                eprintln!("[ScreenCapture] Failed to write frame: {}", e);
                return;
            }

            if frame_num == 0 {
                eprintln!(
                    "[ScreenCapture] First frame processed ({}x{}, pooled buffers)",
                    width, height
                );
            } else if frame_num % 60 == 0 {
                eprintln!("[ScreenCapture] Frame #{}", frame_num);
            }
        }
    }
);

impl ScreenCaptureDelegate {
    fn new() -> Retained<Self> {
        unsafe {
            let this: Retained<Self> = msg_send![Self::class(), new];
            this
        }
    }
}

/// GPU blit from source IOSurface to pooled buffer.
unsafe fn blit_iosurface_to_pooled_buffer(
    ctx: &ScreenCaptureCallbackContext,
    source_iosurface: crate::apple::corevideo_ffi::IOSurfaceRef,
    pooled_buffer: &RhiPixelBuffer,
    width: u32,
    height: u32,
) -> crate::core::Result<()> {
    ctx.gpu_context
        .blit_copy_iosurface(source_iosurface, pooled_buffer, width, height)
}

/// Fall back to direct IOSurface forwarding.
unsafe fn forward_iosurface_directly(
    ctx: &ScreenCaptureCallbackContext,
    source_iosurface: crate::apple::corevideo_ffi::IOSurfaceRef,
) -> String {
    match RhiPixelBufferRef::from_iosurface_ref(source_iosurface) {
        Ok(pixel_buffer_ref) => {
            let pixel_buffer = RhiPixelBuffer::new(pixel_buffer_ref);
            match ctx.gpu_context.check_in_surface(&pixel_buffer) {
                Ok(id) => id,
                Err(_) => IOSurfaceGetID(source_iosurface).to_string(),
            }
        }
        Err(_) => IOSurfaceGetID(source_iosurface).to_string(),
    }
}

#[crate::processor("com.tatolab.screen_capture")]
pub struct AppleScreenCaptureProcessor {
    /// GPU context for surface pooling (set in setup).
    gpu_context: Option<GpuContext>,
    /// Async init state.
    capture_init_state: Option<Arc<ScreenCaptureInitState>>,
}

impl crate::core::ManualProcessor for AppleScreenCaptureProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.gpu_context = Some(ctx.gpu.clone());
        tracing::info!("ScreenCapture: setup() complete");
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        let frame_count = SCREEN_CAPTURE_CALLBACK_CONTEXT
            .get()
            .map(|ctx| ctx.frame_count.load(Ordering::Relaxed))
            .unwrap_or(0);
        tracing::info!("ScreenCapture: teardown() ({} frames)", frame_count);
        std::future::ready(Ok(()))
    }

    fn start(&mut self) -> Result<()> {
        tracing::trace!("ScreenCapture: start() called");

        // Validate config
        validate_config(&self.config)?;

        let gpu_context = self.gpu_context.clone().ok_or_else(|| {
            StreamError::Configuration("GPU context not initialized. Call setup() first.".into())
        })?;

        // Create callback context
        let outputs_arc: Arc<OutputWriter> = self.outputs.clone();
        let output_writer_ptr = Arc::as_ptr(&outputs_arc);
        let callback_context = Arc::new(ScreenCaptureCallbackContext {
            output_writer: output_writer_ptr,
            gpu_context,
            frame_count: AtomicU64::new(0),
            _outputs_arc: outputs_arc,
        });

        let _ = SCREEN_CAPTURE_CALLBACK_CONTEXT.set(callback_context);

        let init_state = Arc::new(ScreenCaptureInitState::new());
        self.capture_init_state = Some(Arc::clone(&init_state));

        let config = self.config.clone();

        use dispatch2::DispatchQueue;
        DispatchQueue::main().exec_async(move || {
            Self::initialize_screen_capture_on_main_thread(&config, init_state);
        });

        tracing::info!("ScreenCapture: Capture started");
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        tracing::trace!("ScreenCapture: stop() called");

        std::thread::sleep(std::time::Duration::from_millis(50));

        let frame_count = SCREEN_CAPTURE_CALLBACK_CONTEXT
            .get()
            .map(|ctx| ctx.frame_count.load(Ordering::Relaxed))
            .unwrap_or(0);

        tracing::info!("ScreenCapture: Stopped ({} frames)", frame_count);
        Ok(())
    }
}

impl AppleScreenCaptureProcessor::Processor {
    /// Initialize ScreenCaptureKit on main thread.
    fn initialize_screen_capture_on_main_thread(
        config: &ScreenCaptureConfig,
        init_state: Arc<ScreenCaptureInitState>,
    ) {
        let config = config.clone();
        let init_state_for_completion = Arc::clone(&init_state);

        unsafe {
            // Get shareable content asynchronously
            let completion_block = RcBlock::new(
                move |content: *mut SCShareableContent, error: *mut NSError| {
                    if !error.is_null() {
                        let error = Retained::retain(error).unwrap();
                        let msg = error.localizedDescription().to_string();
                        eprintln!("[ScreenCapture] Failed to get shareable content: {}", msg);
                        init_state_for_completion.mark_failed(msg);
                        return;
                    }

                    if content.is_null() {
                        init_state_for_completion
                            .mark_failed("No shareable content available".to_string());
                        return;
                    }

                    let content = Retained::retain(content).unwrap();

                    match Self::setup_capture_stream(&config, content) {
                        Ok(()) => {
                            eprintln!("[ScreenCapture] Capture stream started");
                            init_state_for_completion.mark_ready();
                        }
                        Err(e) => {
                            eprintln!("[ScreenCapture] Setup failed: {}", e);
                            init_state_for_completion.mark_failed(e.to_string());
                        }
                    }
                },
            );
            SCShareableContent::getShareableContentWithCompletionHandler(&completion_block);
        }
    }

    /// Set up the capture stream with the given content.
    unsafe fn setup_capture_stream(
        config: &ScreenCaptureConfig,
        content: Retained<SCShareableContent>,
    ) -> Result<()> {
        let displays = content.displays();
        let windows = content.windows();
        let applications = content.applications();

        // Create content filter based on target type
        let filter = match config.target_type {
            TargetType::Display => {
                let display_index = config.display_index.unwrap_or(0) as usize;
                if display_index >= displays.len() {
                    return Err(StreamError::Configuration(format!(
                        "Display {} not found (available: {})",
                        display_index,
                        displays.len()
                    )));
                }
                let display = displays.objectAtIndex(display_index);

                // Use excludingWindows variant to capture entire display
                let empty_windows: Retained<NSArray<SCWindow>> = NSArray::new();
                SCContentFilter::initWithDisplay_excludingWindows(
                    SCContentFilter::alloc(),
                    &display,
                    &empty_windows,
                )
            }
            TargetType::Window => Self::create_window_filter(config, &windows)?,
            TargetType::Application => {
                Self::create_application_filter(config, &displays, &applications)?
            }
        };

        // Configure stream
        let stream_config = SCStreamConfiguration::new();

        // Get dimensions from display
        let (width, height) = match config.target_type {
            TargetType::Display => {
                let display_index = config.display_index.unwrap_or(0) as usize;
                if display_index < displays.len() {
                    let display = displays.objectAtIndex(display_index);
                    (display.width() as u32, display.height() as u32)
                } else {
                    (1920, 1080)
                }
            }
            _ => (1920, 1080), // Default for window/app capture
        };

        stream_config.setWidth(width as usize);
        stream_config.setHeight(height as usize);

        // Set frame rate
        let frame_rate = config.frame_rate.unwrap_or(30.0);
        let frame_interval = CMTime::from_fps(frame_rate);
        let _: () = msg_send![&stream_config, setMinimumFrameInterval: frame_interval];

        // Show cursor
        stream_config.setShowsCursor(config.show_cursor.unwrap_or(false));

        // Exclude current app (for display/app capture)
        if config.exclude_current_app.unwrap_or(true) {
            stream_config.setIgnoreGlobalClipSingleWindow(true);
        }

        // Set pixel format to BGRA for consistency
        stream_config.setPixelFormat(0x42475241); // 'BGRA'

        // Create and start stream
        let stream = SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &stream_config,
            None,
        );

        // Add output delegate
        let delegate = ScreenCaptureDelegate::new();
        let queue = dispatch2::DispatchQueue::new(
            "com.streamlib.screen_capture.video",
            dispatch2::DispatchQueueAttr::SERIAL,
        );

        let add_result: std::result::Result<(), Retained<NSError>> = stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*delegate),
                SCStreamOutputType::Screen,
                Some(&queue),
            );

        if let Err(e) = add_result {
            return Err(StreamError::Configuration(format!(
                "Failed to add stream output: {}",
                e.localizedDescription()
            )));
        }

        // Start capture
        let start_block = RcBlock::new(move |error: *mut NSError| {
            if !error.is_null() {
                let error = Retained::retain(error).unwrap();
                eprintln!(
                    "[ScreenCapture] Start capture failed: {}",
                    error.localizedDescription()
                );
            } else {
                eprintln!("[ScreenCapture] Capture started successfully");
            }
        });
        stream.startCaptureWithCompletionHandler(Some(&*start_block));

        // Leak objects to keep them alive
        let _ = Retained::into_raw(stream);
        let _ = Retained::into_raw(delegate);
        let _ = Retained::into_raw(filter);

        Ok(())
    }

    /// Create content filter for window capture.
    unsafe fn create_window_filter(
        config: &ScreenCaptureConfig,
        windows: &NSArray<SCWindow>,
    ) -> Result<Retained<SCContentFilter>> {
        // Find window by ID or title
        let mut found_window: Option<Retained<SCWindow>> = None;

        for i in 0..windows.len() {
            let w = windows.objectAtIndex(i);

            if let Some(window_id) = config.window_id {
                if w.windowID() == window_id {
                    found_window = Some(w);
                    break;
                }
            } else if let Some(ref title) = config.window_title {
                if let Some(window_title) = w.title() {
                    if window_title.to_string().contains(title) {
                        found_window = Some(w);
                        break;
                    }
                }
            }
        }

        let window = found_window.ok_or_else(|| {
            StreamError::Configuration(format!(
                "Window not found (id: {:?}, title: {:?})",
                config.window_id, config.window_title
            ))
        })?;

        // Capture single window (desktop independent)
        Ok(SCContentFilter::initWithDesktopIndependentWindow(
            SCContentFilter::alloc(),
            &window,
        ))
    }

    /// Create content filter for application capture.
    unsafe fn create_application_filter(
        config: &ScreenCaptureConfig,
        displays: &NSArray<SCDisplay>,
        applications: &NSArray<SCRunningApplication>,
    ) -> Result<Retained<SCContentFilter>> {
        let bundle_id = config.app_bundle_id.as_ref().ok_or_else(|| {
            StreamError::Configuration("Application mode requires app_bundle_id".into())
        })?;

        let mut found_app: Option<Retained<SCRunningApplication>> = None;

        for i in 0..applications.len() {
            let a = applications.objectAtIndex(i);
            let app_bundle_id = a.bundleIdentifier();
            if app_bundle_id.to_string() == *bundle_id {
                found_app = Some(a);
                break;
            }
        }

        let app = found_app.ok_or_else(|| {
            StreamError::Configuration(format!("Application not found: {}", bundle_id))
        })?;

        let display_index = config.app_display_index.unwrap_or(0) as usize;
        if display_index >= displays.len() {
            return Err(StreamError::Configuration(format!(
                "Display {} not found for application capture",
                display_index
            )));
        }
        let display = displays.objectAtIndex(display_index);

        // Capture application on display
        let app_array: Retained<NSArray<SCRunningApplication>> = NSArray::from_slice(&[&*app]);
        let empty_windows: Retained<NSArray<SCWindow>> = NSArray::new();

        Ok(
            SCContentFilter::initWithDisplay_includingApplications_exceptingWindows(
                SCContentFilter::alloc(),
                &display,
                &app_array,
                &empty_windows,
            ),
        )
    }
}

/// Validate config based on target_type.
fn validate_config(config: &ScreenCaptureConfig) -> Result<()> {
    match config.target_type {
        TargetType::Display => Ok(()),
        TargetType::Window => {
            if config.window_title.is_none() && config.window_id.is_none() {
                return Err(StreamError::Configuration(
                    "Window mode requires window_title or window_id".into(),
                ));
            }
            Ok(())
        }
        TargetType::Application => {
            if config.app_bundle_id.is_none() {
                return Err(StreamError::Configuration(
                    "Application mode requires app_bundle_id".into(),
                ));
            }
            Ok(())
        }
    }
}
