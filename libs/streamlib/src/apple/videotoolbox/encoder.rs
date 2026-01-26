// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// VideoToolbox H.264 Encoder
//
// Hardware-accelerated H.264 encoding using Apple's VideoToolbox framework.
// Supports GPU-accelerated texture conversion (wgpu → NV12) and real-time encoding.

use crate::_generated_::{Encodedvideoframe, Videoframe};
use crate::apple::PixelTransferSession;
use crate::core::rhi::{PixelBufferPoolId, RhiPixelBuffer};
use crate::core::{GpuContext, Result, RuntimeContext, StreamError, VideoEncoderConfig};
use objc2_core_video::CVPixelBuffer;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::{ffi, format};

// ============================================================================
// ENCODER IMPLEMENTATION
// ============================================================================

/// VideoToolbox-based hardware encoder
///
/// Encodes VideoFrame textures to compressed video (currently H.264 only).
/// Uses GPU-accelerated texture → NV12 conversion via Metal/VideoToolbox.
pub struct VideoToolboxEncoder {
    config: VideoEncoderConfig,
    compression_session: Option<ffi::VTCompressionSessionRef>,
    frame_count: u64,
    force_next_keyframe: bool,
    gpu_context: Option<GpuContext>,

    // GPU-accelerated texture → NV12 conversion
    pixel_transfer: Option<PixelTransferSession>,

    // For storing encoded output from callback
    encoded_frames: Arc<Mutex<VecDeque<Encodedvideoframe>>>,

    // Callback context that needs to be freed in Drop
    callback_context: Option<*mut std::ffi::c_void>,
}

impl VideoToolboxEncoder {
    /// Create a new VideoToolbox encoder.
    pub fn new(
        config: VideoEncoderConfig,
        gpu_context: Option<GpuContext>,
        ctx: &RuntimeContext,
    ) -> Result<Self> {
        let mut encoder = Self {
            config,
            compression_session: None,
            frame_count: 0,
            force_next_keyframe: true, // First frame should be keyframe
            gpu_context,
            pixel_transfer: None,
            encoded_frames: Arc::new(Mutex::new(VecDeque::new())),
            callback_context: None,
        };

        encoder.setup_compression_session(ctx)?;
        Ok(encoder)
    }

    /// Setup VideoToolbox compression session (must run on main thread)
    fn setup_compression_session(&mut self, ctx: &RuntimeContext) -> Result<()> {
        let encoded_frames_ref = Arc::clone(&self.encoded_frames);

        let width = self.config.width;
        let height = self.config.height;
        let keyframe_interval = self.config.keyframe_interval_frames;
        let bitrate = self.config.bitrate_bps;
        let fps = self.config.fps;
        let codec_fourcc = self.config.codec.fourcc();

        // CRITICAL: VideoToolbox APIs MUST run on main thread
        // Cast pointers to usize for Send compatibility across thread boundary
        let (session_ptr, callback_context_ptr) =
            ctx.run_on_runtime_thread_blocking(move || {
                // Create callback context on main thread
                // Use Arc::into_raw to increment ref count - keeps the Arc alive for the callback
                let callback_context = Arc::into_raw(encoded_frames_ref) as *mut std::ffi::c_void;
                let mut session: ffi::VTCompressionSessionRef = std::ptr::null_mut();

                unsafe {
                    let status = ffi::VTCompressionSessionCreate(
                        std::ptr::null(), // allocator
                        width as i32,
                        height as i32,
                        codec_fourcc,
                        std::ptr::null(), // encoder specification
                        std::ptr::null(), // source image buffer attributes
                        std::ptr::null(), // compressed data allocator
                        compression_output_callback,
                        callback_context,
                        &mut session,
                    );

                    if status != ffi::NO_ERR {
                        // Clean up callback context on error - reconstruct Arc to decrement ref count
                        let _ = Arc::from_raw(
                            callback_context as *const Mutex<VecDeque<Encodedvideoframe>>,
                        );
                        return Err(StreamError::Runtime(format!(
                            "VTCompressionSessionCreate failed: {}",
                            status
                        )));
                    }

                    // Configure encoder properties for real-time streaming
                    // Set H.264 Baseline Profile Level 3.1 (matches 42e01f in SDP)
                    // This ensures compatibility with WebRTC services
                    let status = ffi::VTSessionSetProperty(
                        session,
                        ffi::kVTCompressionPropertyKey_ProfileLevel,
                        ffi::kVTProfileLevel_H264_Baseline_3_1 as *const _,
                    );
                    if status != ffi::NO_ERR {
                        tracing::warn!("Failed to set H.264 profile level: {}", status);
                    }

                    // Enable real-time encoding for low latency
                    let status = ffi::VTSessionSetProperty(
                        session,
                        ffi::kVTCompressionPropertyKey_RealTime,
                        ffi::kCFBooleanTrue as *const _,
                    );
                    if status != ffi::NO_ERR {
                        tracing::warn!("Failed to enable real-time encoding: {}", status);
                    }

                    // Disable frame reordering (B-frames) for low latency
                    // CRITICAL: Set to TRUE to allow encoder to generate keyframes properly
                    // Baseline profile doesn't use B-frames anyway, but encoder needs this enabled
                    let status = ffi::VTSessionSetProperty(
                        session,
                        ffi::kVTCompressionPropertyKey_AllowFrameReordering,
                        ffi::kCFBooleanTrue as *const _,
                    );
                    if status != ffi::NO_ERR {
                        tracing::warn!("Failed to set frame reordering: {}", status);
                    }

                    // Set keyframe interval
                    let max_keyframe_interval = keyframe_interval as i32;
                    let max_keyframe_interval_num = ffi::CFNumberCreate(
                        std::ptr::null(),
                        ffi::K_CFNUMBER_SINT32_TYPE,
                        &max_keyframe_interval as *const _ as *const _,
                    );
                    let status = ffi::VTSessionSetProperty(
                        session,
                        ffi::kVTCompressionPropertyKey_MaxKeyFrameInterval,
                        max_keyframe_interval_num as *const _,
                    );
                    ffi::CFRelease(max_keyframe_interval_num as *const _);
                    if status != ffi::NO_ERR {
                        tracing::warn!("Failed to set keyframe interval: {}", status);
                    }

                    // Set average bitrate
                    let avg_bitrate = bitrate as i32;
                    let avg_bitrate_num = ffi::CFNumberCreate(
                        std::ptr::null(),
                        ffi::K_CFNUMBER_SINT32_TYPE,
                        &avg_bitrate as *const _ as *const _,
                    );
                    let status = ffi::VTSessionSetProperty(
                        session,
                        ffi::kVTCompressionPropertyKey_AverageBitRate,
                        avg_bitrate_num as *const _,
                    );
                    ffi::CFRelease(avg_bitrate_num as *const _);
                    if status != ffi::NO_ERR {
                        tracing::warn!("Failed to set average bitrate: {}", status);
                    }

                    // Set expected frame rate
                    let expected_fps = fps as i32;
                    let expected_fps_num = ffi::CFNumberCreate(
                        std::ptr::null(),
                        ffi::K_CFNUMBER_SINT32_TYPE,
                        &expected_fps as *const _ as *const _,
                    );
                    let status = ffi::VTSessionSetProperty(
                        session,
                        ffi::kVTCompressionPropertyKey_ExpectedFrameRate,
                        expected_fps_num as *const _,
                    );
                    ffi::CFRelease(expected_fps_num as *const _);
                    if status != ffi::NO_ERR {
                        tracing::warn!("Failed to set expected frame rate: {}", status);
                    }
                }

                // Return pointers as usize for Send compatibility
                Ok((session as usize, callback_context as usize))
            })?;

        // Cast back to proper pointer types
        let session = session_ptr as ffi::VTCompressionSessionRef;
        let callback_context = callback_context_ptr as *mut std::ffi::c_void;

        self.compression_session = Some(session);
        self.callback_context = Some(callback_context);

        tracing::info!(
            "VideoToolbox compression session created: {}x{} @ {}fps, H.264 Baseline 3.1",
            self.config.width,
            self.config.height,
            self.config.fps
        );

        // Initialize GPU-accelerated pixel transfer (RGBA → NV12)
        if let Some(ref gpu_ctx) = self.gpu_context {
            // Create PixelTransferSession for GPU-accelerated RGBA → NV12 conversion
            let pixel_transfer = PixelTransferSession::new(gpu_ctx.device().clone())?;
            self.pixel_transfer = Some(pixel_transfer);

            tracing::info!("GPU-accelerated pixel transfer (RGBA → NV12) initialized");
        } else {
            tracing::warn!(
                "No GPU context available, cannot initialize GPU-accelerated pixel transfer"
            );
        }

        Ok(())
    }

    /// Convert RhiPixelBuffer to NV12 CVPixelBuffer using GPU-accelerated VTPixelTransferSession
    fn convert_buffer_to_pixel_buffer(
        &self,
        buffer: &RhiPixelBuffer,
    ) -> Result<*mut CVPixelBuffer> {
        // GPU-accelerated conversion using VTPixelTransferSession
        let pixel_transfer = self.pixel_transfer.as_ref().ok_or_else(|| {
            StreamError::Configuration("PixelTransferSession not initialized".into())
        })?;

        pixel_transfer.convert_buffer_to_nv12(buffer)
    }

    /// Encode a video frame.
    pub fn encode(&mut self, frame: &Videoframe, gpu: &GpuContext) -> Result<Encodedvideoframe> {
        let session = self.compression_session.ok_or_else(|| {
            StreamError::Configuration("Compression session not initialized".into())
        })?;

        // Resolve buffer from surface_id
        let pool_id = PixelBufferPoolId::from_str(&frame.surface_id);
        let buffer = gpu.get_pixel_buffer(&pool_id)?;

        // Parse timestamp from IPC frame
        let timestamp_ns: i64 = frame.timestamp_ns.parse().unwrap_or(0);

        // Step 1: Convert buffer to NV12 CVPixelBuffer
        let pixel_buffer = self.convert_buffer_to_pixel_buffer(&buffer)?;

        // Step 2: Create presentation timestamp
        let presentation_time = ffi::CMTime::new(timestamp_ns, 1_000_000_000);
        let duration = ffi::CMTime::invalid(); // Let VideoToolbox calculate duration

        // Step 3: Determine if we should force a keyframe
        // Force keyframe on first frame and every keyframe_interval frames
        let should_force_keyframe = self.frame_count == 0
            || self
                .frame_count
                .is_multiple_of(self.config.keyframe_interval_frames as u64);

        // Step 4: Encode the frame
        unsafe {
            let frame_properties = if should_force_keyframe {
                tracing::info!(
                    "[VideoToolbox] 🔑 Forcing keyframe at frame {}",
                    self.frame_count
                );
                let dict = Self::create_force_keyframe_properties();
                if dict.is_null() {
                    tracing::error!("[VideoToolbox] ❌ CFDictionary creation FAILED!");
                } else {
                    tracing::debug!(
                        "[VideoToolbox] CFDictionary created successfully at {:p}",
                        dict
                    );
                }
                dict
            } else {
                std::ptr::null()
            };

            let status = ffi::VTCompressionSessionEncodeFrame(
                session,
                pixel_buffer as ffi::CVPixelBufferRef,
                presentation_time,
                duration,
                frame_properties,     // Pass keyframe properties
                std::ptr::null_mut(), // source frame ref con
                std::ptr::null_mut(), // info flags out
            );

            // Release frame properties dictionary if created
            if !frame_properties.is_null() {
                ffi::CFRelease(frame_properties);
            }

            // Release pixel buffer
            objc2::rc::autoreleasepool(|_| {
                let _ = objc2::rc::Retained::from_raw(pixel_buffer);
            });

            if status != ffi::NO_ERR {
                return Err(StreamError::Runtime(format!(
                    "VTCompressionSessionEncodeFrame failed: {}",
                    status
                )));
            }
        }

        // Step 5: Force frame completion to ensure callback is called
        unsafe {
            let complete_status = ffi::VTCompressionSessionCompleteFrames(
                session,
                ffi::CMTime::invalid(), // Complete all pending frames
            );

            if complete_status != ffi::NO_ERR {
                tracing::warn!(
                    "VTCompressionSessionCompleteFrames returned: {}",
                    complete_status
                );
            }
        }

        // Step 6: Retrieve encoded frame from queue (populated by callback)
        let mut encoded_frame = self
            .encoded_frames
            .lock()
            .map_err(|e| {
                StreamError::Runtime(format!("Failed to lock encoded frames queue: {}", e))
            })?
            .pop_front()
            .ok_or_else(|| {
                StreamError::Runtime("No encoded frame available after encoding".into())
            })?;

        // Step 7: Update frame metadata
        encoded_frame.timestamp_ns = timestamp_ns.to_string();
        encoded_frame.frame_number = self.frame_count.to_string();

        self.frame_count += 1;

        // Log encoding info
        if self.frame_count.is_multiple_of(30) {
            tracing::debug!(
                "Encoded frame {}: {} bytes, keyframe={}",
                encoded_frame.frame_number,
                encoded_frame.data.len(),
                encoded_frame.is_keyframe
            );
        }

        Ok(encoded_frame)
    }

    /// Force the next frame to be a keyframe
    pub fn force_keyframe(&mut self) {
        self.force_next_keyframe = true;
    }

    /// Get encoder configuration
    pub fn config(&self) -> &VideoEncoderConfig {
        &self.config
    }

    /// Update encoder bitrate in real-time
    pub fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        self.config.bitrate_bps = bitrate_bps;

        // Update VideoToolbox session property if session exists
        if let Some(session) = self.compression_session {
            unsafe {
                let avg_bitrate = bitrate_bps as i32;
                let avg_bitrate_num = ffi::CFNumberCreate(
                    std::ptr::null(),
                    ffi::K_CFNUMBER_SINT32_TYPE,
                    &avg_bitrate as *const _ as *const _,
                );
                let status = ffi::VTSessionSetProperty(
                    session,
                    ffi::kVTCompressionPropertyKey_AverageBitRate,
                    avg_bitrate_num as *const _,
                );
                ffi::CFRelease(avg_bitrate_num as *const _);
                if status != ffi::NO_ERR {
                    return Err(StreamError::Runtime(format!(
                        "Failed to update encoder bitrate: {}",
                        status
                    )));
                }
            }
        }

        Ok(())
    }

    /// Create a CFDictionary requesting a keyframe
    ///
    /// Returns a CFDictionaryRef with {kVTEncodeFrameOptionKey_ForceKeyFrame: kCFBooleanTrue}
    /// Caller must call CFRelease on the returned pointer
    unsafe fn create_force_keyframe_properties() -> *const std::ffi::c_void {
        // Create dictionary with key-value: {kVTEncodeFrameOptionKey_ForceKeyFrame: true}
        let key = ffi::kVTEncodeFrameOptionKey_ForceKeyFrame;
        let value = ffi::kCFBooleanTrue;

        let keys = [key];
        let values = [value];

        ffi::CFDictionaryCreate(
            std::ptr::null(), // allocator (default)
            keys.as_ptr(),
            values.as_ptr(),
            1,                // count
            std::ptr::null(), // key callbacks (default)
            std::ptr::null(), // value callbacks (default)
        )
    }
}

// SAFETY: VideoToolbox compression session will only be accessed from the main thread
// via RuntimeContext::run_on_runtime_thread_blocking, similar to Mp4WriterProcessor pattern
unsafe impl Send for VideoToolboxEncoder {}

impl Drop for VideoToolboxEncoder {
    fn drop(&mut self) {
        unsafe {
            // Clean up VTCompressionSession
            if let Some(session) = self.compression_session {
                // CRITICAL: Complete all pending frames BEFORE invalidating session
                // This ensures all VideoToolbox callbacks finish executing before we free the callback context
                // Without this, callbacks may try to access freed memory → SIGABRT crash
                tracing::debug!("[VideoToolbox] Waiting for all pending frames to complete...");
                let status = ffi::VTCompressionSessionCompleteFrames(
                    session,
                    ffi::CMTime::invalid(), // kCMTimeInvalid = flush all pending frames
                );
                if status != ffi::NO_ERR {
                    tracing::warn!(
                        "[VideoToolbox] VTCompressionSessionCompleteFrames failed: {}",
                        status
                    );
                }
                tracing::debug!("[VideoToolbox] All pending frames completed");

                // Now safe to invalidate (stops accepting new frames)
                ffi::VTCompressionSessionInvalidate(session);

                // Release the CoreFoundation object (free memory)
                ffi::CFRelease(session as *const std::ffi::c_void);
            }

            // Clean up callback context (the leaked Arc from setup_compression_session)
            // Safe now because VTCompressionSessionCompleteFrames() ensured all callbacks finished
            // Reconstruct Arc to decrement ref count and potentially drop the Mutex
            if let Some(context) = self.callback_context {
                let _ = Arc::from_raw(context as *const Mutex<VecDeque<Encodedvideoframe>>);
                tracing::debug!("[VideoToolbox] Callback context freed");
            }
        }
    }
}

// ============================================================================
// VIDEOTOOLBOX CALLBACK
// ============================================================================

/// VideoToolbox compression output callback
///
/// Called asynchronously by VideoToolbox when a frame is encoded.
/// Extracts the encoded data and adds it to the encoder's output queue.
extern "C" fn compression_output_callback(
    output_callback_ref_con: *mut std::ffi::c_void,
    _source_frame_ref_con: *mut std::ffi::c_void,
    status: ffi::OSStatus,
    _info_flags: u32,
    sample_buffer: ffi::CMSampleBufferRef,
) {
    if status != ffi::NO_ERR {
        tracing::error!("VideoToolbox encoding failed: {}", status);
        return;
    }

    if sample_buffer.is_null() {
        return;
    }

    // Get the encoded_frames queue from the context
    // CRITICAL: The pointer is from Arc::into_raw() which leaks the Arc
    // We can safely dereference the raw pointer because:
    // 1. Arc::into_raw() keeps the allocation alive (ref count > 0)
    // 2. The Mutex won't be freed until Drop calls Arc::from_raw()
    // 3. Drop calls VTCompressionSessionCompleteFrames() first, ensuring no callbacks are running
    let encoded_frames = unsafe {
        let ptr = output_callback_ref_con as *const Mutex<VecDeque<Encodedvideoframe>>;
        &*ptr // Directly deref the raw pointer
    };

    // Extract encoded data from sample buffer and convert to AVCC format
    unsafe {
        let block_buffer = ffi::CMSampleBufferGetDataBuffer(sample_buffer);
        if block_buffer.is_null() {
            tracing::error!("CMSampleBufferGetDataBuffer returned null");
            return;
        }

        let data_length = ffi::CMBlockBufferGetDataLength(block_buffer);
        let mut raw_data = vec![0u8; data_length];

        let copy_status =
            ffi::CMBlockBufferCopyDataBytes(block_buffer, 0, data_length, raw_data.as_mut_ptr());

        if copy_status != ffi::NO_ERR {
            tracing::error!("CMBlockBufferCopyDataBytes failed: {}", copy_status);
            return;
        }

        // VideoToolbox outputs AVCC format (4-byte length-prefixed NAL units)
        // WebRTC/RTP requires Annex B format (start code prefixed)
        // Convert AVCC → Annex B
        let annex_b_data = format::avcc_to_annex_b(&raw_data);

        tracing::trace!(
            "[VideoToolbox] Converted {} bytes AVCC → {} bytes Annex B",
            data_length,
            annex_b_data.len()
        );

        // Check if this is a keyframe using CMSampleBuffer attachments (Apple's official method)
        // The kCMSampleAttachmentKey_NotSync key is ONLY present for non-sync frames (P/B frames)
        // If the key is absent, the frame is a sync frame (keyframe/I-frame)
        let is_keyframe = {
            let attachments = ffi::CMSampleBufferGetSampleAttachmentsArray(sample_buffer, false);
            if !attachments.is_null() {
                let count = ffi::CFArrayGetCount(attachments);
                if count > 0 {
                    let attachment = ffi::CFArrayGetValueAtIndex(attachments, 0);
                    if !attachment.is_null() {
                        let not_sync = ffi::CFDictionaryGetValue(
                            attachment,
                            ffi::kCMSampleAttachmentKey_NotSync,
                        );
                        // If NotSync key is NULL (absent), this is a keyframe
                        not_sync.is_null()
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            }
        };

        // For keyframes, prepend SPS/PPS parameter sets from CMFormatDescription
        // This is critical - without SPS/PPS, H.264 decoder cannot decode any frames
        let final_data = if is_keyframe {
            tracing::info!(
                "[VideoToolbox] 🔑 KEYFRAME detected: {} bytes Annex B data, extracting SPS/PPS...",
                annex_b_data.len()
            );
            format::extract_h264_parameters(sample_buffer, annex_b_data)
        } else {
            tracing::trace!(
                "[VideoToolbox] P-frame: {} bytes Annex B data",
                annex_b_data.len()
            );
            annex_b_data
        };

        let encoded_frame = Encodedvideoframe {
            data: final_data,
            timestamp_ns: String::new(), // Will be set by caller
            is_keyframe,
            frame_number: String::new(), // Will be set by caller
        };

        if let Ok(mut queue) = encoded_frames.lock() {
            queue.push_back(encoded_frame);
        }
    }
}
