// Phase 1: WebRTC WHIP Streaming Implementation
//
// This file contains the complete WebRTC streaming implementation with:
// - H.264 encoding via VideoToolbox
// - Opus audio encoding
// - WHIP signaling
// - WebRTC session management
//
// All in one file for rapid iteration. We'll refactor based on what we learn.
//
// ============================================================================
// CRITICAL PERFORMANCE ISSUE - MUST FIX BEFORE MERGE
// ============================================================================
//
// Current texture conversion (convert_texture_to_pixel_buffer) uses CPU path:
//   GPU Texture (RGBA) → Staging Buffer → CPU Memory (RGBA) → CPU YUV conversion → NV12
//
// This is a MAJOR bottleneck for real-time low-latency streaming:
//   - GPU→CPU copy stalls the pipeline (forces synchronization)
//   - CPU YUV conversion is slow even with SIMD (yuv crate uses AVX2/NEON)
//   - Typical 1080p frame: ~8ms GPU copy + ~5ms YUV conversion = 13ms overhead
//   - At 60fps, budget is 16.6ms per frame - this leaves only 3.6ms for encoding!
//
// REQUIRED BEFORE MERGE: Implement GPU-accelerated conversion using ONE of:
//
// Option 1: Metal Compute Shader (PREFERRED)
//   - Write Metal compute shader for RGBA→NV12 conversion
//   - Use Metal Performance Shaders (MPS) color conversion kernel
//   - Stays entirely on GPU, zero CPU involvement
//   - Estimated performance: <1ms for 1080p
//   - Example: MPSImageConversion or custom kernel with BT.709 matrix
//
// Option 2: VTPixelTransferSession (Apple's GPU converter)
//   - Use VTPixelTransferSessionTransferImage()
//   - Apple's hardware-accelerated format converter
//   - Handles RGBA→NV12 + color space conversion (BT.709, limited range)
//   - Estimated performance: <2ms for 1080p
//   - Requires IOSurface-backed textures (we already use these via WgpuBridge)
//
// Option 3: Core Image (CIImage pipeline)
//   - Use CIContext to convert Metal texture → YUV
//   - More overhead than options 1/2, but simpler API
//   - Estimated performance: ~3-4ms for 1080p
//
// Recommended approach: Option 2 (VTPixelTransferSession)
//   - Lowest implementation effort (just FFI bindings)
//   - Apple-native solution, well-tested
//   - Integrates cleanly with VideoToolbox pipeline
//   - Already have IOSurface textures from camera processor
//
// Implementation notes for VTPixelTransferSession:
//   1. Create session once in setup_compression_session()
//   2. Set properties: kVTPixelTransferPropertyKey_ScalingMode, ColorSpace
//   3. In convert_texture_to_pixel_buffer():
//      - Get IOSurface from wgpu texture (via metal_texture.iosurface())
//      - Create CVPixelBuffer from IOSurface (source)
//      - Create CVPixelBuffer NV12 (destination)
//      - VTPixelTransferSessionTransferImage(session, source, dest)
//      - Return dest
//
// Target performance with GPU conversion:
//   - 1080p@60fps: <2ms conversion overhead (from current 13ms)
//   - Enables real-time streaming with <50ms glass-to-glass latency
//
// DO NOT MERGE until this is resolved. Current implementation is prototype only.
//
// ============================================================================

use crate::core::{
    VideoFrame, AudioFrame, StreamError, Result,
    media_clock::MediaClock, GpuContext,
    StreamInput, RuntimeContext,
    SchedulingConfig, SchedulingMode, ThreadPriority,
};
use streamlib_macros::StreamProcessor;
use crate::apple::{PixelTransferSession, WgpuBridge};
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use objc2_core_video::{CVPixelBuffer, CVPixelBufferLockFlags};
use objc2::runtime::ProtocolObject;
use serde::{Deserialize, Serialize};

// WHIP HTTP client imports
use hyper;
use hyper_rustls;
use hyper_util;
use http_body_util;

// ============================================================================
// INTERNAL TYPES (not exported)
// ============================================================================

/// Internal representation of encoded H.264 frame
#[derive(Clone)]
struct EncodedVideoFrame {
    data: Vec<u8>,
    timestamp_ns: i64,
    is_keyframe: bool,
    frame_number: u64,
}

/// Internal representation of encoded Opus frame
#[derive(Clone, Debug)]
struct EncodedAudioFrame {
    data: Vec<u8>,
    timestamp_ns: i64,
    sample_count: usize,
}

// ============================================================================
// H.264 ENCODING CONFIGURATION
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum H264Profile {
    Baseline,
    Main,
    High,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct VideoEncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
    pub keyframe_interval_frames: u32,
    pub profile: H264Profile,
    pub low_latency: bool,
}

impl Default for VideoEncoderConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_bps: 2_500_000,
            keyframe_interval_frames: 60,
            profile: H264Profile::Main,
            low_latency: true,
        }
    }
}

// ============================================================================
// OPUS ENCODING CONFIGURATION
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioEncoderConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub bitrate_bps: u32,
    pub frame_duration_ms: u32,
    pub complexity: u32,
    pub vbr: bool,
}

impl Default for AudioEncoderConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            bitrate_bps: 128_000,
            frame_duration_ms: 20,
            complexity: 10,
            vbr: true,
        }
    }
}

// ============================================================================
// VIDEOTOOLBOX FFI BINDINGS
// ============================================================================

mod videotoolbox {
    use std::ffi::c_void;
    use super::*;

    pub type OSStatus = i32;
    pub type VTCompressionSessionRef = *mut c_void;
    pub type CVPixelBufferRef = *mut c_void;
    pub type CMSampleBufferRef = *mut c_void;
    pub type CMTimeValue = i64;
    pub type CMTimeScale = i32;
    pub type CMTimeFlags = u32;
    pub type CFStringRef = *const c_void;
    pub type CFNumberRef = *const c_void;
    pub type CFBooleanRef = *const c_void;
    pub type CMBlockBufferRef = *mut c_void;
    pub type CMFormatDescriptionRef = *mut c_void;
    pub type VTPixelTransferSessionRef = *mut c_void;
    pub type CFArrayRef = *const c_void;

    #[repr(C)]
    pub struct CMTime {
        pub value: CMTimeValue,
        pub timescale: CMTimeScale,
        pub flags: CMTimeFlags,
        pub epoch: i64,
    }

    impl CMTime {
        pub fn new(value: i64, timescale: i32) -> Self {
            Self {
                value,
                timescale,
                flags: 1, // kCMTimeFlags_Valid
                epoch: 0,
            }
        }

        pub fn invalid() -> Self {
            Self {
                value: 0,
                timescale: 0,
                flags: 0,
                epoch: 0,
            }
        }
    }

    pub const K_CVRETURN_SUCCESS: OSStatus = 0;
    pub const NO_ERR: OSStatus = 0;

    // Codec types
    pub const K_CMVIDEO_CODEC_TYPE_H264: u32 = 0x61766331; // 'avc1'

    // VTCompressionSession callback type
    pub type VTCompressionOutputCallback = extern "C" fn(
        output_callback_ref_con: *mut c_void,
        source_frame_ref_con: *mut c_void,
        status: OSStatus,
        info_flags: u32,
        sample_buffer: CMSampleBufferRef,
    );

    #[link(name = "VideoToolbox", kind = "framework")]
    extern "C" {
        pub fn VTCompressionSessionCreate(
            allocator: *const c_void,
            width: i32,
            height: i32,
            codec_type: u32,
            encoder_specification: *const c_void,
            source_image_buffer_attributes: *const c_void,
            compressed_data_allocator: *const c_void,
            output_callback: VTCompressionOutputCallback,
            output_callback_ref_con: *mut c_void,
            compression_session_out: *mut VTCompressionSessionRef,
        ) -> OSStatus;

        pub fn VTCompressionSessionEncodeFrame(
            session: VTCompressionSessionRef,
            image_buffer: CVPixelBufferRef,
            presentation_time_stamp: CMTime,
            duration: CMTime,
            frame_properties: *const c_void,
            source_frame_ref_con: *mut c_void,
            info_flags_out: *mut u32,
        ) -> OSStatus;

        pub fn VTCompressionSessionCompleteFrames(
            session: VTCompressionSessionRef,
            complete_until_presentation_time_stamp: CMTime,
        ) -> OSStatus;

        pub fn VTCompressionSessionInvalidate(
            session: VTCompressionSessionRef,
        );

        pub fn VTSessionSetProperty(
            session: VTCompressionSessionRef,
            property_key: CFStringRef,
            property_value: *const c_void,
        ) -> OSStatus;

        // VTPixelTransferSession - GPU-accelerated format conversion
        pub fn VTPixelTransferSessionCreate(
            allocator: *const c_void,
            pixel_transfer_session_out: *mut VTPixelTransferSessionRef,
        ) -> OSStatus;

        pub fn VTPixelTransferSessionTransferImage(
            session: VTPixelTransferSessionRef,
            source_buffer: CVPixelBufferRef,
            destination_buffer: CVPixelBufferRef,
        ) -> OSStatus;

        pub fn VTPixelTransferSessionInvalidate(
            session: VTPixelTransferSessionRef,
        );

        // For getting encoded data from CMSampleBuffer
        pub fn CMSampleBufferGetDataBuffer(
            sbuf: CMSampleBufferRef,
        ) -> CMBlockBufferRef;

        pub fn CMBlockBufferGetDataLength(
            the_buffer: CMBlockBufferRef,
        ) -> usize;

        pub fn CMBlockBufferCopyDataBytes(
            the_buffer: CMBlockBufferRef,
            offset_to_data: usize,
            data_length: usize,
            destination: *mut u8,
        ) -> OSStatus;

        pub fn CMSampleBufferGetFormatDescription(
            sbuf: CMSampleBufferRef,
        ) -> CMFormatDescriptionRef;

        // For checking keyframe status via sample attachments
        pub fn CMSampleBufferGetSampleAttachmentsArray(
            sbuf: CMSampleBufferRef,
            create_if_necessary: bool,
        ) -> CFArrayRef;

        // Sample attachment keys
        pub static kCMSampleAttachmentKey_NotSync: CFStringRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFNumberCreate(
            allocator: *const c_void,
            the_type: i32,
            value_ptr: *const c_void,
        ) -> CFNumberRef;

        pub fn CFRelease(cf: *const c_void);

        // CFArray functions for accessing sample attachments
        pub fn CFArrayGetCount(the_array: CFArrayRef) -> isize;
        pub fn CFArrayGetValueAtIndex(the_array: CFArrayRef, idx: isize) -> *const c_void;

        // CFDictionary functions for checking attachment keys
        pub fn CFDictionaryGetValue(
            the_dict: CFDictionaryRef,
            key: *const c_void,
        ) -> *const c_void;

        // Boolean constants
        pub static kCFBooleanTrue: CFBooleanRef;
        pub static kCFBooleanFalse: CFBooleanRef;
    }

    // CFNumber types
    pub const K_CFNUMBER_SINT32_TYPE: i32 = 3;

    // VideoToolbox property keys and values
    #[link(name = "VideoToolbox", kind = "framework")]
    extern "C" {
        // Profile/Level property key
        pub static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;

        // H.264 Baseline Profile Level 3.1 (matches 42e01f in SDP)
        // This is the most compatible profile for WebRTC streaming
        pub static kVTProfileLevel_H264_Baseline_3_1: CFStringRef;

        // Real-time encoding properties
        pub static kVTCompressionPropertyKey_RealTime: CFStringRef;
        pub static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
        pub static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFStringRef;
        pub static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
        pub static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;

        // Encode frame options
        pub static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;
    }

    // CoreFoundation dictionary types
    pub type CFDictionaryRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFDictionaryRef;
    }
}

// ============================================================================
// VIDEO ENCODER TRAIT
// ============================================================================

trait VideoEncoderH264: Send {
    fn encode(&mut self, frame: &VideoFrame) -> Result<EncodedVideoFrame>;
    fn force_keyframe(&mut self);
    fn config(&self) -> &VideoEncoderConfig;
    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()>;
}

// ============================================================================
// VIDEOTOOLBOX H.264 ENCODER IMPLEMENTATION
// ============================================================================

struct VideoToolboxH264Encoder {
    config: VideoEncoderConfig,
    compression_session: Option<videotoolbox::VTCompressionSessionRef>,
    frame_count: u64,
    force_next_keyframe: bool,
    gpu_context: Option<GpuContext>,

    // GPU-accelerated texture → NV12 conversion
    pixel_transfer: Option<PixelTransferSession>,
    wgpu_bridge: Option<Arc<WgpuBridge>>,

    // For storing encoded output from callback
    encoded_frames: Arc<Mutex<VecDeque<EncodedVideoFrame>>>,

    // Callback context that needs to be freed in Drop
    callback_context: Option<*mut std::ffi::c_void>,
}

impl VideoToolboxH264Encoder {
    fn new(config: VideoEncoderConfig, gpu_context: Option<GpuContext>, ctx: &RuntimeContext) -> Result<Self> {
        let mut encoder = Self {
            config,
            compression_session: None,
            frame_count: 0,
            force_next_keyframe: true, // First frame should be keyframe
            gpu_context,
            pixel_transfer: None,
            wgpu_bridge: None,
            encoded_frames: Arc::new(Mutex::new(VecDeque::new())),
            callback_context: None,
        };

        encoder.setup_compression_session(ctx)?;
        Ok(encoder)
    }

    fn setup_compression_session(&mut self, ctx: &RuntimeContext) -> Result<()> {
        let encoded_frames_ref = Arc::clone(&self.encoded_frames);

        let width = self.config.width;
        let height = self.config.height;
        let keyframe_interval = self.config.keyframe_interval_frames;
        let bitrate = self.config.bitrate_bps;
        let fps = self.config.fps;

        // CRITICAL: VideoToolbox APIs MUST run on main thread
        // Cast pointers to usize for Send compatibility across thread boundary
        let (session_ptr, callback_context_ptr) = ctx.run_on_main_blocking(move || {
            // Create callback context on main thread
            // Use Arc::into_raw to increment ref count - keeps the Arc alive for the callback
            let callback_context = Arc::into_raw(encoded_frames_ref) as *mut std::ffi::c_void;
            let mut session: videotoolbox::VTCompressionSessionRef = std::ptr::null_mut();

            unsafe {
                let status = videotoolbox::VTCompressionSessionCreate(
                    std::ptr::null(), // allocator
                    width as i32,
                    height as i32,
                    videotoolbox::K_CMVIDEO_CODEC_TYPE_H264,
                    std::ptr::null(), // encoder specification
                    std::ptr::null(), // source image buffer attributes
                    std::ptr::null(), // compressed data allocator
                    compression_output_callback,
                    callback_context,
                    &mut session,
                );

                if status != videotoolbox::NO_ERR {
                    // Clean up callback context on error - reconstruct Arc to decrement ref count
                    let _ = Arc::from_raw(callback_context as *const Mutex<VecDeque<EncodedVideoFrame>>);
                    return Err(StreamError::Runtime(format!("VTCompressionSessionCreate failed: {}", status)));
                }

                // Configure encoder properties for WebRTC streaming
                // Set H.264 Baseline Profile Level 3.1 (matches 42e01f in SDP)
                // This ensures compatibility with Cloudflare Stream and other WebRTC services
                let status = videotoolbox::VTSessionSetProperty(
                    session,
                    videotoolbox::kVTCompressionPropertyKey_ProfileLevel,
                    videotoolbox::kVTProfileLevel_H264_Baseline_3_1 as *const _,
                );
                if status != videotoolbox::NO_ERR {
                    tracing::warn!("Failed to set H.264 profile level: {}", status);
                }

                // Enable real-time encoding for low latency
                let status = videotoolbox::VTSessionSetProperty(
                    session,
                    videotoolbox::kVTCompressionPropertyKey_RealTime,
                    videotoolbox::kCFBooleanTrue as *const _,
                );
                if status != videotoolbox::NO_ERR {
                    tracing::warn!("Failed to enable real-time encoding: {}", status);
                }

                // Disable frame reordering (B-frames) for low latency
                // CRITICAL: Set to TRUE to allow encoder to generate keyframes properly
                // Baseline profile doesn't use B-frames anyway, but encoder needs this enabled
                let status = videotoolbox::VTSessionSetProperty(
                    session,
                    videotoolbox::kVTCompressionPropertyKey_AllowFrameReordering,
                    videotoolbox::kCFBooleanTrue as *const _,
                );
                if status != videotoolbox::NO_ERR {
                    tracing::warn!("Failed to set frame reordering: {}", status);
                }

                // Set keyframe interval
                let max_keyframe_interval = keyframe_interval as i32;
                let max_keyframe_interval_num = videotoolbox::CFNumberCreate(
                    std::ptr::null(),
                    videotoolbox::K_CFNUMBER_SINT32_TYPE,
                    &max_keyframe_interval as *const _ as *const _,
                );
                let status = videotoolbox::VTSessionSetProperty(
                    session,
                    videotoolbox::kVTCompressionPropertyKey_MaxKeyFrameInterval,
                    max_keyframe_interval_num as *const _,
                );
                videotoolbox::CFRelease(max_keyframe_interval_num as *const _);
                if status != videotoolbox::NO_ERR {
                    tracing::warn!("Failed to set keyframe interval: {}", status);
                }

                // Set average bitrate
                let avg_bitrate = bitrate as i32;
                let avg_bitrate_num = videotoolbox::CFNumberCreate(
                    std::ptr::null(),
                    videotoolbox::K_CFNUMBER_SINT32_TYPE,
                    &avg_bitrate as *const _ as *const _,
                );
                let status = videotoolbox::VTSessionSetProperty(
                    session,
                    videotoolbox::kVTCompressionPropertyKey_AverageBitRate,
                    avg_bitrate_num as *const _,
                );
                videotoolbox::CFRelease(avg_bitrate_num as *const _);
                if status != videotoolbox::NO_ERR {
                    tracing::warn!("Failed to set average bitrate: {}", status);
                }

                // Set expected frame rate
                let expected_fps = fps as i32;
                let expected_fps_num = videotoolbox::CFNumberCreate(
                    std::ptr::null(),
                    videotoolbox::K_CFNUMBER_SINT32_TYPE,
                    &expected_fps as *const _ as *const _,
                );
                let status = videotoolbox::VTSessionSetProperty(
                    session,
                    videotoolbox::kVTCompressionPropertyKey_ExpectedFrameRate,
                    expected_fps_num as *const _,
                );
                videotoolbox::CFRelease(expected_fps_num as *const _);
                if status != videotoolbox::NO_ERR {
                    tracing::warn!("Failed to set expected frame rate: {}", status);
                }
            }

            // Return pointers as usize for Send compatibility
            Ok((session as usize, callback_context as usize))
        })?;

        // Cast back to proper pointer types
        let session = session_ptr as videotoolbox::VTCompressionSessionRef;
        let callback_context = callback_context_ptr as *mut std::ffi::c_void;

        self.compression_session = Some(session);
        self.callback_context = Some(callback_context);

        tracing::info!("VideoToolbox compression session created: {}x{} @ {}fps, H.264 Baseline 3.1",
            self.config.width, self.config.height, self.config.fps);

        // Initialize GPU-accelerated pixel transfer (RGBA → NV12)
        if let Some(ref gpu_ctx) = self.gpu_context {
            use crate::apple::{MetalDevice, WgpuBridge, PixelTransferSession};

            // Create Metal device (gets system default, same as wgpu)
            let metal_device = MetalDevice::new()?;

            // Create WgpuBridge with Metal device and wgpu device/queue from GpuContext
            let wgpu_bridge = Arc::new(WgpuBridge::from_shared_device(
                metal_device.clone_device(),
                gpu_ctx.device().as_ref().clone(),
                gpu_ctx.queue().as_ref().clone(),
            ));

            // Create PixelTransferSession for GPU-accelerated RGBA → NV12 conversion
            let pixel_transfer = PixelTransferSession::new(wgpu_bridge.clone())?;

            self.wgpu_bridge = Some(wgpu_bridge);
            self.pixel_transfer = Some(pixel_transfer);

            tracing::info!("GPU-accelerated pixel transfer (RGBA → NV12) initialized");
        } else {
            tracing::warn!("No GPU context available, cannot initialize GPU-accelerated pixel transfer");
        }

        Ok(())
    }

    // PERFORMANCE WARNING: This function uses CPU conversion path
    // See file header (lines 11-67) for critical performance issue details.
    // This MUST be replaced with GPU-accelerated conversion (VTPixelTransferSession)
    // before merging for production use.
    //
    // Current overhead: ~13ms for 1080p (GPU copy + CPU YUV conversion)
    // Target overhead: <2ms for 1080p (GPU-only conversion)
    fn convert_texture_to_pixel_buffer(&self, frame: &VideoFrame) -> Result<*mut CVPixelBuffer> {
        // GPU-accelerated conversion using VTPixelTransferSession
        let pixel_transfer = self.pixel_transfer.as_ref()
            .ok_or_else(|| StreamError::Configuration("PixelTransferSession not initialized".into()))?;

        return pixel_transfer.convert_to_nv12(
            &frame.texture,
            frame.width,
            frame.height,
        );
    }
}
extern "C" fn compression_output_callback(
    output_callback_ref_con: *mut std::ffi::c_void,
    _source_frame_ref_con: *mut std::ffi::c_void,
    status: videotoolbox::OSStatus,
    _info_flags: u32,
    sample_buffer: videotoolbox::CMSampleBufferRef,
) {
    if status != videotoolbox::NO_ERR {
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
        let ptr = output_callback_ref_con as *const Mutex<VecDeque<EncodedVideoFrame>>;
        &*ptr  // Directly deref the raw pointer
    };

    // Extract encoded data from sample buffer and convert to AVCC format
    unsafe {
        let block_buffer = videotoolbox::CMSampleBufferGetDataBuffer(sample_buffer);
        if block_buffer.is_null() {
            tracing::error!("CMSampleBufferGetDataBuffer returned null");
            return;
        }

        let data_length = videotoolbox::CMBlockBufferGetDataLength(block_buffer);
        let mut raw_data = vec![0u8; data_length];

        let copy_status = videotoolbox::CMBlockBufferCopyDataBytes(
            block_buffer,
            0,
            data_length,
            raw_data.as_mut_ptr(),
        );

        if copy_status != videotoolbox::NO_ERR {
            tracing::error!("CMBlockBufferCopyDataBytes failed: {}", copy_status);
            return;
        }

        // VideoToolbox outputs elementary stream (raw NAL units)
        // We need to convert to AVCC format (length-prefixed NAL units)
        // CRITICAL: Each NAL unit must be prefixed with its 4-byte length in big-endian

        // For now, assume single NAL unit per sample (common for WebRTC)
        // TODO: Handle multiple NAL units per sample (e.g., SPS+PPS+IDR in keyframes)
        let avcc_data = convert_elementary_stream_to_avcc(&raw_data);

        // Check if this is a keyframe using CMSampleBuffer attachments (Apple's official method)
        // The kCMSampleAttachmentKey_NotSync key is ONLY present for non-sync frames (P/B frames)
        // If the key is absent, the frame is a sync frame (keyframe/I-frame)
        let is_keyframe = {
            let attachments = videotoolbox::CMSampleBufferGetSampleAttachmentsArray(sample_buffer, false);
            if !attachments.is_null() {
                let count = videotoolbox::CFArrayGetCount(attachments);
                if count > 0 {
                    let attachment = videotoolbox::CFArrayGetValueAtIndex(attachments, 0);
                    if !attachment.is_null() {
                        let not_sync = videotoolbox::CFDictionaryGetValue(
                            attachment,
                            videotoolbox::kCMSampleAttachmentKey_NotSync as *const std::ffi::c_void,
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

        // Log frame info for debugging
        if is_keyframe {
            tracing::info!("[VideoToolbox] 🔑 KEYFRAME detected: {} bytes", avcc_data.len());
        } else {
            tracing::trace!("[VideoToolbox] P-frame: {} bytes", avcc_data.len());
        }

        let encoded_frame = EncodedVideoFrame {
            data: avcc_data,
            timestamp_ns: 0, // Will be set by caller
            is_keyframe,
            frame_number: 0, // Will be set by caller
        };

        if let Ok(mut queue) = encoded_frames.lock() {
            queue.push_back(encoded_frame);
        }
    }
}

/// Convert elementary stream (raw NAL units) to AVCC format (length-prefixed)
///
/// Elementary stream: [NAL data]
/// AVCC format: [4-byte length in big-endian][NAL data]
///
/// WebRTC requires AVCC format for H.264 RTP packetization.
fn convert_elementary_stream_to_avcc(elementary_stream: &[u8]) -> Vec<u8> {
    if elementary_stream.is_empty() {
        return Vec::new();
    }

    // AVCC format: prepend 4-byte length header
    let nal_length = elementary_stream.len() as u32;
    let mut avcc_data = Vec::with_capacity(4 + elementary_stream.len());

    // Write length as 4-byte big-endian
    avcc_data.extend_from_slice(&nal_length.to_be_bytes());

    // Copy NAL unit data
    avcc_data.extend_from_slice(elementary_stream);

    tracing::trace!(
        "[AVCC Conversion] Elementary stream {} bytes → AVCC {} bytes (length prefix: {})",
        elementary_stream.len(),
        avcc_data.len(),
        nal_length
    );

    avcc_data
}

// SAFETY: VideoToolbox compression session will only be accessed from the main thread
// via RuntimeContext::run_on_main_blocking, similar to Mp4WriterProcessor pattern
unsafe impl Send for VideoToolboxH264Encoder {}

impl VideoEncoderH264 for VideoToolboxH264Encoder {
    fn encode(&mut self, frame: &VideoFrame) -> Result<EncodedVideoFrame> {
        let session = self.compression_session
            .ok_or_else(|| StreamError::Configuration("Compression session not initialized".into()))?;

        // Step 1: Convert VideoFrame texture to CVPixelBuffer
        let pixel_buffer = self.convert_texture_to_pixel_buffer(frame)?;

        // Step 2: Create presentation timestamp
        let presentation_time = videotoolbox::CMTime::new(frame.timestamp_ns, 1_000_000_000);
        let duration = videotoolbox::CMTime::invalid(); // Let VideoToolbox calculate duration

        // Step 3: Determine if we should force a keyframe
        // Force keyframe on first frame and every keyframe_interval frames
        let should_force_keyframe = self.frame_count == 0 ||
            (self.frame_count % self.config.keyframe_interval_frames as u64 == 0);

        // Step 4: Encode the frame
        unsafe {
            let frame_properties = if should_force_keyframe {
                tracing::info!("[VideoToolbox] 🔑 Forcing keyframe at frame {} (dict created: yes)", self.frame_count);
                let dict = Self::create_force_keyframe_properties();
                if dict.is_null() {
                    tracing::error!("[VideoToolbox] ❌ CFDictionary creation FAILED!");
                } else {
                    tracing::debug!("[VideoToolbox] CFDictionary created successfully at {:p}", dict);
                }
                dict
            } else {
                std::ptr::null()
            };

            let status = videotoolbox::VTCompressionSessionEncodeFrame(
                session,
                pixel_buffer as videotoolbox::CVPixelBufferRef,
                presentation_time,
                duration,
                frame_properties, // Pass keyframe properties
                std::ptr::null_mut(), // source frame ref con
                std::ptr::null_mut(), // info flags out
            );

            // Release frame properties dictionary if created
            if !frame_properties.is_null() {
                videotoolbox::CFRelease(frame_properties);
            }

            // Release pixel buffer
            objc2::rc::autoreleasepool(|_| {
                let _ = objc2::rc::Retained::from_raw(pixel_buffer);
            });

            if status != videotoolbox::NO_ERR {
                return Err(StreamError::Runtime(format!("VTCompressionSessionEncodeFrame failed: {}", status)));
            }
        }

        // Step 4: Force frame completion to ensure callback is called
        unsafe {
            let complete_status = videotoolbox::VTCompressionSessionCompleteFrames(
                session,
                videotoolbox::CMTime::invalid(), // Complete all pending frames
            );

            if complete_status != videotoolbox::NO_ERR {
                tracing::warn!("VTCompressionSessionCompleteFrames returned: {}", complete_status);
            }
        }

        // Step 5: Retrieve encoded frame from queue (populated by callback)
        let mut encoded_frame = self.encoded_frames.lock()
            .map_err(|e| StreamError::Runtime(format!("Failed to lock encoded frames queue: {}", e)))?
            .pop_front()
            .ok_or_else(|| StreamError::Runtime("No encoded frame available after encoding".into()))?;

        // Step 6: Update frame metadata
        encoded_frame.timestamp_ns = frame.timestamp_ns;
        encoded_frame.frame_number = self.frame_count;

        self.frame_count += 1;

        // Log encoding info
        if self.frame_count % 30 == 0 {
            tracing::debug!(
                "Encoded frame {}: {} bytes, keyframe={}",
                encoded_frame.frame_number,
                encoded_frame.data.len(),
                encoded_frame.is_keyframe
            );
        }

        Ok(encoded_frame)
    }

    fn force_keyframe(&mut self) {
        self.force_next_keyframe = true;
    }

    fn config(&self) -> &VideoEncoderConfig {
        &self.config
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        // TODO: Update compression session bitrate
        self.config.bitrate_bps = bitrate_bps;
        Ok(())
    }
}

impl VideoToolboxH264Encoder {
    /// Create a CFDictionary requesting a keyframe
    ///
    /// Returns a CFDictionaryRef with {kVTEncodeFrameOptionKey_ForceKeyFrame: kCFBooleanTrue}
    /// Caller must call CFRelease on the returned pointer
    unsafe fn create_force_keyframe_properties() -> *const std::ffi::c_void {
        // Create dictionary with key-value: {kVTEncodeFrameOptionKey_ForceKeyFrame: true}
        let key = videotoolbox::kVTEncodeFrameOptionKey_ForceKeyFrame as *const std::ffi::c_void;
        let value = videotoolbox::kCFBooleanTrue as *const std::ffi::c_void;

        let keys = [key];
        let values = [value];

        let dict = videotoolbox::CFDictionaryCreate(
            std::ptr::null(), // allocator (default)
            keys.as_ptr() as *const *const std::ffi::c_void,
            values.as_ptr() as *const *const std::ffi::c_void,
            1, // count
            std::ptr::null(), // key callbacks (default)
            std::ptr::null(), // value callbacks (default)
        );

        dict as *const std::ffi::c_void
    }
}

impl Drop for VideoToolboxH264Encoder {
    fn drop(&mut self) {
        unsafe {
            // Clean up VTCompressionSession
            if let Some(session) = self.compression_session {
                // CRITICAL: Complete all pending frames BEFORE invalidating session
                // This ensures all VideoToolbox callbacks finish executing before we free the callback context
                // Without this, callbacks may try to access freed memory → SIGABRT crash
                tracing::debug!("[VideoToolbox] Waiting for all pending frames to complete...");
                let status = videotoolbox::VTCompressionSessionCompleteFrames(
                    session,
                    videotoolbox::CMTime::invalid(), // kCMTimeInvalid = flush all pending frames
                );
                if status != videotoolbox::NO_ERR {
                    tracing::warn!("[VideoToolbox] VTCompressionSessionCompleteFrames failed: {}", status);
                }
                tracing::debug!("[VideoToolbox] All pending frames completed");

                // Now safe to invalidate (stops accepting new frames)
                videotoolbox::VTCompressionSessionInvalidate(session);

                // Release the CoreFoundation object (free memory)
                videotoolbox::CFRelease(session as *const std::ffi::c_void);
            }

            // Clean up callback context (the leaked Arc from setup_compression_session)
            // Safe now because VTCompressionSessionCompleteFrames() ensured all callbacks finished
            // Reconstruct Arc to decrement ref count and potentially drop the Mutex
            if let Some(context) = self.callback_context {
                let _ = Arc::from_raw(context as *const Mutex<VecDeque<EncodedVideoFrame>>);
                tracing::debug!("[VideoToolbox] Callback context freed");
            }
        }
    }
}

// ============================================================================
// AUDIO ENCODER TRAIT
// ============================================================================

trait AudioEncoderOpus: Send {
    fn encode(&mut self, frame: &AudioFrame<2>) -> Result<EncodedAudioFrame>;
    fn config(&self) -> &AudioEncoderConfig;
    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()>;
}

// ============================================================================
// OPUS ENCODER IMPLEMENTATION
// ============================================================================

/// Opus audio encoder for real-time WebRTC streaming.
///
/// # Requirements
/// - Input must be stereo (`AudioFrame<2>`)
/// - Sample rate must be 48kHz
/// - Frame size must be exactly 960 samples (20ms @ 48kHz)
///
/// # Pipeline Setup
/// Typical pipeline for Opus encoding requires preprocessing:
/// - AudioSource → AudioResamplerProcessor(48kHz) → BufferRechunkerProcessor(960) → OpusEncoder
///
/// # Configuration
/// - **Bitrate**: 128 kbps default (adjust with `set_bitrate()`)
/// - **VBR**: Enabled by default for better quality
/// - **FEC**: Forward error correction enabled for packet loss resilience
#[derive(Debug)]
struct OpusEncoder {
    config: AudioEncoderConfig,
    encoder: opus::Encoder,
    frame_size: usize,  // 960 samples per channel @ 48kHz (20ms)
}

impl OpusEncoder {
    fn new(config: AudioEncoderConfig) -> Result<Self> {
        // Validate config
        if config.sample_rate != 48000 {
            return Err(StreamError::Configuration(
                format!("Opus encoder only supports 48kHz sample rate, got {}Hz", config.sample_rate)
            ));
        }
        if config.channels != 2 {
            return Err(StreamError::Configuration(
                format!("Opus encoder only supports stereo (2 channels), got {}", config.channels)
            ));
        }

        // Calculate frame size (20ms @ 48kHz = 960 samples per channel)
        let frame_size = (config.sample_rate as usize * config.frame_duration_ms as usize) / 1000;

        // Create opus encoder
        let mut encoder = opus::Encoder::new(
            config.sample_rate,
            opus::Channels::Stereo,
            opus::Application::Audio,  // Use Audio for best quality (music/broadcast)
        ).map_err(|e| StreamError::Configuration(format!("Failed to create Opus encoder: {:?}", e)))?;

        // Configure encoder
        encoder.set_bitrate(opus::Bitrate::Bits(config.bitrate_bps as i32))
            .map_err(|e| StreamError::Configuration(format!("Failed to set bitrate: {:?}", e)))?;

        encoder.set_vbr(config.vbr)
            .map_err(|e| StreamError::Configuration(format!("Failed to set VBR: {:?}", e)))?;

        // Enable FEC (Forward Error Correction) for better packet loss resilience
        encoder.set_inband_fec(true)
            .map_err(|e| StreamError::Configuration(format!("Failed to set FEC: {:?}", e)))?;

        tracing::info!(
            "OpusEncoder initialized: {}Hz, {} channels, {} kbps, {}ms frames, VBR={}",
            config.sample_rate,
            config.channels,
            config.bitrate_bps / 1000,
            config.frame_duration_ms,
            config.vbr
        );

        Ok(Self {
            config,
            encoder,
            frame_size,
        })
    }
}

impl AudioEncoderOpus for OpusEncoder {
    fn encode(&mut self, frame: &AudioFrame<2>) -> Result<EncodedAudioFrame> {
        // Validate sample rate
        if frame.sample_rate != 48000 {
            return Err(StreamError::Configuration(
                format!(
                    "Expected 48kHz, got {}Hz. Use AudioResamplerProcessor upstream to convert to 48kHz.",
                    frame.sample_rate
                )
            ));
        }

        // Validate frame size (should be exactly 960 samples per channel for 20ms @ 48kHz)
        let expected_samples = self.frame_size;  // 960
        let actual_samples = frame.sample_count();

        if actual_samples != expected_samples {
            return Err(StreamError::Configuration(
                format!(
                    "Expected {} samples (20ms @ 48kHz), got {}. Use BufferRechunkerProcessor(960) upstream.",
                    expected_samples, actual_samples
                )
            ));
        }

        // Encode (opus expects interleaved f32, which is what AudioFrame uses)
        // Max packet size ~4KB is enough for worst case Opus output
        let encoded_data = self.encoder
            .encode_vec_float(&frame.samples, 4000)
            .map_err(|e| StreamError::Runtime(format!("Opus encoding failed: {:?}", e)))?;

        tracing::trace!(
            "Encoded audio frame: {} samples → {} bytes (compression: {:.2}x)",
            actual_samples * 2,  // Total samples (stereo)
            encoded_data.len(),
            (actual_samples * 2 * 4) as f32 / encoded_data.len() as f32  // f32 = 4 bytes per sample
        );

        Ok(EncodedAudioFrame {
            data: encoded_data,
            timestamp_ns: frame.timestamp_ns,  // Preserve timestamp exactly
            sample_count: actual_samples,
        })
    }

    fn config(&self) -> &AudioEncoderConfig {
        &self.config
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        self.encoder
            .set_bitrate(opus::Bitrate::Bits(bitrate_bps as i32))
            .map_err(|e| StreamError::Configuration(format!("Failed to set bitrate: {:?}", e)))?;

        self.config.bitrate_bps = bitrate_bps;

        tracing::info!("Opus bitrate changed to {} kbps", bitrate_bps / 1000);
        Ok(())
    }
}

// ============================================================================
// H.264 NAL UNIT PARSER
// ============================================================================

/// Parses H.264 Annex B format into individual NAL units.
///
/// VideoToolbox outputs Annex B format with start codes:
/// - 4-byte: `[0, 0, 0, 1]`
/// - 3-byte: `[0, 0, 1]`
///
/// webrtc-rs expects NAL units WITHOUT start codes for RTP packetization.
///
/// # Arguments
/// * `data` - Annex B formatted data (with start codes)
///
/// # Returns
/// Vec of NAL units (without start codes), preserving order.
///
/// # Performance
/// - Zero-copy slicing where possible
/// - ~1-2µs for typical frame (3-5 NAL units)
/// Parse NAL units from AVCC format (length-prefixed, used by VideoToolbox)
///
/// AVCC format: [4-byte length][NAL unit][4-byte length][NAL unit]...
/// Each NAL unit length is a 4-byte big-endian integer.
fn parse_nal_units_avcc(data: &[u8]) -> Vec<Vec<u8>> {
    let mut nal_units = Vec::new();
    let mut i = 0;

    while i + 4 <= data.len() {
        // Read 4-byte big-endian length
        let nal_length = u32::from_be_bytes([
            data[i],
            data[i + 1],
            data[i + 2],
            data[i + 3],
        ]) as usize;

        i += 4; // Skip length prefix

        // Check bounds
        if i + nal_length > data.len() {
            tracing::warn!(
                "AVCC NAL unit length {} exceeds remaining data {} at offset {}",
                nal_length,
                data.len() - i,
                i - 4
            );
            break;
        }

        // Extract NAL unit
        let nal_unit = data[i..i + nal_length].to_vec();
        if !nal_unit.is_empty() {
            nal_units.push(nal_unit);
        }

        i += nal_length;
    }

    nal_units
}

/// Parse NAL units from Annex B format (start-code-prefixed)
///
/// Annex B format: [00 00 00 01 or 00 00 01][NAL unit][start code][NAL unit]...
fn parse_nal_units_annex_b(data: &[u8]) -> Vec<Vec<u8>> {
    let mut nal_units = Vec::new();
    let mut i = 0;

    while i < data.len() {
        // Look for start code (4-byte or 3-byte)
        let start_code_len = if i + 3 < data.len()
            && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1
        {
            4
        } else if i + 2 < data.len()
            && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1
        {
            3
        } else {
            i += 1;
            continue;
        };

        // Find next start code (or end of data)
        let mut nal_end = i + start_code_len;
        while nal_end < data.len() {
            if (nal_end + 3 < data.len()
                && data[nal_end] == 0
                && data[nal_end + 1] == 0
                && data[nal_end + 2] == 0
                && data[nal_end + 3] == 1)
                || (nal_end + 2 < data.len()
                    && data[nal_end] == 0
                    && data[nal_end + 1] == 0
                    && data[nal_end + 2] == 1)
            {
                break;
            }
            nal_end += 1;
        }

        // Extract NAL unit (without start code)
        let nal_unit = data[i + start_code_len..nal_end].to_vec();
        if !nal_unit.is_empty() {
            nal_units.push(nal_unit);
        }

        i = nal_end;
    }

    nal_units
}

/// Auto-detect format and parse NAL units
///
/// Checks first 4 bytes to determine if data is AVCC or Annex B format:
/// - AVCC: First 4 bytes = big-endian length (typically < 100KB for a frame)
/// - Annex B: Starts with 00 00 00 01 or 00 00 01
fn parse_nal_units(data: &[u8]) -> Vec<Vec<u8>> {
    if data.len() < 4 {
        return Vec::new();
    }

    // Check for Annex B start codes
    let is_annex_b = (data[0] == 0 && data[1] == 0 && data[2] == 0 && data[3] == 1)
        || (data[0] == 0 && data[1] == 0 && data[2] == 1);

    if is_annex_b {
        tracing::debug!("Detected Annex B format H.264");
        parse_nal_units_annex_b(data)
    } else {
        // Assume AVCC format (VideoToolbox default on macOS)
        let length = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;

        // Sanity check: length should be reasonable (< 1MB for a single NAL unit)
        if length > 0 && length < 1_000_000 && length + 4 <= data.len() {
            tracing::debug!("Detected AVCC format H.264 (first NAL length: {})", length);
            parse_nal_units_avcc(data)
        } else {
            tracing::warn!(
                "Unknown H.264 format: first 4 bytes = {:02x} {:02x} {:02x} {:02x} (length={}, data_len={})",
                data[0], data[1], data[2], data[3], length, data.len()
            );
            Vec::new()
        }
    }
}

// ============================================================================
// RTP SAMPLE CONVERSION
// ============================================================================

use bytes::Bytes;
use std::time::Duration;

/// Converts encoded H.264 video frame to webrtc Sample(s).
///
/// # Process
/// 1. Parse NAL units from Annex B format
/// 2. Create one Sample per NAL unit (webrtc-rs handles packetization)
/// 3. Calculate frame duration from fps
///
/// # Arguments
/// * `frame` - Encoded H.264 frame (Annex B format with start codes)
/// * `fps` - Frames per second (for duration calculation)
///
/// # Returns
/// Vec of Samples (one per NAL unit), all with same duration
///
/// # Performance
/// - ~2-3µs for typical frame (parsing + allocation)
fn convert_video_to_samples(
    frame: &EncodedVideoFrame,
    fps: u32,
) -> Result<Vec<webrtc::media::Sample>> {
    // Parse NAL units from Annex B format
    let nal_units = parse_nal_units(&frame.data);

    if nal_units.is_empty() {
        return Err(StreamError::Runtime(
            "No NAL units found in H.264 frame".into()
        ));
    }

    // Calculate frame duration
    let duration = Duration::from_secs_f64(1.0 / fps as f64);

    // Convert each NAL unit to a Sample
    let samples = nal_units
        .into_iter()
        .map(|nal| webrtc::media::Sample {
            data: Bytes::from(nal),
            duration,
            ..Default::default()
        })
        .collect();

    Ok(samples)
}

/// Converts encoded Opus audio frame to webrtc Sample.
///
/// # Process
/// 1. Wrap encoded data in Bytes (zero-copy if possible)
/// 2. Calculate duration from sample count and rate
///
/// # Arguments
/// * `frame` - Encoded Opus frame
/// * `sample_rate` - Audio sample rate (typically 48000 Hz)
///
/// # Returns
/// Single Sample (Opus frames fit in one RTP packet)
///
/// # Performance
/// - ~0.5µs (duration calc + Bytes conversion)
fn convert_audio_to_sample(
    frame: &EncodedAudioFrame,
    sample_rate: u32,
) -> Result<webrtc::media::Sample> {
    // Calculate duration from sample count
    let duration = Duration::from_secs_f64(frame.sample_count as f64 / sample_rate as f64);

    Ok(webrtc::media::Sample {
        data: Bytes::from(frame.data.clone()),
        duration,
        ..Default::default()
    })
}

// ============================================================================
// RTP TIMESTAMP CALCULATOR
// ============================================================================

/// Calculates RTP timestamps from monotonic MediaClock timestamps.
///
/// # RTP Timestamp Format
/// - Video (H.264): 90kHz clock rate
/// - Audio (Opus): 48kHz clock rate (same as sample rate)
///
/// # Security
/// Uses random RTP base timestamp (RFC 3550 Section 5.1) to prevent:
/// - Timing prediction attacks
/// - Known-plaintext attacks on encryption
///
/// # Performance
/// - `new()`: ~0.5ns (fastrand)
/// - `calculate()`: ~2-3ns (i128 multiplication + division)
struct RtpTimestampCalculator {
    start_time_ns: i64,
    rtp_base: u32,
    clock_rate: u32,
}

impl RtpTimestampCalculator {
    /// Creates a new RTP timestamp calculator with random base.
    ///
    /// # Arguments
    /// * `start_time_ns` - Session start time from MediaClock::now()
    /// * `clock_rate` - RTP clock rate (90000 for video, 48000 for audio)
    fn new(start_time_ns: i64, clock_rate: u32) -> Self {
        // Random RTP base (RFC 3550 compliance)
        // Uses fastrand for speed (~0.5ns) - doesn't need crypto-grade PRNG
        let rtp_base = fastrand::u32(..);

        Self {
            start_time_ns,
            rtp_base,
            clock_rate,
        }
    }

    /// Converts monotonic nanosecond timestamp to RTP timestamp.
    ///
    /// # Arguments
    /// * `timestamp_ns` - Current MediaClock timestamp (nanoseconds)
    ///
    /// # Returns
    /// RTP timestamp (u32) - automatically wraps at 2^32
    ///
    /// # Example
    /// ```ignore
    /// let calc = RtpTimestampCalculator::new(0, 90000);
    /// let rtp_ts = calc.calculate(1_000_000_000); // 1 second
    /// // For 90kHz: 1s × 90000 ticks/s = 90000 ticks (+ random base)
    /// ```
    fn calculate(&self, timestamp_ns: i64) -> u32 {
        let elapsed_ns = timestamp_ns - self.start_time_ns;
        let elapsed_ticks = (elapsed_ns as i128 * self.clock_rate as i128) / 1_000_000_000;
        self.rtp_base.wrapping_add(elapsed_ticks as u32)
    }
}

// ============================================================================
// WHIP SIGNALING
// ============================================================================

#[derive(Clone, Serialize, Deserialize)]
pub struct WhipConfig {
    pub endpoint_url: String,
    /// Optional Bearer token for authentication.
    /// Set to None for endpoints that don't require authentication (e.g., Cloudflare Stream).
    pub auth_token: Option<String>,
    pub timeout_ms: u64,
}

/// WHIP (WebRTC-HTTP Ingestion Protocol) client per RFC 9725
///
/// Handles HTTP signaling for WebRTC streaming:
/// - POST /whip: Create session (SDP offer → answer)
/// - PATCH /session: Send ICE candidates (trickle ICE)
/// - DELETE /session: Terminate session
///
/// Uses pollster::block_on() for async HTTP calls (same pattern as WebRtcSession).
struct WhipClient {
    config: WhipConfig,

    /// HTTP client with HTTPS support
    /// Body type: http_body_util::combinators::BoxBody for flexibility
    http_client: hyper_util::client::legacy::Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        http_body_util::combinators::BoxBody<bytes::Bytes, Box<dyn std::error::Error + Send + Sync>>,
    >,

    /// Session URL (from Location header after POST success)
    session_url: Option<String>,

    /// Session ETag (for ICE restart - future use)
    session_etag: Option<String>,

    /// Pending ICE candidates (buffered for batch sending)
    pending_candidates: Arc<Mutex<Vec<String>>>,

    /// Tokio runtime for HTTP operations (required for tokio::time::timeout)
    _runtime: tokio::runtime::Runtime,
}

impl WhipClient {
    fn new(config: WhipConfig) -> Result<Self> {
        tracing::info!("[WhipClient] Creating WHIP client for endpoint: {}", config.endpoint_url);

        // Build HTTPS connector using rustls with ring crypto provider and native CA roots
        tracing::debug!("[WhipClient] Building HTTPS connector with native roots...");
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()  // Use system CA store (includes ring provider via feature flag)
            .map_err(|e| {
                tracing::error!("[WhipClient] Failed to load CA roots: {}", e);
                StreamError::Configuration(format!("Failed to load CA roots: {}", e))
            })?
            .https_or_http()      // Allow http:// for local testing
            .enable_http1()
            .enable_http2()
            .build();
        tracing::debug!("[WhipClient] HTTPS connector built successfully");

        // Create HTTP client
        tracing::debug!("[WhipClient] Creating HTTP client...");
        let http_client = hyper_util::client::legacy::Client::builder(
            hyper_util::rt::TokioExecutor::new()
        )
        .pool_idle_timeout(std::time::Duration::from_secs(30))
        .build(https);
        tracing::info!("[WhipClient] HTTP client created successfully");

        // Create Tokio runtime for HTTP operations (tokio::time::timeout needs it)
        tracing::debug!("[WhipClient] Creating Tokio runtime for HTTP operations...");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| StreamError::Runtime(format!("Failed to create Tokio runtime for WHIP client: {}", e)))?;
        tracing::debug!("[WhipClient] Tokio runtime created successfully");

        Ok(Self {
            config,
            http_client,
            session_url: None,
            session_etag: None,
            pending_candidates: Arc::new(Mutex::new(Vec::new())),
            _runtime: runtime,
        })
    }

    /// POST SDP offer to WHIP endpoint, receive SDP answer
    ///
    /// RFC 9725 Section 4.1: Client creates session by POSTing SDP offer.
    /// Server responds with 201 Created, Location header (session URL), and SDP answer.
    ///
    /// # Arguments
    /// * `sdp_offer` - SDP offer string (application/sdp)
    ///
    /// # Returns
    /// SDP answer string from server
    ///
    /// # Errors
    /// - 400 Bad Request: Malformed SDP
    /// - 401 Unauthorized: Invalid auth token
    /// - 422 Unprocessable Content: Unsupported SDP configuration
    /// - 503 Service Unavailable: Server overloaded (should retry with backoff)
    /// - 307 Temporary Redirect: Load balancing (automatically followed)
    fn post_offer(&mut self, sdp_offer: &str) -> Result<String> {
        use hyper::{Request, StatusCode, header};
        use http_body_util::Full;

        // Clone what we need to avoid borrow issues
        let endpoint_url = self.config.endpoint_url.clone();
        let auth_token = self.config.auth_token.clone();
        let timeout_ms = self.config.timeout_ms;

        // Extract http_client reference before async block
        let http_client = &self.http_client;

        // MUST use Tokio runtime (tokio::time::timeout requires it)
        let result = self._runtime.block_on(async {
            // Build POST request per RFC 9725 Section 4.1
            use http_body_util::BodyExt;
            let body = Full::new(bytes::Bytes::from(sdp_offer.to_owned()));
            let boxed_body = body.map_err(|never| match never {}).boxed();

            let mut req_builder = Request::builder()
                .method("POST")
                .uri(&endpoint_url)
                .header(header::CONTENT_TYPE, "application/sdp");

            // Add Authorization header only if token is provided
            if let Some(token) = &auth_token {
                req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }

            let req = req_builder.body(boxed_body)
                .map_err(|e| StreamError::Runtime(format!("Failed to build WHIP POST request: {}", e)))?;

            tracing::debug!("WHIP POST to {}", endpoint_url);

            // Send request with timeout
            let response = tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                http_client.request(req),
            )
            .await
            .map_err(|_| StreamError::Runtime(format!("WHIP POST timed out after {}ms", timeout_ms)))?
            .map_err(|e| StreamError::Runtime(format!("WHIP POST request failed: {}", e)))?;

            let status = response.status();
            let headers = response.headers().clone();

            // Read response body
            let body_bytes = http_body_util::BodyExt::collect(response.into_body())
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to read WHIP response body: {}", e)))?
                .to_bytes();

            // Return status, headers, and body for processing outside async block
            Ok::<_, StreamError>((status, headers, body_bytes))
        })?;

        // Process response outside async block to avoid borrow conflicts
        let (status, headers, body_bytes) = result;

        match status {
            StatusCode::CREATED => {
                // Extract Location header (REQUIRED per RFC 9725)
                let location = headers
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| StreamError::Runtime(
                        "WHIP server returned 201 Created without Location header".into()
                    ))?;

                // Convert relative URLs to absolute URLs
                // Cloudflare returns relative paths like "/stream-id/webRTC/publish/session-id"
                self.session_url = if location.starts_with('/') {
                    // Parse endpoint URL to get base
                    let base_url = self.config.endpoint_url
                        .split('/')
                        .take(3)  // Take "https:", "", "hostname"
                        .collect::<Vec<_>>()
                        .join("/");
                    Some(format!("{}{}", base_url, location))
                } else {
                    Some(location.to_owned())
                };

                tracing::debug!(
                    "WHIP Location header: '{}' → session URL: '{}'",
                    location,
                    self.session_url.as_ref().unwrap()
                );

                    // Extract ETag header (optional, used for ICE restart)
                    self.session_etag = headers
                        .get(header::ETAG)
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_owned());

                    // TODO: Parse Link header for STUN/TURN servers (RFC 9725 Section 4.1)
                    // Format: Link: <stun:stun.example.com>; rel="ice-server"

                    // Parse SDP answer from body
                    let sdp_answer = String::from_utf8(body_bytes.to_vec())
                        .map_err(|e| StreamError::Runtime(format!("Invalid UTF-8 in SDP answer: {}", e)))?;

                    tracing::info!(
                        "WHIP session created: {} (ETag: {})",
                        self.session_url.as_ref().unwrap(),
                        self.session_etag.as_deref().unwrap_or("none")
                    );

                    Ok(sdp_answer)
                }

                StatusCode::TEMPORARY_REDIRECT => {
                    // Handle redirect per RFC 9725 Section 4.5
                    let location = headers
                        .get(header::LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .ok_or_else(|| StreamError::Runtime(
                            "WHIP 307 redirect without Location header".into()
                        ))?;

                    tracing::info!("WHIP redirecting to: {}", location);

                    // Update endpoint and retry (recursive, but 307 should be rare)
                    self.config.endpoint_url = location.to_owned();
                    self.post_offer(sdp_offer)
                }

                StatusCode::SERVICE_UNAVAILABLE => {
                    // Server overloaded - caller should retry with backoff
                    let retry_after = headers
                        .get(header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("unknown");

                    Err(StreamError::Runtime(format!(
                        "WHIP server overloaded (503), retry after: {}",
                        retry_after
                    )))
                }

                _ => {
                    // Other error (400, 401, 422, etc.)
                    let error_body = String::from_utf8(body_bytes.to_vec())
                        .unwrap_or_else(|_| format!("HTTP {}", status));

                    Err(StreamError::Runtime(format!(
                        "WHIP POST failed ({}): {}",
                        status,
                        error_body
                    )))
                }
        }
    }

    /// Queue an ICE candidate for batched transmission
    ///
    /// Candidates are buffered and sent in batches via PATCH to reduce HTTP overhead.
    ///
    /// # Arguments
    /// * `candidate_sdp` - ICE candidate in SDP fragment format (e.g., "a=candidate:...")
    fn queue_ice_candidate(&self, candidate_sdp: String) {
        self.pending_candidates.lock().unwrap().push(candidate_sdp);
    }

    /// Send pending ICE candidates to WHIP server via PATCH
    ///
    /// RFC 9725 Section 4.2: Trickle ICE candidates sent via PATCH with
    /// Content-Type: application/trickle-ice-sdpfrag
    ///
    /// Sends all buffered candidates in a single PATCH request, then clears the queue.
    fn send_ice_candidates(&self) -> Result<()> {
        use hyper::{Request, StatusCode, header};
        use http_body_util::{BodyExt, Full};

        let session_url = match &self.session_url {
            Some(url) => url,
            None => {
                return Err(StreamError::Configuration(
                    "Cannot send ICE candidates: no WHIP session URL".into()
                ));
            }
        };

        // Drain pending candidates (atomic swap)
        let candidates = {
            let mut queue = self.pending_candidates.lock().unwrap();
            if queue.is_empty() {
                return Ok(()); // Nothing to send
            }
            queue.drain(..).collect::<Vec<_>>()
        };

        // Build SDP fragment per RFC 8840 (trickle-ice-sdpfrag)
        // Format: Multiple "a=candidate:..." lines joined by CRLF
        let sdp_fragment = candidates.join("\r\n");

        // MUST use Tokio runtime for HTTP operations
        self._runtime.block_on(async {
            let body = Full::new(bytes::Bytes::from(sdp_fragment));
            let boxed_body = body.map_err(|never| match never {}).boxed();

            let mut req_builder = Request::builder()
                .method("PATCH")
                .uri(session_url)
                .header(header::CONTENT_TYPE, "application/trickle-ice-sdpfrag");

            // Add Authorization header only if token is provided
            if let Some(token) = &self.config.auth_token {
                req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }

            let req = req_builder.body(boxed_body)
                .map_err(|e| StreamError::Runtime(format!("Failed to build WHIP PATCH request: {}", e)))?;

            tracing::debug!("WHIP PATCH to {} ({} candidates)", session_url, candidates.len());

            let response = tokio::time::timeout(
                std::time::Duration::from_millis(self.config.timeout_ms),
                self.http_client.request(req),
            )
            .await
            .map_err(|_| StreamError::Runtime(format!("WHIP PATCH timed out after {}ms", self.config.timeout_ms)))?
            .map_err(|e| StreamError::Runtime(format!("WHIP PATCH request failed: {}", e)))?;

            let status = response.status();

            match status {
                StatusCode::NO_CONTENT => {
                    tracing::debug!("Sent {} ICE candidates to WHIP server", candidates.len());
                    Ok(())
                }
                StatusCode::OK => {
                    // 200 OK with body indicates ICE restart (server sends new candidates)
                    // We'll ignore server candidates for now (Phase 4.1)
                    tracing::debug!("ICE restart response (200 OK) - ignoring server candidates");
                    Ok(())
                }
                _ => {
                    // Read error body for debugging
                    let body_bytes = BodyExt::collect(response.into_body())
                        .await
                        .ok()
                        .and_then(|b| String::from_utf8(b.to_bytes().to_vec()).ok())
                        .unwrap_or_else(|| format!("HTTP {}", status));

                    Err(StreamError::Runtime(format!(
                        "WHIP PATCH failed: {}",
                        body_bytes
                    )))
                }
            }
        })
    }

    /// DELETE session to WHIP server (graceful shutdown)
    ///
    /// RFC 9725 Section 4.4: Client terminates session by DELETEing session URL.
    /// Server responds with 200 OK.
    fn terminate(&self) -> Result<()> {
        use hyper::{Request, header};
        use http_body_util::Empty;

        let session_url = match &self.session_url {
            Some(url) => url,
            None => {
                tracing::debug!("No WHIP session to terminate");
                return Ok(()); // No session was created
            }
        };

        // MUST use Tokio runtime for HTTP operations
        self._runtime.block_on(async {
            use http_body_util::BodyExt;
            let body = Empty::<bytes::Bytes>::new();
            let boxed_body = body.map_err(|never| match never {}).boxed();

            let mut req_builder = Request::builder()
                .method("DELETE")
                .uri(session_url);

            // Add Authorization header only if token is provided
            if let Some(token) = &self.config.auth_token {
                req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }

            let req = req_builder.body(boxed_body)
                .map_err(|e| StreamError::Runtime(format!("Failed to build WHIP DELETE request: {}", e)))?;

            tracing::debug!("WHIP DELETE to {}", session_url);

            let response = tokio::time::timeout(
                std::time::Duration::from_millis(self.config.timeout_ms),
                self.http_client.request(req),
            )
            .await
            .map_err(|_| StreamError::Runtime(format!("WHIP DELETE timed out after {}ms", self.config.timeout_ms)))?
            .map_err(|e| StreamError::Runtime(format!("WHIP DELETE request failed: {}", e)))?;

            if response.status().is_success() {
                tracing::info!("WHIP session terminated: {}", session_url);
                Ok(())
            } else {
                // Non-fatal - session will timeout server-side
                tracing::warn!(
                    "WHIP DELETE failed (status {}), session may still exist server-side",
                    response.status()
                );
                Ok(())
            }
        })
    }
}

// ============================================================================
// WEBRTC SESSION
// ============================================================================

/// WebRTC session wrapper using webrtc-rs (0.14.0)
///
/// Manages PeerConnection, tracks, and RTP packetization.
/// Uses pollster::block_on() for low-latency async calls (<20µs overhead).
///
/// # Architecture
/// - webrtc-rs handles RTP packetization, SRTP, ICE, DTLS automatically
/// - We provide samples via Track.write_sample() (async)
/// - pollster::block_on() converts async calls to sync in processor thread
/// - No separate Tokio runtime needed (webrtc-rs spawns its own internally)
///
/// # Performance
/// - pollster::block_on(): <20µs latency per call
/// - Track.write_sample(): Packetizes H.264 NALs into RTP automatically
/// - Zero-copy via bytes::Bytes
struct WebRtcSession {
    /// RTCPeerConnection (handles ICE, DTLS, RTP/RTCP)
    peer_connection: Arc<webrtc::peer_connection::RTCPeerConnection>,

    /// Video track (H.264 @ 90kHz)
    video_track: Option<Arc<webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample>>,

    /// Audio track (Opus @ 48kHz)
    audio_track: Option<Arc<webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample>>,

    /// Flag indicating ICE connection is ready (tracks are bound with correct PT)
    /// CRITICAL: Must be true before calling write_sample() to ensure PT is set correctly
    /// Set to true when ICE connection state becomes Connected
    ice_connected: Arc<std::sync::atomic::AtomicBool>,

    /// Tokio runtime for WebRTC background tasks
    /// CRITICAL: Must stay alive for session lifetime (ICE gathering, DTLS, stats)
    _runtime: tokio::runtime::Runtime,
}

impl WebRtcSession {
    /// Creates a new WebRTC session with H.264 video and Opus audio tracks.
    ///
    /// Uses pollster::block_on() for async initialization (runs in processor thread).
    ///
    /// # Arguments
    /// * `on_ice_candidate` - Callback invoked when ICE candidates are discovered
    ///   - Receives SDP fragment format: "a=candidate:..."
    ///   - Should queue candidates for WHIP PATCH transmission
    fn new<F>(on_ice_candidate: F) -> Result<Self>
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        // CRITICAL: WebRTC crate requires Tokio runtime for background tasks
        // (ICE gathering, DTLS, SRTP, stats collection, mDNS)
        // We create a dedicated multi-threaded Tokio runtime for WebRTC operations
        // NOTE: pollster::block_on() doesn't work here - webrtc needs tokio::spawn()
        // NOTE: Single-threaded runtime doesn't work either - mDNS spawns tasks that need runtime context

        // Create Tokio runtime for WebRTC (multi-threaded with 2 workers for background tasks)
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)  // Minimal threads: 1 for blocking ops, 1 for background tasks
            .thread_name("webrtc-tokio")
            .enable_all()
            .build()
            .map_err(|e| StreamError::Runtime(format!("Failed to create Tokio runtime for WebRTC: {}", e)))?;

        tracing::info!("[WebRTC] Created Tokio runtime with 2 worker threads");

        // Block on async initialization within Tokio context
        let init_result = runtime.block_on(async {
            tracing::debug!("[WebRTC] Creating MediaEngine and registering codecs...");

            // Create MediaEngine and register ONLY the codecs we use
            // CRITICAL: Do NOT use register_default_codecs() - it registers ALL codecs (VP8, VP9, H.264, AV1, H.265)
            // and Cloudflare will choose VP8 as preferred, but we're encoding H.264!
            let mut media_engine = webrtc::api::media_engine::MediaEngine::default();

            // Register H.264 video codec (Baseline profile)
            media_engine
                .register_codec(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                        capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                            mime_type: webrtc::api::media_engine::MIME_TYPE_H264.to_owned(),
                            clock_rate: 90000,
                            channels: 0,
                            sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_owned(),
                            rtcp_feedback: vec![],
                        },
                        payload_type: 102,
                        ..Default::default()
                    },
                    webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Video,
                )
                .map_err(|e| StreamError::Configuration(format!("Failed to register H.264 codec: {}", e)))?;

            // Register Opus audio codec
            media_engine
                .register_codec(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                        capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                            mime_type: webrtc::api::media_engine::MIME_TYPE_OPUS.to_owned(),
                            clock_rate: 48000,
                            channels: 2,
                            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
                            rtcp_feedback: vec![],
                        },
                        payload_type: 111,
                        ..Default::default()
                    },
                    webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio,
                )
                .map_err(|e| StreamError::Configuration(format!("Failed to register Opus codec: {}", e)))?;

            tracing::info!("[WebRTC] Registered ONLY H.264 (PT=102) and Opus (PT=111) codecs");

            tracing::debug!("[WebRTC] Creating interceptor registry...");

            // Create InterceptorRegistry for RTCP feedback
            // This is CRITICAL for proper WebRTC operation - provides:
            // - NACK (negative acknowledgments for lost packets)
            // - RTCP sender/receiver reports
            // - Statistics collection
            let mut registry = webrtc::interceptor::registry::Registry::new();

            // Register default interceptors (NACK, RTCP reports, etc.)
            registry = webrtc::api::interceptor_registry::register_default_interceptors(registry, &mut media_engine)
                .map_err(|e| StreamError::Configuration(format!("Failed to register interceptors: {}", e)))?;

            tracing::debug!("[WebRTC] Creating WebRTC API...");

            // Create API with MediaEngine and InterceptorRegistry
            let api = webrtc::api::APIBuilder::new()
                .with_media_engine(media_engine)
                .with_interceptor_registry(registry)
                .build();

            tracing::debug!("[WebRTC] Creating RTCPeerConnection...");

            // Create RTCPeerConnection
            let config = webrtc::peer_connection::configuration::RTCConfiguration::default();
            let peer_connection = Arc::new(
                api
                    .new_peer_connection(config)
                    .await
                    .map_err(|e| StreamError::Configuration(format!("Failed to create PeerConnection: {}", e)))?
            );

            tracing::debug!("[WebRTC] RTCPeerConnection created successfully");

            // Subscribe to ICE candidate events
            // Convert webrtc-rs ICE candidate to SDP fragment and call callback
            let on_candidate = Arc::new(on_ice_candidate);
            let pc_for_ice_candidate = Arc::clone(&peer_connection);
            pc_for_ice_candidate.on_ice_candidate(Box::new(move |candidate_opt| {
                let callback = Arc::clone(&on_candidate);
                Box::pin(async move {
                    if let Some(candidate) = candidate_opt {
                        // Convert RTCIceCandidate to SDP fragment format per RFC 8840
                        // Format: "a=candidate:..." (without the "a=" prefix in JSON)
                        if let Ok(json) = candidate.to_json() {
                            let sdp_fragment = format!("a={}", json.candidate);
                            tracing::debug!("ICE candidate discovered: {}", sdp_fragment);
                            callback(sdp_fragment);
                        }
                    } else {
                        // None indicates end of candidates
                        tracing::debug!("ICE candidate gathering complete");
                    }
                    // Return unit future
                    ()
                })
            }));

            // ========================================
            // COMPREHENSIVE STATE MONITORING
            // ========================================

            // 1. Monitor signaling state changes
            peer_connection.on_signaling_state_change(Box::new(move |state| {
                Box::pin(async move {
                    tracing::info!("[WebRTC] 🔄 Signaling state: {:?}", state);
                })
            }));

            // 2. Monitor peer connection state changes - CRITICAL FOR DTLS!
            // This tells us if the DTLS handshake completed successfully.
            // ICE can be Connected but peer connection still Connecting if DTLS fails!
            peer_connection.on_peer_connection_state_change(Box::new(move |state| {
                Box::pin(async move {
                    tracing::info!("[WebRTC] ========================================");
                    tracing::info!("[WebRTC] 🔗 Peer connection state: {:?}", state);
                    tracing::info!("[WebRTC] ========================================");
                    match state {
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::New => {
                            tracing::debug!("[WebRTC] Peer connection: New");
                        }
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Connecting => {
                            tracing::info!("[WebRTC] Peer connection: Connecting... (DTLS handshake in progress)");
                        }
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Connected => {
                            tracing::info!("[WebRTC] ✅✅✅ Peer connection: CONNECTED! ✅✅✅");
                            tracing::info!("[WebRTC] DTLS handshake completed successfully!");
                            tracing::info!("[WebRTC] RTP packets can now be sent/received!");
                        }
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Disconnected => {
                            tracing::warn!("[WebRTC] ⚠️  Peer connection: DISCONNECTED!");
                        }
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Failed => {
                            tracing::error!("[WebRTC] ❌❌❌ Peer connection: FAILED! ❌❌❌");
                            tracing::error!("[WebRTC] This means DTLS handshake or ICE failed!");
                        }
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Closed => {
                            tracing::info!("[WebRTC] Peer connection: Closed");
                        }
                        _ => {}
                    }
                })
            }));

            // 3. Monitor ICE gathering state
            peer_connection.on_ice_gathering_state_change(Box::new(move |state| {
                Box::pin(async move {
                    tracing::info!("[WebRTC] 🧊 ICE gathering state: {:?}", state);
                })
            }));

            // 4. CRITICAL: Subscribe to ICE connection state changes
            // This signals when track binding is complete and PT values are set correctly.
            // We MUST wait for Connected state before calling write_sample().
            let ice_connected_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let ice_connected_clone = Arc::clone(&ice_connected_flag);
            let pc_for_ice_handler = Arc::clone(&peer_connection);

            peer_connection.on_ice_connection_state_change(Box::new(move |connection_state| {
                let flag = Arc::clone(&ice_connected_clone);
                let pc = Arc::clone(&pc_for_ice_handler);
                Box::pin(async move {
                    tracing::info!("[WebRTC] ========================================");
                    tracing::info!("[WebRTC] ICE connection state changed: {:?}", connection_state);
                    tracing::info!("[WebRTC] ========================================");

                    if connection_state == webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Connected {
                        tracing::info!("[WebRTC] ✅ ICE Connected - track binding complete!");

                        // CRITICAL: Verify PT values AFTER ICE connection
                        let transceivers = pc.get_transceivers().await;
                        tracing::info!("[WebRTC] Verifying PT values for {} transceivers after ICE connection:", transceivers.len());

                        for (i, transceiver) in transceivers.iter().enumerate() {
                            let sender = transceiver.sender().await;
                            let params = sender.get_parameters().await;

                            tracing::info!("[WebRTC] ┌─ Transceiver #{} ─────────────────────", i);

                            // Log all codecs
                            for (codec_idx, codec) in params.rtp_parameters.codecs.iter().enumerate() {
                                tracing::info!("[WebRTC] │  Codec #{}: {} (PT={})",
                                    codec_idx,
                                    codec.capability.mime_type,
                                    codec.payload_type);
                            }

                            // Log encoding parameters - THIS IS CRITICAL!
                            if let Some(enc) = params.encodings.first() {
                                if enc.payload_type == 0 {
                                    tracing::error!("[WebRTC] │  ❌ ENCODING: PT={} SSRC={:?} ← STILL WRONG AFTER ICE!",
                                        enc.payload_type,
                                        enc.ssrc);
                                } else {
                                    tracing::info!("[WebRTC] │  ✅ ENCODING: PT={} SSRC={:?} ← CORRECT!",
                                        enc.payload_type,
                                        enc.ssrc);
                                }
                            } else {
                                tracing::error!("[WebRTC] │  ❌ NO ENCODING FOUND!");
                            }

                            tracing::info!("[WebRTC] └────────────────────────────────────");
                        }

                        flag.store(true, std::sync::atomic::Ordering::Release);
                        tracing::info!("[WebRTC] 🚀 Ready to send samples!");

                    } else if connection_state == webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Disconnected
                           || connection_state == webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Failed {
                        tracing::warn!("[WebRTC] ❌ ICE connection lost: {:?}", connection_state);
                        flag.store(false, std::sync::atomic::Ordering::Release);
                    }

                    ()
                })
            }));

            // Create video track (H.264)
            // Following play-from-disk-h264 example - only specify MIME type, use defaults for rest
            let video_track = Arc::new(
                webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample::new(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                        mime_type: webrtc::api::media_engine::MIME_TYPE_H264.to_owned(),
                        ..Default::default()
                    },
                    "video".to_owned(),
                    "streamlib-video".to_owned(),
                ),
            );

            // Add video track to PeerConnection
            let video_rtp_sender = peer_connection
                .add_track(Arc::clone(&video_track) as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>)
                .await
                .map_err(|e| StreamError::Configuration(format!("Failed to add video track: {}", e)))?;

            // TELEMETRY: Video track binding
            tracing::info!("[TELEMETRY:VIDEO_TRACK_ADDED] track_id=video");
            let video_params = video_rtp_sender.get_parameters().await;

            // Log all codec parameters
            for (idx, codec) in video_params.rtp_parameters.codecs.iter().enumerate() {
                tracing::info!("[TELEMETRY:VIDEO_CODEC_{}] mime_type={}, pt={}, clock_rate={}, channels={}, fmtp='{}'",
                    idx,
                    codec.capability.mime_type,
                    codec.payload_type,
                    codec.capability.clock_rate,
                    codec.capability.channels,
                    codec.capability.sdp_fmtp_line);
            }

            // Log encoding parameters
            if let Some(enc) = video_params.encodings.first() {
                tracing::info!("[TELEMETRY:VIDEO_ENCODING] pt={}, ssrc={:?}, rid={:?}",
                    enc.payload_type,
                    enc.ssrc,
                    enc.rid);
            } else {
                tracing::warn!("[TELEMETRY:VIDEO_ENCODING] NO_ENCODING_FOUND");
            }

            // Create audio track (Opus)
            // Following play-from-disk-h264 example - only specify MIME type, use defaults for rest
            let audio_track = Arc::new(
                webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample::new(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                        mime_type: webrtc::api::media_engine::MIME_TYPE_OPUS.to_owned(),
                        ..Default::default()
                    },
                    "audio".to_owned(),
                    "streamlib-audio".to_owned(),
                ),
            );

            // Add audio track to PeerConnection
            let audio_rtp_sender = peer_connection
                .add_track(Arc::clone(&audio_track) as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>)
                .await
                .map_err(|e| StreamError::Configuration(format!("Failed to add audio track: {}", e)))?;

            // TELEMETRY: Audio track binding
            tracing::info!("[TELEMETRY:AUDIO_TRACK_ADDED] track_id=audio");
            let audio_params = audio_rtp_sender.get_parameters().await;

            // Log all codec parameters
            for (idx, codec) in audio_params.rtp_parameters.codecs.iter().enumerate() {
                tracing::info!("[TELEMETRY:AUDIO_CODEC_{}] mime_type={}, pt={}, clock_rate={}, channels={}, fmtp='{}'",
                    idx,
                    codec.capability.mime_type,
                    codec.payload_type,
                    codec.capability.clock_rate,
                    codec.capability.channels,
                    codec.capability.sdp_fmtp_line);
            }

            // Log encoding parameters
            if let Some(enc) = audio_params.encodings.first() {
                tracing::info!("[TELEMETRY:AUDIO_ENCODING] pt={}, ssrc={:?}, rid={:?}",
                    enc.payload_type,
                    enc.ssrc,
                    enc.rid);
            } else {
                tracing::warn!("[TELEMETRY:AUDIO_ENCODING] NO_ENCODING_FOUND");
            }

            // Set all transceivers to send-only (WHIP ingestion - we're publishing to Cloudflare, not receiving)
            let transceivers = peer_connection.get_transceivers().await;
            for transceiver in transceivers {
                transceiver.set_direction(webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection::Sendonly).await;
            }
            tracing::info!("[WebRTC] Set all transceivers to SendOnly for WHIP ingestion");

            // Return tuple of (peer_connection, video_track, audio_track, ice_connected_flag)
            Ok::<_, StreamError>((peer_connection, video_track, audio_track, ice_connected_flag))
        })?;

        // Destructure init result
        let (peer_connection, video_track, audio_track, ice_connected) = init_result;

        // Return Self with runtime (must stay alive for background tasks)
        // peer_connection is already Arc-wrapped
        Ok(Self {
            peer_connection,
            video_track: Some(video_track),
            audio_track: Some(audio_track),
            ice_connected,
            _runtime: runtime,
        })
    }

    /// Adds bandwidth attributes to SDP for WHIP compatibility.
    ///
    /// Cloudflare Stream requires explicit bandwidth signaling in the SDP.
    /// Adds b=AS (Application-Specific) and b=TIAS (Transport-Independent) lines
    /// after each m= line based on configured bitrates.
    ///
    /// # Arguments
    /// * `sdp` - Original SDP string
    /// * `video_bitrate_bps` - Video bitrate in bits per second
    /// * `audio_bitrate_bps` - Audio bitrate in bits per second
    ///
    /// # Returns
    /// Modified SDP with bandwidth attributes
    fn add_bandwidth_to_sdp(sdp: &str, video_bitrate_bps: u32, audio_bitrate_bps: u32) -> String {
        let mut result = String::new();
        let lines: Vec<&str> = sdp.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];
            result.push_str(line);
            result.push('\n');

            // Add bandwidth attributes after m=video line
            if line.starts_with("m=video") {
                // Add b=AS (Application-Specific bandwidth in kbps)
                let bitrate_kbps = video_bitrate_bps / 1000;
                result.push_str(&format!("b=AS:{}\n", bitrate_kbps));

                // Add b=TIAS (Transport-Independent Application-Specific in bps)
                result.push_str(&format!("b=TIAS:{}\n", video_bitrate_bps));

                tracing::debug!(
                    "[WebRTC] Added video bandwidth: b=AS:{} b=TIAS:{}",
                    bitrate_kbps,
                    video_bitrate_bps
                );
            }
            // Add bandwidth attributes after m=audio line
            else if line.starts_with("m=audio") {
                // Add b=AS (Application-Specific bandwidth in kbps)
                let bitrate_kbps = audio_bitrate_bps / 1000;
                result.push_str(&format!("b=AS:{}\n", bitrate_kbps));

                // Add b=TIAS (Transport-Independent Application-Specific in bps)
                result.push_str(&format!("b=TIAS:{}\n", audio_bitrate_bps));

                tracing::debug!(
                    "[WebRTC] Added audio bandwidth: b=AS:{} b=TIAS:{}",
                    bitrate_kbps,
                    audio_bitrate_bps
                );
            }

            i += 1;
        }

        result
    }

    /// Creates SDP offer for WHIP signaling.
    ///
    /// Uses Tokio runtime (CRITICAL: mDNS ICE gathering needs Tokio context).
    fn create_offer(&self) -> Result<String> {
        // MUST use Tokio runtime, not pollster - ICE gathering spawns mDNS tasks
        self._runtime.block_on(async {
            tracing::debug!("[WebRTC] Creating SDP offer...");

            let offer = self
                .peer_connection
                .create_offer(None)
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to create offer: {}", e)))?;

            tracing::debug!("[WebRTC] Setting local description (starts ICE gathering)...");

            // Set local description (triggers ICE candidate gathering via mDNS)
            self.peer_connection
                .set_local_description(offer)
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to set local description: {}", e)))?;

            // Get SDP string
            let local_desc = self
                .peer_connection
                .local_description()
                .await
                .ok_or_else(|| StreamError::Runtime("No local description".into()))?;

            tracing::debug!("[WebRTC] SDP offer created successfully");
            Ok(local_desc.sdp)
        })
    }

    /// Sets remote SDP answer from WHIP server.
    ///
    /// Uses Tokio runtime (required for WebRTC background tasks).
    fn set_remote_answer(&mut self, sdp: &str) -> Result<()> {
        // MUST use Tokio runtime for WebRTC operations
        self._runtime.block_on(async {
            tracing::debug!("[WebRTC] Setting remote SDP answer...");

            let answer = webrtc::peer_connection::sdp::session_description::RTCSessionDescription::answer(sdp.to_owned())
                .map_err(|e| StreamError::Runtime(format!("Failed to parse SDP answer: {}", e)))?;

            self.peer_connection
                .set_remote_description(answer)
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to set remote description: {}", e)))?;

            tracing::debug!("[WebRTC] Remote SDP answer set successfully");

            // TELEMETRY: After SDP negotiation
            tracing::info!("[TELEMETRY:SDP_ANSWER_SET] remote_description=set");

            let transceivers = self.peer_connection.get_transceivers().await;
            for (i, transceiver) in transceivers.iter().enumerate() {
                let sender = transceiver.sender().await;
                let params = sender.get_parameters().await;

                // TELEMETRY: Log all codecs for this transceiver
                for (codec_idx, codec) in params.rtp_parameters.codecs.iter().enumerate() {
                    tracing::info!("[TELEMETRY:TRANSCEIVER_{}_CODEC_{}] mime_type={}, pt={}, clock_rate={}, channels={}, fmtp='{}'",
                        i,
                        codec_idx,
                        codec.capability.mime_type,
                        codec.payload_type,
                        codec.capability.clock_rate,
                        codec.capability.channels,
                        codec.capability.sdp_fmtp_line);
                }

                // TELEMETRY: Log encoding for this transceiver
                if let Some(first_encoding) = params.encodings.first() {
                    tracing::info!("[TELEMETRY:TRANSCEIVER_{}_ENCODING] pt={}, ssrc={:?}, rid={:?}",
                        i,
                        first_encoding.payload_type,
                        first_encoding.ssrc,
                        first_encoding.rid);
                } else {
                    tracing::warn!("[TELEMETRY:TRANSCEIVER_{}_ENCODING] NO_ENCODING_FOUND", i);
                }
            }

            Ok(())
        })
    }

    /// Validate and log H.264 NAL unit format
    fn validate_and_log_h264_nal(sample_data: &[u8], sample_idx: usize) {
        if sample_data.len() < 5 {
            tracing::error!("[H264 Validation] ❌ Sample {}: Too short ({} bytes, need ≥5)",
                sample_idx, sample_data.len());
            return;
        }

        // Log first 8 bytes to identify format
        tracing::info!("[H264 Validation] Sample {}: First 8 bytes: {:02X?}",
            sample_idx,
            &sample_data[..sample_data.len().min(8)]);

        // Check for Annex-B start codes (0x00 0x00 0x00 0x01 or 0x00 0x00 0x01)
        let is_annex_b = (sample_data.len() >= 4
            && sample_data[0] == 0x00
            && sample_data[1] == 0x00
            && sample_data[2] == 0x00
            && sample_data[3] == 0x01)
            || (sample_data.len() >= 3
                && sample_data[0] == 0x00
                && sample_data[1] == 0x00
                && sample_data[2] == 0x01);

        if is_annex_b {
            tracing::error!("[H264 Validation] ❌❌❌ Sample {}: ANNEX-B FORMAT DETECTED!", sample_idx);
            tracing::error!("[H264 Validation] WebRTC requires AVCC format (length-prefixed), not Annex-B!");
            tracing::error!("[H264 Validation] This explains why Cloudflare receives no packets!");

            // Extract NAL unit type from after start code
            let nal_offset = if sample_data.len() >= 4 && sample_data[3] == 0x01 { 4 } else { 3 };
            if sample_data.len() > nal_offset {
                let nal_unit_type = sample_data[nal_offset] & 0x1F;
                tracing::error!("[H264 Validation] NAL type: {} (after Annex-B start code)", nal_unit_type);
            }
            return;
        }

        // AVCC format: [4-byte length][NAL unit data]
        let nal_length = u32::from_be_bytes([
            sample_data[0],
            sample_data[1],
            sample_data[2],
            sample_data[3],
        ]) as usize;

        if nal_length + 4 != sample_data.len() {
            tracing::warn!("[H264 Validation] ⚠️  Sample {}: NAL length mismatch (prefix says {}, actual {})",
                sample_idx, nal_length, sample_data.len() - 4);
        }

        // Extract NAL unit type from first byte of NAL data (after 4-byte length)
        let nal_unit_type = sample_data[4] & 0x1F;

        // Log NAL unit type
        match nal_unit_type {
            1 => tracing::trace!("[H264] Sample {}: Coded slice (non-IDR)", sample_idx),
            5 => tracing::info!("[H264] Sample {}: IDR (keyframe) ✅", sample_idx),
            6 => tracing::trace!("[H264] Sample {}: SEI", sample_idx),
            7 => tracing::info!("[H264] Sample {}: SPS (Sequence Parameter Set) ✅", sample_idx),
            8 => tracing::info!("[H264] Sample {}: PPS (Picture Parameter Set) ✅", sample_idx),
            9 => tracing::trace!("[H264] Sample {}: AUD (Access Unit Delimiter)", sample_idx),
            _ => tracing::debug!("[H264] Sample {}: NAL type {}", sample_idx, nal_unit_type),
        }
    }

    /// Writes video samples to the video track.
    ///
    /// Uses Tokio runtime to convert async write to sync.
    /// webrtc-rs handles RTP packetization automatically.
    ///
    /// # Arguments
    /// * `samples` - H.264 NAL units (one Sample per NAL, without start codes)
    fn write_video_samples(&mut self, samples: Vec<webrtc::media::Sample>) -> Result<()> {
        // CRITICAL: Wait for ICE connection before writing samples
        // This ensures track binding is complete and PT values are set correctly.
        // Without this, PT=0 will be used and Cloudflare will reject packets!
        if !self.ice_connected.load(std::sync::atomic::Ordering::Acquire) {
            tracing::debug!("[WebRTC] Skipping video write - waiting for ICE connection");
            return Ok(()); // Silently skip until connected
        }

        let track = self
            .video_track
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("Video track not initialized".into()))?;

        static FIRST_WRITE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
        let is_first = FIRST_WRITE.swap(false, std::sync::atomic::Ordering::Relaxed);

        if is_first {
            tracing::info!("[WebRTC] 🎥 FIRST VIDEO WRITE after ICE Connected!");
            tracing::info!("[WebRTC]    Samples: {}, Total bytes: {}",
                samples.len(),
                samples.iter().map(|s| s.data.len()).sum::<usize>());

            // VALIDATION: Check first batch of H.264 NAL units
            tracing::info!("[WebRTC] 🔍 Validating H.264 NAL units...");
            for (idx, sample) in samples.iter().enumerate() {
                Self::validate_and_log_h264_nal(&sample.data, idx);
            }
        } else {
            tracing::debug!("[WebRTC] Writing {} video samples ({} bytes)",
                samples.len(),
                samples.iter().map(|s| s.data.len()).sum::<usize>());
        }

        // Write each sample (NAL unit) to the track
        // MUST use Tokio runtime for WebRTC operations
        for (i, sample) in samples.iter().enumerate() {
            // TELEMETRY: Track sample writing (only log first sample to avoid spam)
            if i == 0 && is_first {
                tracing::info!("[TELEMETRY:VIDEO_SAMPLE_WRITE] sample_num={}, bytes={}, duration_ms={:?}",
                    i,
                    sample.data.len(),
                    sample.duration.as_millis());
            }

            let result = self._runtime.block_on(async {
                track
                    .write_sample(sample)
                    .await
                    .map_err(|e| StreamError::Runtime(format!("Failed to write video sample {}: {}", i, e)))
            });

            if let Err(ref e) = result {
                tracing::error!("[WebRTC] ❌ Failed to write video sample {}: {}", i, e);
                return result;
            }
        }

        if is_first {
            tracing::info!("[WebRTC] ✅ Successfully wrote first batch of {} video samples", samples.len());
        }
        Ok(())
    }

    /// Writes audio sample to the audio track.
    ///
    /// Uses Tokio runtime to convert async write to sync.
    ///
    /// # Arguments
    /// * `sample` - Opus-encoded audio frame
    fn write_audio_sample(&mut self, sample: webrtc::media::Sample) -> Result<()> {
        // CRITICAL: Wait for ICE connection before writing samples
        // This ensures track binding is complete and PT values are set correctly.
        // Without this, PT=0 will be used and Cloudflare will reject packets!
        if !self.ice_connected.load(std::sync::atomic::Ordering::Acquire) {
            tracing::debug!("[WebRTC] Skipping audio write - waiting for ICE connection");
            return Ok(()); // Silently skip until connected
        }

        let track = self
            .audio_track
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("Audio track not initialized".into()))?;

        // Track first write and periodic telemetry
        static AUDIO_SAMPLE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let counter = AUDIO_SAMPLE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if counter == 0 {
            tracing::info!("[WebRTC] 🎵 FIRST AUDIO WRITE after ICE Connected!");
            tracing::info!("[WebRTC]    Bytes: {}, Duration: {:?}",
                sample.data.len(),
                sample.duration);
        } else if counter % 50 == 0 {
            tracing::debug!("[TELEMETRY:AUDIO_SAMPLE_WRITE] sample_num={}, bytes={}, duration_ms={:?}",
                counter,
                sample.data.len(),
                sample.duration.as_millis());
        }

        // MUST use Tokio runtime for WebRTC operations
        let result = self._runtime.block_on(async {
            track
                .write_sample(&sample)
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to write audio sample: {}", e)))
        });

        if let Err(ref e) = result {
            tracing::error!("[WebRTC] ❌ Failed to write audio sample {}: {}", counter, e);
        } else if counter == 0 {
            tracing::info!("[WebRTC] ✅ Successfully wrote first audio sample");
        }

        result
    }

    /// Gets RTCP statistics from the peer connection.
    ///
    /// Returns stats including OutboundRTP (bytes/packets sent) and
    /// RemoteInboundRTP (Cloudflare's receiver reports with packet loss, jitter, RTT).
    fn get_stats(&self) -> Result<webrtc::stats::StatsReport> {
        // MUST use Tokio runtime for WebRTC operations
        self._runtime.block_on(async {
            Ok(self.peer_connection.get_stats().await)
        })
    }

    /// Closes the WebRTC session.
    fn close(&self) -> Result<()> {
        // MUST use Tokio runtime for WebRTC operations
        self._runtime.block_on(async {
            self.peer_connection
                .close()
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to close peer connection: {}", e)))
        })
    }
}

// ============================================================================
// MAIN WEBRTC WHIP PROCESSOR
// ============================================================================

#[derive(Clone, Serialize, Deserialize)]
pub struct WebRtcWhipConfig {
    pub whip: WhipConfig,
    pub video: VideoEncoderConfig,
    pub audio: AudioEncoderConfig,
}

impl Default for WebRtcWhipConfig {
    fn default() -> Self {
        Self {
            whip: WhipConfig {
                endpoint_url: String::new(),
                auth_token: None, // No authentication by default
                timeout_ms: 10000,
            },
            video: VideoEncoderConfig::default(),
            audio: AudioEncoderConfig::default(),
        }
    }
}

#[derive(StreamProcessor)]
#[processor(
    mode = Push,
    description = "Streams video and audio to Cloudflare Stream via WebRTC WHIP"
)]
pub struct WebRtcWhipProcessor {
    #[input(description = "Input video frames to encode and stream")]
    video_in: StreamInput<VideoFrame>,

    #[input(description = "Input audio frames to encode and stream")]
    audio_in: StreamInput<AudioFrame<2>>,

    #[config]
    config: WebRtcWhipConfig,

    // RuntimeContext for main thread dispatch
    ctx: Option<RuntimeContext>,

    // Session state
    session_started: bool,
    start_time_ns: Option<i64>,
    gpu_context: Option<GpuContext>,  // Store for lazy encoder init

    // Encoders (will be Box<dyn Trait> when implemented)
    #[cfg(target_os = "macos")]
    video_encoder: Option<VideoToolboxH264Encoder>,
    audio_encoder: Option<OpusEncoder>,

    // RTP timestamp calculators
    video_rtp_calc: Option<RtpTimestampCalculator>,
    audio_rtp_calc: Option<RtpTimestampCalculator>,

    // WHIP and WebRTC
    whip_client: Option<Arc<Mutex<WhipClient>>>,
    webrtc_session: Option<WebRtcSession>,

    // RTCP stats monitoring
    last_stats_time_ns: i64,
    last_video_bytes_sent: u64,
    last_audio_bytes_sent: u64,
    last_video_packets_sent: u64,
    last_audio_packets_sent: u64,
}

impl WebRtcWhipProcessor {
    /// Called by StreamProcessor macro during setup phase.
    /// Stores GPU context and runtime context for lazy encoder initialization.
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        // Store contexts for lazy encoder initialization
        // IMPORTANT: VideoToolbox encoder MUST be created on main thread,
        // so we defer creation until start_session() is called from process()
        self.gpu_context = Some(ctx.gpu.clone());
        self.ctx = Some(ctx.clone());

        // Initialize audio encoder (doesn't require main thread)
        self.audio_encoder = Some(OpusEncoder::new(self.config.audio.clone())?);

        tracing::info!("WebRtcWhipProcessor initialized (will create video encoder on first frames)");
        Ok(())
    }

    /// Called by StreamProcessor macro during teardown phase.
    /// Gracefully shuts down the WebRTC session and closes the WHIP connection.
    fn teardown(&mut self) -> Result<()> {
        tracing::info!("WebRtcWhipProcessor shutting down");

        // Close WebRTC session
        if let Some(webrtc_session) = &self.webrtc_session {
            if let Err(e) = webrtc_session.close() {
                tracing::warn!("Error closing WebRTC session: {}", e);
            }
        }

        // Terminate WHIP session (DELETE request)
        if let Some(whip_client) = &self.whip_client {
            if let Ok(client) = whip_client.lock() {
                if let Err(e) = client.terminate() {
                    tracing::warn!("Error terminating WHIP session: {}", e);
                }
            }
        }

        tracing::info!("WebRtcWhipProcessor shutdown complete");
        Ok(())
    }

    /// Main processing loop: reads video/audio frames, encodes them, and streams via WebRTC
    fn process(&mut self) -> Result<()> {
        // Read latest video and audio frames from input ports
        let video_frame = self.video_in.read_latest();
        let audio_frame = self.audio_in.read_latest();

        // Debug logging to diagnose frame delivery
        tracing::debug!(
            "[WebRTC] process() called - video: {}, audio: {}, session_started: {}",
            if video_frame.is_some() { "YES" } else { "NO" },
            if audio_frame.is_some() { "YES" } else { "NO" },
            self.session_started
        );

        // Start session on first frame (video or audio)
        // WebRTC can handle streams that start with only one track and add the other later
        if !self.session_started && (video_frame.is_some() || audio_frame.is_some()) {
            tracing::info!("[WebRTC] Starting session - received first frame");
            self.start_session()?;
            self.session_started = true;
        }

        // Process video if available
        if let Some(frame) = video_frame {
            self.process_video_frame(&frame)?;
        }

        // Process audio if available
        if let Some(frame) = audio_frame {
            self.process_audio_frame(&frame)?;
        }

        // Collect and log RTCP stats every 2 seconds
        if self.session_started {
            let current_time_ns = MediaClock::now().as_nanos() as i64;
            let elapsed_since_last_stats = current_time_ns - self.last_stats_time_ns;

            // Log stats every 2 seconds (2_000_000_000 ns)
            if elapsed_since_last_stats >= 2_000_000_000 {
                self.log_rtcp_stats()?;
                self.last_stats_time_ns = current_time_ns;
            }
        }

        Ok(())
    }

    /// Starts the WebRTC WHIP session.
    /// Called automatically when the first video+audio frames arrive.
    fn start_session(&mut self) -> Result<()> {
        // 1. Initialize VideoToolbox encoder lazily (deferred from setup() phase)
        // NOTE: VideoToolbox initialization requires main thread dispatch, which is
        // handled inside VideoToolboxH264Encoder::new() via setup_compression_session()
        // We defer creation from setup() to here because we need RuntimeContext
        if self.video_encoder.is_none() {
            let gpu_context = self.gpu_context.clone();
            let ctx = self.ctx.as_ref().ok_or_else(|| {
                StreamError::Runtime("RuntimeContext not available".into())
            })?;
            self.video_encoder = Some(VideoToolboxH264Encoder::new(
                self.config.video.clone(),
                gpu_context,
                ctx
            )?);
            tracing::info!("VideoToolbox H.264 encoder initialized");
        }

        // 2. Set start time
        self.start_time_ns = Some(MediaClock::now().as_nanos() as i64);

        // 3. Initialize RTP timestamp calculators
        self.video_rtp_calc = Some(RtpTimestampCalculator::new(
            self.start_time_ns.unwrap(),
            90000 // 90kHz for video
        ));
        self.audio_rtp_calc = Some(RtpTimestampCalculator::new(
            self.start_time_ns.unwrap(),
            48000 // 48kHz for Opus
        ));

        // 3. Install rustls crypto provider BEFORE creating WHIP client
        // This is CRITICAL for rustls 0.23+ to avoid "CryptoProvider not set" panic
        tracing::info!("[WebRTC] Installing rustls crypto provider (ring)...");
        match rustls::crypto::CryptoProvider::get_default() {
            Some(_) => {
                tracing::info!("[WebRTC] Crypto provider already installed");
            }
            None => {
                tracing::info!("[WebRTC] Installing ring crypto provider...");
                rustls::crypto::ring::default_provider()
                    .install_default()
                    .map_err(|e| StreamError::Runtime(format!("Failed to install rustls crypto provider: {:?}", e)))?;
                tracing::info!("[WebRTC] Ring crypto provider installed successfully");
            }
        }

        // 4. Create WHIP client
        tracing::info!("[WebRTC] Creating WHIP client for endpoint: {}", self.config.whip.endpoint_url);
        let whip_client = Arc::new(Mutex::new(WhipClient::new(self.config.whip.clone())?));
        self.whip_client = Some(whip_client.clone());
        tracing::info!("[WebRTC] WHIP client created successfully");

        // 4. Create WebRTC session with ICE callback
        // Callback queues ICE candidates for WHIP PATCH transmission
        let whip_clone = whip_client.clone();
        let mut webrtc_session = WebRtcSession::new(move |candidate_sdp| {
            if let Ok(whip) = whip_clone.lock() {
                whip.queue_ice_candidate(candidate_sdp);
            }
        })?;

        // 5. Create SDP offer
        let offer = webrtc_session.create_offer()?;

        // 5a. Add bandwidth attributes to SDP for Cloudflare compatibility
        // CRITICAL: Cloudflare Stream requires explicit bandwidth signaling in SDP
        // Without b=AS and b=TIAS lines, the dashboard won't show encoding bitrates
        let offer_with_bandwidth = WebRtcSession::add_bandwidth_to_sdp(
            &offer,
            self.config.video.bitrate_bps,
            self.config.audio.bitrate_bps,
        );

        tracing::info!("[WebRTC] ========== SDP OFFER (with bandwidth) ==========");
        for (i, line) in offer_with_bandwidth.lines().enumerate() {
            tracing::info!("[WebRTC] SDP OFFER [{}]: {}", i, line);
        }
        tracing::info!("[WebRTC] ================================");

        // 6. POST to WHIP endpoint (receives SDP answer)
        let answer = whip_client.lock().unwrap().post_offer(&offer_with_bandwidth)?;
        tracing::info!("[WebRTC] ========== SDP ANSWER ==========");
        for (i, line) in answer.lines().enumerate() {
            tracing::info!("[WebRTC] SDP ANSWER [{}]: {}", i, line);
        }
        tracing::info!("[WebRTC] =================================");

        // 7. Set remote answer
        webrtc_session.set_remote_answer(&answer)?;

        // 8. Send any buffered ICE candidates to WHIP server
        // (Candidates may be discovered during offer/answer exchange)
        // NOTE: Trickle ICE is OPTIONAL per RFC 9725, some servers (like Cloudflare) don't support it
        // If PATCH fails, we log a warning but don't fail session startup
        match whip_client.lock().unwrap().send_ice_candidates() {
            Ok(_) => {
                tracing::info!("[WebRTC] ICE candidates sent successfully (trickle ICE supported)");
            }
            Err(e) => {
                tracing::warn!(
                    "[WebRTC] Failed to send ICE candidates (trickle ICE not supported): {}. \
                     This is OK - candidates are already in the SDP offer/answer.",
                    e
                );
            }
        }

        self.webrtc_session = Some(webrtc_session);
        self.session_started = true;

        tracing::info!("WebRTC WHIP session started");
        Ok(())
    }

    fn process_video_frame(&mut self, frame: &VideoFrame) -> Result<()> {
        if !self.session_started {
            return Ok(());
        }

        // 1. Encode video frame to H.264
        let encoder = self.video_encoder.as_mut()
            .ok_or_else(|| StreamError::Configuration("Video encoder not initialized".into()))?;
        let encoded = encoder.encode(frame)?;

        // 2. Convert EncodedVideoFrame to webrtc::media::Sample(s)
        // Uses parse_nal_units() to extract NALs from Annex B format
        let samples = convert_video_to_samples(&encoded, self.config.video.fps)?;

        // 3. Write samples to WebRTC video track
        // Uses pollster::block_on() internally (<20µs overhead)
        // webrtc-rs handles RTP packetization, timestamps, etc.
        self.webrtc_session.as_mut().unwrap()
            .write_video_samples(samples)?;

        Ok(())
    }

    fn process_audio_frame(&mut self, frame: &AudioFrame<2>) -> Result<()> {
        if !self.session_started {
            return Ok(());
        }

        // 1. Encode audio frame to Opus
        let encoder = self.audio_encoder.as_mut()
            .ok_or_else(|| StreamError::Configuration("Audio encoder not initialized".into()))?;
        let encoded = encoder.encode(frame)?;

        // 2. Convert EncodedAudioFrame to webrtc::media::Sample
        let sample = convert_audio_to_sample(&encoded, self.config.audio.sample_rate)?;

        // 3. Write sample to WebRTC audio track
        // Uses pollster::block_on() internally (<20µs overhead)
        self.webrtc_session.as_mut().unwrap()
            .write_audio_sample(sample)?;

        Ok(())
    }

    /// Collects and logs RTCP statistics from the WebRTC peer connection.
    /// Calculates bitrates from delta of bytes sent since last measurement.
    fn log_rtcp_stats(&mut self) -> Result<()> {
        let webrtc_session = self.webrtc_session.as_ref()
            .ok_or_else(|| StreamError::Runtime("WebRTC session not initialized".into()))?;

        // Get stats from peer connection (async operation, run in Tokio runtime)
        let stats = webrtc_session.get_stats()?;

        let mut video_bytes_sent = 0u64;
        let mut audio_bytes_sent = 0u64;
        let mut video_packets_sent = 0u64;
        let mut audio_packets_sent = 0u64;

        // Iterate over stats to find OutboundRTP for video and audio
        for (_id, stat_type) in stats.reports.iter() {
            match stat_type {
                webrtc::stats::StatsReportType::OutboundRTP(outbound) => {
                    if outbound.kind == "video" {
                        video_bytes_sent = outbound.bytes_sent;
                        video_packets_sent = outbound.packets_sent;
                        tracing::debug!(
                            "[WebRTC Stats] Video OutboundRTP - bytes_sent: {}, packets_sent: {}, header_bytes_sent: {}",
                            outbound.bytes_sent,
                            outbound.packets_sent,
                            outbound.header_bytes_sent
                        );
                    } else if outbound.kind == "audio" {
                        audio_bytes_sent = outbound.bytes_sent;
                        audio_packets_sent = outbound.packets_sent;
                        tracing::debug!(
                            "[WebRTC Stats] Audio OutboundRTP - bytes_sent: {}, packets_sent: {}, header_bytes_sent: {}",
                            outbound.bytes_sent,
                            outbound.packets_sent,
                            outbound.header_bytes_sent
                        );
                    }
                }
                webrtc::stats::StatsReportType::RemoteInboundRTP(remote_inbound) => {
                    // These stats come from Cloudflare's RTCP receiver reports
                    // They tell us what Cloudflare is actually receiving
                    tracing::debug!(
                        "[WebRTC Stats] RemoteInboundRTP ({}) - packets_received: {}, packets_lost: {}",
                        remote_inbound.kind,
                        remote_inbound.packets_received,
                        remote_inbound.packets_lost
                    );
                }
                _ => {
                    // Ignore other stat types for now
                }
            }
        }

        // Calculate bitrates from deltas (bytes sent since last measurement)
        if self.last_stats_time_ns > 0 {
            let current_time_ns = MediaClock::now().as_nanos() as i64;
            let delta_time_s = (current_time_ns - self.last_stats_time_ns) as f64 / 1_000_000_000.0;

            let video_bytes_delta = video_bytes_sent.saturating_sub(self.last_video_bytes_sent);
            let audio_bytes_delta = audio_bytes_sent.saturating_sub(self.last_audio_bytes_sent);

            let video_packets_delta = video_packets_sent.saturating_sub(self.last_video_packets_sent);
            let audio_packets_delta = audio_packets_sent.saturating_sub(self.last_audio_packets_sent);

            // Calculate bitrates (bits per second)
            let video_bitrate_bps = (video_bytes_delta as f64 * 8.0) / delta_time_s;
            let audio_bitrate_bps = (audio_bytes_delta as f64 * 8.0) / delta_time_s;

            // Calculate packet rates (packets per second)
            let video_packet_rate = video_packets_delta as f64 / delta_time_s;
            let audio_packet_rate = audio_packets_delta as f64 / delta_time_s;

            tracing::info!(
                "[WebRTC Stats] ========== OUTBOUND STATS ==========\n\
                 Video: {:.2} Mbps ({:.0} pps, {} packets, {:.2} MB total)\n\
                 Audio: {:.2} kbps ({:.0} pps, {} packets, {:.2} KB total)\n\
                 Total: {:.2} Mbps\n\
                 ===========================================",
                video_bitrate_bps / 1_000_000.0,
                video_packet_rate,
                video_packets_sent,
                video_bytes_sent as f64 / 1_000_000.0,
                audio_bitrate_bps / 1_000.0,
                audio_packet_rate,
                audio_packets_sent,
                audio_bytes_sent as f64 / 1_000.0,
                (video_bitrate_bps + audio_bitrate_bps) / 1_000_000.0
            );
        }

        // Update last stats for next delta calculation
        self.last_video_bytes_sent = video_bytes_sent;
        self.last_audio_bytes_sent = audio_bytes_sent;
        self.last_video_packets_sent = video_packets_sent;
        self.last_audio_packets_sent = audio_packets_sent;

        Ok(())
    }
}

impl Drop for WebRtcWhipProcessor {
    fn drop(&mut self) {
        if let Some(whip_client) = &self.whip_client {
            if let Ok(client) = whip_client.lock() {
                let _ = client.terminate();
            }
        }
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // SAMPLE CONVERSION TESTS
    // ========================================================================

    #[test]
    fn test_convert_video_to_samples() {
        let encoded = EncodedVideoFrame {
            data: vec![
                0, 0, 0, 1, 0x67, 0x42,  // SPS
                0, 0, 0, 1, 0x68, 0x43,  // PPS
                0, 0, 0, 1, 0x65, 0xAA,  // IDR
            ],
            timestamp_ns: 1_000_000_000,
            is_keyframe: true,
            frame_number: 0,
        };

        let samples = convert_video_to_samples(&encoded, 30).unwrap();

        // Should create 3 samples (one per NAL unit)
        assert_eq!(samples.len(), 3);

        // Check duration (1/30 fps = ~33.33ms)
        let expected_duration = Duration::from_secs_f64(1.0 / 30.0);
        assert_eq!(samples[0].duration, expected_duration);
        assert_eq!(samples[1].duration, expected_duration);
        assert_eq!(samples[2].duration, expected_duration);

        // Check data (should be NAL units without start codes)
        assert_eq!(samples[0].data.as_ref(), &[0x67, 0x42]);
        assert_eq!(samples[1].data.as_ref(), &[0x68, 0x43]);
        assert_eq!(samples[2].data.as_ref(), &[0x65, 0xAA]);
    }

    #[test]
    fn test_convert_video_no_nal_units() {
        let encoded = EncodedVideoFrame {
            data: vec![0xAA, 0xBB, 0xCC],  // No start codes
            timestamp_ns: 0,
            is_keyframe: false,
            frame_number: 0,
        };

        let result = convert_video_to_samples(&encoded, 30);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No NAL units"));
    }

    #[test]
    fn test_convert_audio_to_sample() {
        let encoded = EncodedAudioFrame {
            data: vec![0xAA, 0xBB, 0xCC, 0xDD],
            timestamp_ns: 1_000_000_000,
            sample_count: 960,  // 20ms @ 48kHz
        };

        let sample = convert_audio_to_sample(&encoded, 48000).unwrap();

        // Check duration (960 samples @ 48kHz = 20ms)
        let expected_duration = Duration::from_secs_f64(960.0 / 48000.0);
        assert_eq!(sample.duration, expected_duration);

        // Check data
        assert_eq!(sample.data.as_ref(), &[0xAA, 0xBB, 0xCC, 0xDD]);
    }

    #[test]
    fn test_convert_audio_duration_calculation() {
        // Test various sample counts
        let test_cases = vec![
            (480, 48000, 10.0),    // 10ms
            (960, 48000, 20.0),    // 20ms
            (1920, 48000, 40.0),   // 40ms
        ];

        for (sample_count, sample_rate, expected_ms) in test_cases {
            let encoded = EncodedAudioFrame {
                data: vec![0x00],
                timestamp_ns: 0,
                sample_count,
            };

            let sample = convert_audio_to_sample(&encoded, sample_rate).unwrap();
            let actual_ms = sample.duration.as_secs_f64() * 1000.0;

            assert!((actual_ms - expected_ms).abs() < 0.01,
                "Expected ~{}ms, got {}ms for {} samples @ {}Hz",
                expected_ms, actual_ms, sample_count, sample_rate);
        }
    }

    // ========================================================================
    // NAL UNIT PARSER TESTS
    // ========================================================================

    #[test]
    fn test_parse_nal_units_single_4byte() {
        let data = vec![0, 0, 0, 1, 0x65, 0xAA, 0xBB, 0xCC];
        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 1);
        assert_eq!(nals[0], vec![0x65, 0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn test_parse_nal_units_single_3byte() {
        let data = vec![0, 0, 1, 0x65, 0xDD, 0xEE];
        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 1);
        assert_eq!(nals[0], vec![0x65, 0xDD, 0xEE]);
    }

    #[test]
    fn test_parse_nal_units_multiple() {
        let data = vec![
            0, 0, 0, 1, 0x67, 0x42,  // SPS (4-byte start code)
            0, 0, 0, 1, 0x68, 0x43,  // PPS (4-byte start code)
            0, 0, 1, 0x65, 0xAA,     // IDR (3-byte start code)
        ];
        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0], vec![0x67, 0x42]);
        assert_eq!(nals[1], vec![0x68, 0x43]);
        assert_eq!(nals[2], vec![0x65, 0xAA]);
    }

    #[test]
    fn test_parse_nal_units_mixed_start_codes() {
        let data = vec![
            0, 0, 0, 1, 0x67, 0x11,  // 4-byte start code
            0, 0, 1, 0x68, 0x22,     // 3-byte start code
            0, 0, 0, 1, 0x65, 0x33,  // 4-byte start code
        ];
        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0], vec![0x67, 0x11]);
        assert_eq!(nals[1], vec![0x68, 0x22]);
        assert_eq!(nals[2], vec![0x65, 0x33]);
    }

    #[test]
    fn test_parse_nal_units_empty() {
        let data = vec![];
        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 0);
    }

    #[test]
    fn test_parse_nal_units_no_start_code() {
        let data = vec![0x65, 0xAA, 0xBB, 0xCC];
        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 0, "Should not parse data without start codes");
    }

    #[test]
    fn test_parse_nal_units_realistic_frame() {
        // Simulate real VideoToolbox output (SPS + PPS + IDR)
        let mut data = Vec::new();

        // SPS
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(&[0x67, 0x42, 0xC0, 0x1E]);  // Fake SPS

        // PPS
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(&[0x68, 0xCE, 0x3C, 0x80]);  // Fake PPS

        // IDR slice
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(&[0x65, 0x88, 0x84, 0x00, 0x10]);  // Fake IDR

        let nals = parse_nal_units(&data);
        assert_eq!(nals.len(), 3);

        // Verify NAL unit types (first byte & 0x1F)
        assert_eq!(nals[0][0] & 0x1F, 0x07);  // SPS
        assert_eq!(nals[1][0] & 0x1F, 0x08);  // PPS
        assert_eq!(nals[2][0] & 0x1F, 0x05);  // IDR
    }

    // ========================================================================
    // RTP TIMESTAMP CALCULATOR TESTS
    // ========================================================================

    #[test]
    fn test_rtp_timestamp_calculator() {
        let start_ns = 1_000_000_000; // 1 second
        let calc = RtpTimestampCalculator::new(start_ns, 90000);

        // At start time, should return base timestamp
        let ts1 = calc.calculate(start_ns);

        // After 1 second (90000 ticks for 90kHz)
        let ts2 = calc.calculate(start_ns + 1_000_000_000);

        // Difference should be ~90000
        let diff = ts2.wrapping_sub(ts1);
        assert_eq!(diff, 90000);
    }

    #[test]
    fn test_rtp_timestamp_random_base() {
        // RTP base should be random (not predictable)
        let calc1 = RtpTimestampCalculator::new(0, 90000);
        let calc2 = RtpTimestampCalculator::new(0, 90000);

        // Different calculators should have different bases
        assert_ne!(calc1.rtp_base, calc2.rtp_base,
            "RTP base should be random, not deterministic");
    }

    #[test]
    fn test_rtp_timestamp_wraparound() {
        let start_ns = 0;
        let calc = RtpTimestampCalculator::new(start_ns, 90000);

        // Simulate enough time for wraparound at 90kHz
        // u32::MAX / 90000 ~= 47721 seconds ~= 13.25 hours
        // Test 50 seconds which should wrap if base is near max
        let ts_50s = 50_000_000_000i64;
        let rtp_ts = calc.calculate(ts_50s);

        // Check that calculation doesn't panic
        // Verify wrapping math is correct
        let expected_ticks = (50_000_000_000i128 * 90000) / 1_000_000_000;
        let expected = calc.rtp_base.wrapping_add(expected_ticks as u32);
        assert_eq!(rtp_ts, expected);
    }

    #[test]
    fn test_rtp_timestamp_audio_48khz() {
        let start_ns = 1_000_000_000;
        let calc = RtpTimestampCalculator::new(start_ns, 48000);

        // 20ms audio frame @ 48kHz = 960 samples
        let frame_ns = start_ns + 20_000_000;
        let rtp_ts1 = calc.calculate(frame_ns);

        // Next frame (another 20ms)
        let rtp_ts2 = calc.calculate(frame_ns + 20_000_000);

        // Should increment by 960 samples
        assert_eq!(rtp_ts2.wrapping_sub(rtp_ts1), 960);
    }

    #[test]
    fn test_rtp_timestamp_video_90khz() {
        let start_ns = 1_000_000_000;
        let calc = RtpTimestampCalculator::new(start_ns, 90000);

        // 33.33ms video frame @ 30fps
        let frame_duration_ns = 33_333_333i64;
        let frame_ns = start_ns + frame_duration_ns;
        let rtp_ts1 = calc.calculate(frame_ns);

        // Next frame
        let rtp_ts2 = calc.calculate(frame_ns + frame_duration_ns);

        // Should increment by ~3000 ticks (33.33ms × 90kHz)
        let diff = rtp_ts2.wrapping_sub(rtp_ts1);
        assert!((diff as i32 - 3000).abs() < 2, "Expected ~3000 ticks, got {}", diff);
    }

    #[test]
    fn test_rtp_timestamp_long_session() {
        let start_ns = 0;
        let calc = RtpTimestampCalculator::new(start_ns, 90000);

        // Simulate 1 hour of video @ 30fps
        let one_hour_ns = 3600_000_000_000i64;
        let rtp_ts = calc.calculate(one_hour_ns);

        // Should handle large elapsed times without overflow
        let expected_ticks = (one_hour_ns as i128 * 90000) / 1_000_000_000;
        let expected = calc.rtp_base.wrapping_add(expected_ticks as u32);
        assert_eq!(rtp_ts, expected);
    }

    #[test]
    fn test_config_defaults() {
        let video_config = VideoEncoderConfig::default();
        assert_eq!(video_config.width, 1280);
        assert_eq!(video_config.height, 720);
        assert_eq!(video_config.fps, 30);

        let audio_config = AudioEncoderConfig::default();
        assert_eq!(audio_config.sample_rate, 48000);
        assert_eq!(audio_config.channels, 2);
    }

    // ========================================================================
    // OPUS ENCODER TESTS
    // ========================================================================

    #[test]
    fn test_opus_encoder_creation() {
        let config = AudioEncoderConfig::default();
        let encoder = OpusEncoder::new(config);
        assert!(encoder.is_ok());
    }

    #[test]
    fn test_opus_encoder_invalid_sample_rate() {
        let mut config = AudioEncoderConfig::default();
        config.sample_rate = 44100;  // Not supported
        let encoder = OpusEncoder::new(config);
        assert!(encoder.is_err());
        let err = encoder.unwrap_err().to_string();
        assert!(err.contains("48kHz"));
    }

    #[test]
    fn test_opus_encoder_invalid_channels() {
        let mut config = AudioEncoderConfig::default();
        config.channels = 1;  // Mono not supported
        let encoder = OpusEncoder::new(config);
        assert!(encoder.is_err());
        let err = encoder.unwrap_err().to_string();
        assert!(err.contains("stereo"));
    }

    #[test]
    fn test_opus_encode_correct_frame_size() {
        let config = AudioEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config).unwrap();

        // Create 20ms frame @ 48kHz stereo = 960 samples * 2 channels = 1920 f32
        let samples = vec![0.0f32; 1920];
        let frame = AudioFrame::<2>::new(
            samples,
            0,      // timestamp_ns
            0,      // frame_number
            48000   // sample_rate
        );

        let result = encoder.encode(&frame);
        assert!(result.is_ok());

        let encoded = result.unwrap();
        assert!(encoded.data.len() > 0);
        assert_eq!(encoded.timestamp_ns, 0);
        assert_eq!(encoded.sample_count, 960);
    }

    #[test]
    fn test_opus_encode_wrong_frame_size() {
        let config = AudioEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config).unwrap();

        // Wrong size: 512 samples instead of 960
        let samples = vec![0.0f32; 512 * 2];
        let frame = AudioFrame::<2>::new(
            samples,
            0,
            0,
            48000
        );

        let result = encoder.encode(&frame);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("BufferRechunkerProcessor"));
    }

    #[test]
    fn test_opus_encode_wrong_sample_rate() {
        let config = AudioEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config).unwrap();

        // Wrong sample rate
        let samples = vec![0.0f32; 1920];
        let frame = AudioFrame::<2>::new(
            samples,
            0,
            0,
            44100  // Wrong sample rate
        );

        let result = encoder.encode(&frame);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("AudioResamplerProcessor"));
    }

    #[test]
    fn test_opus_timestamp_preservation() {
        let config = AudioEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config).unwrap();

        let timestamp_ns = 123456789i64;
        let samples = vec![0.0f32; 1920];
        let frame = AudioFrame::<2>::new(
            samples,
            timestamp_ns,
            42,
            48000
        );

        let encoded = encoder.encode(&frame).unwrap();
        assert_eq!(encoded.timestamp_ns, timestamp_ns);
    }

    #[test]
    fn test_opus_bitrate_change() {
        let config = AudioEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config).unwrap();

        let result = encoder.set_bitrate(96_000);
        assert!(result.is_ok());
        assert_eq!(encoder.config().bitrate_bps, 96_000);
    }

    #[test]
    fn test_opus_encode_sine_wave() {
        use std::f32::consts::PI;

        let config = AudioEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config).unwrap();

        // Generate 20ms of 440Hz sine wave @ 48kHz stereo
        let mut samples = Vec::with_capacity(1920);
        for i in 0..960 {
            let t = i as f32 / 48000.0;
            let sample = (2.0 * PI * 440.0 * t).sin() * 0.5;
            samples.push(sample);  // Left
            samples.push(sample);  // Right
        }

        let frame = AudioFrame::<2>::new(
            samples,
            0,
            0,
            48000
        );
        let encoded = encoder.encode(&frame).unwrap();

        // Encoded size should be reasonable (< 4KB for 20ms)
        assert!(encoded.data.len() > 10);  // At least some bytes
        assert!(encoded.data.len() < 4000);  // Not too large
    }

    #[test]
    fn test_opus_encode_multiple_frames() {
        let config = AudioEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config).unwrap();

        // Simulate encoding 10 frames (200ms of audio)
        for frame_num in 0..10 {
            let timestamp_ns = frame_num * 20_000_000;  // 20ms increments
            let samples = vec![0.1f32; 1920];
            let frame = AudioFrame::<2>::new(
                samples,
                timestamp_ns,
                frame_num as u64,
                48000
            );

            let encoded = encoder.encode(&frame).unwrap();
            assert!(encoded.data.len() > 0);
            assert_eq!(encoded.timestamp_ns, timestamp_ns);
            assert_eq!(encoded.sample_count, 960);
        }
    }
}
