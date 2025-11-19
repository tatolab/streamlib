// VideoToolbox H.264 Decoder (STUB - NOT YET IMPLEMENTED)
//
// This module will provide hardware-accelerated H.264 decoding using Apple's VideoToolbox.
//
// **STATUS**: Placeholder stub only. Full implementation pending.
//
// **REQUIRED FOR**: WHEP (WebRTC HTTP Egress Protocol) playback support
//
// **IMPLEMENTATION TASKS**:
// 1. VTDecompressionSession setup with SPS/PPS from SDP or in-band NAL units
// 2. Annex B → AVCC format conversion (inverse of encoder)
// 3. Async decode callback handling with frame queue
// 4. NV12 → RGBA pixel transfer using VTPixelTransferSession (reverse of encoder)
// 5. CVPixelBuffer → wgpu::Texture import via IOSurface
// 6. Proper thread-safe wrapper types for Send/Sync compliance
// 7. Main thread dispatch for all VideoToolbox APIs
// 8. Memory management for CoreFoundation objects (CFRetain/CFRelease)
//
// **REFERENCE IMPLEMENTATIONS**:
// - Encoder: src/apple/videotoolbox/encoder.rs (reverse operations)
// - Pixel transfer: src/apple/pixel_transfer.rs (needs NV12→RGBA method)
// - Format conversion: src/apple/videotoolbox/format.rs (annex_b_to_avcc exists)

use crate::core::{VideoFrame, StreamError, Result, GpuContext, RuntimeContext};
use std::sync::Arc;

/// Decoded video frame configuration
#[derive(Clone, Debug)]
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

/// VideoToolbox-based hardware H.264 decoder (STUB - NOT IMPLEMENTED)
///
/// **This is a placeholder.** The actual implementation is deferred until WHEP integration.
pub struct VideoToolboxDecoder {
    config: VideoDecoderConfig,
    _gpu_context: Option<GpuContext>,
    _runtime_context: Arc<RuntimeContext>,
}

impl VideoToolboxDecoder {
    /// Create a new VideoToolbox decoder (STUB)
    pub fn new(
        config: VideoDecoderConfig,
        gpu_context: Option<GpuContext>,
        ctx: &RuntimeContext,
    ) -> Result<Self> {
        tracing::warn!(
            "[VideoToolbox Decoder] STUB ONLY - Not yet implemented ({}x{})",
            config.width,
            config.height
        );

        Ok(Self {
            config,
            _gpu_context: gpu_context,
            _runtime_context: Arc::new(ctx.clone()),
        })
    }

    /// Decode H.264 NAL units to VideoFrame (STUB - returns error)
    ///
    /// # Arguments
    /// * `_nal_units_annex_b` - H.264 data in Annex B format (start codes)
    /// * `_timestamp_ns` - Presentation timestamp in nanoseconds
    ///
    /// # Returns
    /// Error indicating decoder is not yet implemented
    pub fn decode(
        &mut self,
        _nal_units_annex_b: &[u8],
        _timestamp_ns: i64,
    ) -> Result<Option<VideoFrame>> {
        Err(StreamError::Runtime(
            "VideoToolboxDecoder::decode() not yet implemented - see decoder.rs for implementation tasks".to_string()
        ))
    }

    /// Update SPS/PPS from NAL units (STUB)
    pub fn update_format(&mut self, _sps: &[u8], _pps: &[u8]) -> Result<()> {
        Err(StreamError::Runtime(
            "VideoToolboxDecoder::update_format() not yet implemented".to_string()
        ))
    }
}
