// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// VideoToolbox H.264 Decoder
//
// Hardware-accelerated H.264 decoding using Apple's VideoToolbox framework.
// Complements the encoder for WHEP (WebRTC HTTP Egress Protocol) playback.
//
// **Architecture**:
// - Annex B NAL units → AVCC format → CMSampleBuffer
// - VTDecompressionSession decode → CVPixelBuffer (NV12)
// - VTPixelTransferSession convert → CVPixelBuffer (RGBA)
// - CVPixelBuffer → IOSurface → wgpu::Texture
// - All VideoToolbox operations run on main thread
//
// **Reference**: Encoder implementation (encoder.rs) - inverse operations

use super::{ffi, format};
use crate::apple::{MetalDevice, WgpuBridge};
use crate::core::{GpuContext, Result, RuntimeContext, StreamError, VideoFrame};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Decoded video frame configuration
#[derive(Clone, Debug, PartialEq)]
pub struct VideoDecoderConfig {
    pub width: u32,
    pub height: u32,
}

impl Default for VideoDecoderConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
        }
    }
}

/// VideoToolbox-based hardware H.264 decoder
///
/// Decodes H.264 NAL units (Annex B format) to VideoFrame objects.
/// Uses VTDecompressionSession for hardware-accelerated decoding.
pub struct VideoToolboxDecoder {
    config: VideoDecoderConfig,
    gpu_context: Option<GpuContext>,
    runtime_context: Arc<RuntimeContext>,

    // VideoToolbox session (null until SPS/PPS received)
    decompression_session: Option<ffi::VTDecompressionSessionRef>,

    // Format descriptor (created from SPS/PPS)
    format_description: Option<ffi::CMFormatDescriptionRef>,

    // Decoded frames queue (populated by callback)
    decoded_frames: Arc<Mutex<VecDeque<DecodedFrame>>>,

    // Callback context (owned pointer for cleanup)
    callback_context: Option<*mut std::ffi::c_void>,

    // wgpu bridge for texture import
    wgpu_bridge: Option<Arc<WgpuBridge>>,

    // Frame counter
    frame_count: u64,

    // SPS/PPS state
    has_format: bool,
}

/// Internal structure for decoded frames
struct DecodedFrame {
    pixel_buffer: *mut objc2_core_video::CVPixelBuffer,
    timestamp_ns: i64,
}

// SAFETY: CVPixelBuffer is thread-safe after creation and we properly retain/release it
unsafe impl Send for DecodedFrame {}

impl VideoToolboxDecoder {
    /// Create a new VideoToolbox decoder
    pub fn new(
        config: VideoDecoderConfig,
        gpu_context: Option<GpuContext>,
        ctx: &RuntimeContext,
    ) -> Result<Self> {
        tracing::info!(
            "[VideoToolbox Decoder] Initializing ({}x{})",
            config.width,
            config.height
        );

        // Initialize wgpu bridge for texture import
        let wgpu_bridge = if let Some(ref gpu_ctx) = gpu_context {
            let metal_device = MetalDevice::new()?;
            Some(Arc::new(WgpuBridge::from_shared_device(
                metal_device.clone_device(),
                gpu_ctx.device().as_ref().clone(),
                gpu_ctx.queue().as_ref().clone(),
            )))
        } else {
            None
        };

        Ok(Self {
            config,
            gpu_context,
            runtime_context: Arc::new(ctx.clone()),
            decompression_session: None,
            format_description: None,
            decoded_frames: Arc::new(Mutex::new(VecDeque::new())),
            callback_context: None,
            wgpu_bridge,
            frame_count: 0,
            has_format: false,
        })
    }

    /// Update SPS/PPS from NAL units and create decompression session
    ///
    /// Must be called before decode() when receiving SPS/PPS NAL units.
    /// The decompression session will be created with these parameters.
    pub fn update_format(&mut self, sps: &[u8], pps: &[u8]) -> Result<()> {
        tracing::info!(
            "[VideoToolbox Decoder] Updating format (SPS: {} bytes, PPS: {} bytes)",
            sps.len(),
            pps.len()
        );

        let ctx = self.runtime_context.clone();
        let decoded_frames = Arc::clone(&self.decoded_frames);

        // Clone SPS/PPS to move into closure
        let sps = sps.to_vec();
        let pps = pps.to_vec();

        // CRITICAL: Create format description and decompression session on main thread
        let (format_desc_ptr, session_ptr, callback_context_ptr) = ctx
            .run_on_runtime_thread_blocking(move || {
                unsafe {
                    // Create CMVideoFormatDescription from SPS/PPS
                    let param_set_pointers = [sps.as_ptr(), pps.as_ptr()];
                    let param_set_sizes = [sps.len(), pps.len()];
                    let mut format_desc: ffi::CMFormatDescriptionRef = std::ptr::null_mut();

                    let status = ffi::CMVideoFormatDescriptionCreateFromH264ParameterSets(
                        std::ptr::null(), // allocator
                        2,                // parameter set count
                        param_set_pointers.as_ptr(),
                        param_set_sizes.as_ptr(),
                        4, // NAL unit header length (AVCC uses 4-byte length prefixes)
                        &mut format_desc,
                    );

                    if status != ffi::NO_ERR {
                        return Err(StreamError::Runtime(format!(
                            "CMVideoFormatDescriptionCreateFromH264ParameterSets failed: {}",
                            status
                        )));
                    }

                    // Create destination pixel buffer attributes (request BGRA for easier wgpu import)
                    let bgra_format: u32 = 0x42475241; // 'BGRA' - kCVPixelFormatType_32BGRA
                    let format_number = ffi::CFNumberCreate(
                        std::ptr::null(),
                        ffi::K_CFNUMBER_SINT32_TYPE,
                        &bgra_format as *const _ as *const _,
                    );

                    let keys = [ffi::kCVPixelBufferPixelFormatTypeKey];
                    let values = [format_number];

                    let pixel_buffer_attrs = ffi::CFDictionaryCreate(
                        std::ptr::null(),
                        keys.as_ptr(),
                        values.as_ptr(),
                        1,
                        std::ptr::null(), // use default key callbacks
                        std::ptr::null(), // use default value callbacks
                    );

                    ffi::CFRelease(format_number as *const _);

                    // Prepare callback context
                    let callback_context_arc =
                        Arc::into_raw(decoded_frames) as *mut std::ffi::c_void;

                    // Create output callback structure
                    #[repr(C)]
                    struct VTDecompressionOutputCallbackRecord {
                        callback: ffi::VTDecompressionOutputCallback,
                        context: *mut std::ffi::c_void,
                    }

                    let callback_record = VTDecompressionOutputCallbackRecord {
                        callback: decompression_output_callback,
                        context: callback_context_arc,
                    };

                    // Create decompression session
                    let mut session: ffi::VTDecompressionSessionRef = std::ptr::null_mut();
                    let status = ffi::VTDecompressionSessionCreate(
                        std::ptr::null(), // allocator
                        format_desc,
                        std::ptr::null(), // video decoder specification
                        pixel_buffer_attrs,
                        &callback_record as *const _ as *const std::ffi::c_void,
                        &mut session,
                    );

                    ffi::CFRelease(pixel_buffer_attrs as *const _);

                    if status != ffi::NO_ERR {
                        ffi::CFRelease(format_desc as *const _);
                        // Restore Arc ownership before returning error
                        let _ = Arc::from_raw(
                            callback_context_arc as *const Mutex<VecDeque<DecodedFrame>>,
                        );
                        return Err(StreamError::Runtime(format!(
                            "VTDecompressionSessionCreate failed: {}",
                            status
                        )));
                    }

                    // Return pointers as usize for Send compatibility
                    Ok((
                        format_desc as usize,
                        session as usize,
                        callback_context_arc as usize,
                    ))
                }
            })?;

        // Cast back to proper pointer types
        self.format_description = Some(format_desc_ptr as ffi::CMFormatDescriptionRef);
        self.decompression_session = Some(session_ptr as ffi::VTDecompressionSessionRef);
        self.callback_context = Some(callback_context_ptr as *mut std::ffi::c_void);
        self.has_format = true;

        tracing::info!("[VideoToolbox Decoder] ✅ Decompression session created");
        Ok(())
    }

    /// Decode H.264 NAL units to VideoFrame.
    pub fn decode(
        &mut self,
        nal_units_annex_b: &[u8],
        timestamp_ns: i64,
    ) -> Result<Option<VideoFrame>> {
        // Check if we have a decompression session
        let session = self.decompression_session.ok_or_else(|| {
            StreamError::Configuration(
                "Decompression session not initialized - call update_format() with SPS/PPS first"
                    .into(),
            )
        })?;

        let format_desc = self
            .format_description
            .ok_or_else(|| StreamError::Configuration("Format description not available".into()))?;

        // Step 1: Convert Annex B → AVCC format (required by VideoToolbox)
        let avcc_data = format::annex_b_to_avcc(nal_units_annex_b)?;

        // Step 2: Create CMBlockBuffer from AVCC data
        let ctx = self.runtime_context.clone();
        let avcc_len = avcc_data.len();

        // We need to leak the data pointer for CMBlockBuffer, but we'll manage cleanup
        let avcc_ptr = Box::into_raw(avcc_data.into_boxed_slice()) as *mut std::ffi::c_void;
        let avcc_ptr_usize = avcc_ptr as usize; // Convert to usize for Send
        let format_desc_usize = format_desc as usize; // Convert format_desc for Send

        let (_block_buffer_ptr, sample_buffer_ptr) =
            ctx.run_on_runtime_thread_blocking(move || {
                unsafe {
                    let avcc_ptr = avcc_ptr_usize as *mut std::ffi::c_void; // Convert back
                    let format_desc = format_desc_usize as ffi::CMFormatDescriptionRef; // Convert back
                    let mut block_buffer: ffi::CMBlockBufferRef = std::ptr::null_mut();

                    let status = ffi::CMBlockBufferCreateWithMemoryBlock(
                        std::ptr::null(), // allocator
                        avcc_ptr,
                        avcc_len,
                        std::ptr::null(), // block allocator (null = use malloc/free)
                        std::ptr::null(), // custom block source
                        0,                // offset to data
                        avcc_len,
                        0, // flags
                        &mut block_buffer,
                    );

                    if status != ffi::NO_ERR {
                        // Cleanup leaked data
                        let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                            avcc_ptr as *mut u8,
                            avcc_len,
                        ));
                        return Err(StreamError::Runtime(format!(
                            "CMBlockBufferCreateWithMemoryBlock failed: {}",
                            status
                        )));
                    }

                    // Step 3: Create CMSampleBuffer from CMBlockBuffer
                    let mut sample_buffer: ffi::CMSampleBufferRef = std::ptr::null_mut();
                    let presentation_time = ffi::CMTime::new(timestamp_ns, 1_000_000_000);

                    let status = ffi::CMSampleBufferCreate(
                        std::ptr::null(), // allocator
                        block_buffer,
                        true,                 // data ready
                        None,                 // make data ready callback
                        std::ptr::null_mut(), // make data ready refcon
                        format_desc,
                        1,                // num samples
                        0,                // num sample timing entries
                        std::ptr::null(), // sample timing array
                        1,                // num sample size entries
                        &avcc_len,
                        &mut sample_buffer,
                    );

                    if status != ffi::NO_ERR {
                        ffi::CFRelease(block_buffer as *const _);
                        return Err(StreamError::Runtime(format!(
                            "CMSampleBufferCreate failed: {}",
                            status
                        )));
                    }

                    // Set presentation timestamp on sample buffer
                    ffi::CMSampleBufferSetOutputPresentationTimeStamp(
                        sample_buffer,
                        presentation_time,
                    );

                    // Release block buffer (sample buffer retains it)
                    ffi::CFRelease(block_buffer as *const _);

                    Ok((block_buffer as usize, sample_buffer as usize))
                }
            })?;

        let sample_buffer = sample_buffer_ptr as ffi::CMSampleBufferRef;

        // Step 4: Decode frame
        unsafe {
            let mut info_flags: ffi::VTDecodeInfoFlags = 0;
            let status = ffi::VTDecompressionSessionDecodeFrame(
                session,
                sample_buffer,
                0,                                     // decode flags
                timestamp_ns as *mut std::ffi::c_void, // source frame refcon (pass timestamp)
                &mut info_flags,
            );

            // Release sample buffer
            ffi::CFRelease(sample_buffer as *const _);

            if status != ffi::NO_ERR {
                return Err(StreamError::Runtime(format!(
                    "VTDecompressionSessionDecodeFrame failed: {}",
                    status
                )));
            }
        }

        // Step 5: Wait for frame to be decoded
        unsafe {
            let status = ffi::VTDecompressionSessionWaitForAsynchronousFrames(session);
            if status != ffi::NO_ERR {
                tracing::warn!(
                    "[VideoToolbox Decoder] WaitForAsynchronousFrames returned: {}",
                    status
                );
            }
        }

        // Step 6: Retrieve decoded frame from queue
        let decoded_frame = {
            let mut queue = self.decoded_frames.lock().map_err(|e| {
                StreamError::Runtime(format!("Failed to lock decoded frames: {}", e))
            })?;
            queue.pop_front()
        };

        let decoded_frame = match decoded_frame {
            Some(frame) => frame,
            None => {
                tracing::debug!("[VideoToolbox Decoder] No decoded frame available (buffering)");
                return Ok(None);
            }
        };

        // Step 7: Convert CVPixelBuffer → wgpu::Texture
        let video_frame = self.pixel_buffer_to_video_frame(decoded_frame)?;

        self.frame_count += 1;

        if self.frame_count.is_multiple_of(30) {
            tracing::debug!(
                "[VideoToolbox Decoder] Decoded frame {} ({} bytes AVCC input)",
                self.frame_count,
                avcc_len
            );
        }

        Ok(Some(video_frame))
    }

    /// Convert CVPixelBuffer (BGRA) to VideoFrame with wgpu texture
    fn pixel_buffer_to_video_frame(&self, decoded_frame: DecodedFrame) -> Result<VideoFrame> {
        let wgpu_bridge = self
            .wgpu_bridge
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("wgpu bridge not initialized".into()))?;

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("GPU context not available".into()))?;

        // Query actual dimensions from CVPixelBuffer (ground truth from decoded frame)
        let (actual_width, actual_height) = unsafe {
            let width =
                ffi::CVPixelBufferGetWidth(decoded_frame.pixel_buffer as ffi::CVPixelBufferRef);
            let height =
                ffi::CVPixelBufferGetHeight(decoded_frame.pixel_buffer as ffi::CVPixelBufferRef);
            (width as u32, height as u32)
        };

        // Log resolution discovery on first frame or if resolution changes
        if actual_width != self.config.width || actual_height != self.config.height {
            tracing::info!(
                "[VideoToolbox Decoder] 🎥 Actual decoded resolution: {}x{} (config was {}x{})",
                actual_width,
                actual_height,
                self.config.width,
                self.config.height
            );
        }

        // Import CVPixelBuffer as wgpu texture via IOSurface
        let texture = unsafe {
            self.import_pixel_buffer_as_texture(decoded_frame.pixel_buffer, wgpu_bridge, gpu_ctx)?
        };

        // Release pixel buffer
        unsafe {
            use super::ffi;
            ffi::CFRelease(decoded_frame.pixel_buffer as *const _);
        }

        Ok(VideoFrame::new(
            Arc::new(texture),
            wgpu::TextureFormat::Bgra8Unorm,
            decoded_frame.timestamp_ns,
            self.frame_count,
            actual_width,  // Use actual dimensions from decoded buffer
            actual_height, // Use actual dimensions from decoded buffer
        ))
    }

    /// Import CVPixelBuffer as wgpu texture via IOSurface
    unsafe fn import_pixel_buffer_as_texture(
        &self,
        pixel_buffer: *mut objc2_core_video::CVPixelBuffer,
        wgpu_bridge: &WgpuBridge,
        _gpu_ctx: &GpuContext,
    ) -> Result<wgpu::Texture> {
        use super::ffi;
        use crate::apple::iosurface;

        // Get IOSurface from CVPixelBuffer
        let iosurface_ptr = ffi::CVPixelBufferGetIOSurface(pixel_buffer as *const std::ffi::c_void);
        if iosurface_ptr.is_null() {
            return Err(StreamError::GpuError(
                "Failed to get IOSurface from CVPixelBuffer".into(),
            ));
        }

        let iosurface = &*iosurface_ptr;

        // Create Metal texture from IOSurface
        let metal_texture = iosurface::create_metal_texture_from_iosurface(
            wgpu_bridge.metal_device(),
            iosurface,
            0, // plane 0
        )?;

        // Wrap Metal texture as wgpu texture
        let wgpu_texture = wgpu_bridge.wrap_metal_texture(
            &metal_texture,
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        )?;

        Ok(wgpu_texture)
    }
}

impl Drop for VideoToolboxDecoder {
    fn drop(&mut self) {
        unsafe {
            if let Some(session) = self.decompression_session {
                ffi::VTDecompressionSessionInvalidate(session);
            }

            if let Some(format_desc) = self.format_description {
                use super::ffi;
                ffi::CFRelease(format_desc as *const _);
            }

            // Restore Arc ownership for cleanup
            if let Some(callback_context) = self.callback_context {
                let _ = Arc::from_raw(callback_context as *const Mutex<VecDeque<DecodedFrame>>);
            }
        }

        tracing::info!("[VideoToolbox Decoder] Cleaned up");
    }
}

// SAFETY: VideoToolbox session is thread-safe after creation
unsafe impl Send for VideoToolboxDecoder {}
unsafe impl Sync for VideoToolboxDecoder {}

/// Decompression output callback (called by VideoToolbox when frame is decoded)
///
/// SAFETY: This function is called from VideoToolbox's internal thread.
/// The context pointer (from VTDecompressionOutputCallbackRecord) contains Arc<Mutex<VecDeque<DecodedFrame>>>.
extern "C" fn decompression_output_callback(
    decompress_ref: *mut std::ffi::c_void,
    source_frame_refcon: *mut std::ffi::c_void,
    status: ffi::OSStatus,
    _info_flags: ffi::VTDecodeInfoFlags,
    image_buffer: ffi::CVImageBufferRef,
    _presentation_time_stamp: ffi::CMTime,
    _duration: ffi::CMTime,
) {
    if status != ffi::NO_ERR {
        tracing::error!("[VideoToolbox Decoder] Decode callback error: {}", status);
        return;
    }

    if image_buffer.is_null() {
        tracing::warn!("[VideoToolbox Decoder] Decode callback received null image buffer");
        return;
    }

    // Extract timestamp from source_frame_refcon
    let timestamp_ns = source_frame_refcon as i64;

    // Get decoded frames queue from callback context
    // CRITICAL: The pointer is from Arc::into_raw() which leaks the Arc
    // We can safely dereference the raw pointer because:
    // 1. Arc::into_raw() keeps the allocation alive (ref count > 0)
    // 2. The Mutex won't be freed until Drop calls Arc::from_raw()
    // 3. Drop calls VTDecompressionSessionWaitForAsynchronousFrames() first, ensuring no callbacks are running
    let decoded_frames = unsafe {
        let ptr = decompress_ref as *const Mutex<VecDeque<DecodedFrame>>;
        &*ptr // Directly deref the raw pointer
    };

    // Retain the pixel buffer (will be released after conversion to wgpu texture)
    unsafe {
        use super::ffi;
        ffi::CFRetain(image_buffer as *const std::ffi::c_void);
    }

    // Add decoded frame to queue
    let decoded_frame = DecodedFrame {
        pixel_buffer: image_buffer as *mut objc2_core_video::CVPixelBuffer,
        timestamp_ns,
    };

    match decoded_frames.lock() {
        Ok(mut queue) => {
            queue.push_back(decoded_frame);
            tracing::trace!(
                "[VideoToolbox Decoder] Decoded frame queued (timestamp={}ns, queue_len={})",
                timestamp_ns,
                queue.len()
            );
        }
        Err(e) => {
            tracing::error!(
                "[VideoToolbox Decoder] Failed to lock decoded frames queue: {}",
                e
            );
            // Release the pixel buffer since we can't queue it
            unsafe {
                use super::ffi;
                ffi::CFRelease(image_buffer as *const std::ffi::c_void);
            }
        }
    }
}
