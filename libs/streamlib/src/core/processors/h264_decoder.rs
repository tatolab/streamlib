// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// H.264 Decoder Processor
//
// Decodes EncodedVideoFrame (H.264 NAL units) to VideoFrame using
// platform-specific decoders (Vulkan Video on Linux, VideoToolbox on macOS).
// Handles SPS/PPS parameter set extraction and decoder initialization.

use crate::_generated_::Encodedvideoframe;
use crate::core::codec::{VideoDecoder, VideoDecoderConfig};
use crate::core::{GpuContext, Result, RuntimeContext, StreamError};
use bytes::Bytes;

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.h264_decoder")]
pub struct H264DecoderProcessor {
    /// Runtime context.
    runtime_context: Option<RuntimeContext>,

    /// GPU context for decoded frame buffers.
    gpu_context: Option<GpuContext>,

    /// Video decoder (initialized after SPS/PPS received).
    video_decoder: Option<VideoDecoder>,

    /// Cached SPS NAL for decoder initialization.
    sps_nal: Option<Bytes>,

    /// Cached PPS NAL for decoder initialization.
    pps_nal: Option<Bytes>,

    /// Frames decoded counter.
    frames_decoded: u64,
}

impl crate::core::ReactiveProcessor for H264DecoderProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());
        self.runtime_context = Some(ctx);
        tracing::info!("[H264Decoder] Initialized (waiting for SPS/PPS)");
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            frames_decoded = self.frames_decoded,
            "[H264Decoder] Shutting down"
        );
        self.video_decoder.take();
        self.gpu_context.take();
        self.runtime_context.take();
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        if !self.inputs.has_data("encoded_video_in") {
            return Ok(());
        }
        let encoded: Encodedvideoframe = self.inputs.read("encoded_video_in")?;

        // Log NAL types present in this packet
        let nal_types = scan_nal_types(&encoded.data);
        if !nal_types.is_empty() {
            tracing::info!(
                "[H264Decoder] Received {} bytes, NAL types: {:?}",
                encoded.data.len(),
                nal_types
            );
        }

        // Scan NAL units in the encoded data for SPS/PPS
        let (sps, pps) = extract_h264_parameter_sets(&encoded.data);
        if let Some(s) = sps {
            tracing::info!("[H264Decoder] SPS extracted ({} bytes)", s.len());
            self.sps_nal = Some(s);
        }
        if let Some(p) = pps {
            tracing::info!("[H264Decoder] PPS extracted ({} bytes)", p.len());
            self.pps_nal = Some(p);
        }

        // Try to initialize decoder if we have SPS + PPS but no decoder yet
        if self.video_decoder.is_none() {
            if let (Some(sps), Some(pps), Some(ctx)) =
                (&self.sps_nal, &self.pps_nal, &self.runtime_context)
            {
                match VideoDecoder::new(VideoDecoderConfig::default(), ctx) {
                    Ok(mut decoder) => {
                        if let Err(e) = decoder.update_format(sps, pps) {
                            tracing::error!("[H264Decoder] Failed to set format: {}", e);
                        } else {
                            tracing::info!("[H264Decoder] Decoder initialized with SPS/PPS");
                            self.video_decoder = Some(decoder);
                        }
                    }
                    Err(e) => {
                        tracing::error!("[H264Decoder] Failed to create decoder: {}", e);
                    }
                }
            }
        }

        // Decode if decoder is ready
        if let Some(decoder) = &mut self.video_decoder {
            let gpu = self
                .gpu_context
                .as_ref()
                .ok_or_else(|| StreamError::Runtime("GPU context not available".into()))?;

            let timestamp_ns: i64 = encoded.timestamp_ns.parse().unwrap_or(0);

            match decoder.decode(&encoded.data, timestamp_ns, gpu) {
                Ok(Some(frame)) => {
                    self.outputs.write("video_out", &frame)?;
                    self.frames_decoded += 1;
                    if self.frames_decoded == 1 {
                        tracing::info!("[H264Decoder] First frame decoded");
                    } else if self.frames_decoded % 100 == 0 {
                        tracing::info!(
                            frames = self.frames_decoded,
                            "[H264Decoder] Decode progress"
                        );
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!("[H264Decoder] Decode error: {}", e);
                }
            }
        }

        Ok(())
    }
}

// ============================================================================
// H.264 NAL PARSING
// ============================================================================

/// Extract SPS (NAL type 7) and PPS (NAL type 8) RBSP payloads from Annex B data.
///
/// Returns the raw byte sequence payload (RBSP) with the NAL header byte stripped.
/// The Annex B structure is: [start_code][nal_header_byte][rbsp_payload...]
/// The NAL header byte encodes forbidden_zero_bit, nal_ref_idc, and nal_unit_type.
/// Downstream consumers (VulkanVideoDecodeSession, VideoToolbox) expect RBSP only.
fn extract_h264_parameter_sets(data: &[u8]) -> (Option<Bytes>, Option<Bytes>) {
    let mut sps = None;
    let mut pps = None;
    let mut i = 0;

    while i < data.len().saturating_sub(4) {
        // Look for Annex B start codes (0x00000001 or 0x000001)
        let (start_code_len, found) = if i + 3 < data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            (4, true)
        } else if i + 2 < data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            (3, true)
        } else {
            (0, false)
        };

        if !found {
            i += 1;
            continue;
        }

        let nal_header_pos = i + start_code_len;
        if nal_header_pos >= data.len() {
            break;
        }

        let nal_type = data[nal_header_pos] & 0x1F;
        // RBSP starts after the NAL header byte
        let rbsp_start = nal_header_pos + 1;

        // Find NAL end (next start code or end of data)
        let mut nal_end = data.len();
        let mut j = rbsp_start;
        while j < data.len().saturating_sub(2) {
            if data[j] == 0
                && data[j + 1] == 0
                && (data[j + 2] == 1
                    || (j + 3 < data.len() && data[j + 2] == 0 && data[j + 3] == 1))
            {
                nal_end = j;
                break;
            }
            j += 1;
        }

        match nal_type {
            7 => {
                if rbsp_start < nal_end {
                    sps = Some(Bytes::copy_from_slice(&data[rbsp_start..nal_end]));
                }
            }
            8 => {
                if rbsp_start < nal_end {
                    pps = Some(Bytes::copy_from_slice(&data[rbsp_start..nal_end]));
                }
            }
            _ => {}
        }

        i = nal_end;
    }

    (sps, pps)
}

/// Scan NAL unit types present in Annex B data for logging.
fn scan_nal_types(data: &[u8]) -> Vec<&'static str> {
    let mut types = Vec::new();
    let mut i = 0;

    while i < data.len().saturating_sub(3) {
        let (start_code_len, found) = if i + 3 < data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            (4, true)
        } else if i + 2 < data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            (3, true)
        } else {
            (0, false)
        };

        if !found {
            i += 1;
            continue;
        }

        let nal_start = i + start_code_len;
        if nal_start >= data.len() {
            break;
        }

        let nal_type = data[nal_start] & 0x1F;
        types.push(match nal_type {
            1 => "non-IDR(1)",
            2 => "partA(2)",
            3 => "partB(3)",
            4 => "partC(4)",
            5 => "IDR(5)",
            6 => "SEI(6)",
            7 => "SPS(7)",
            8 => "PPS(8)",
            9 => "AUD(9)",
            _ => "other",
        });

        i = nal_start + 1;
    }

    types
}
