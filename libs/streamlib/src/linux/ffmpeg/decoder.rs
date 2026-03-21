// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! FFmpeg-based H.264 decoder for Linux.

use std::sync::Once;

use ffmpeg::codec::context::Context as CodecContext;
use ffmpeg::software::scaling;
use ffmpeg::util::format::Pixel;
use ffmpeg_next as ffmpeg;

use crate::_generated_::Videoframe;
use crate::core::codec::VideoDecoderConfig;
use crate::core::rhi::PixelFormat;
use crate::core::{GpuContext, Result, RuntimeContext, StreamError};

static FFMPEG_INIT: Once = Once::new();

fn ensure_ffmpeg_initialized() {
    FFMPEG_INIT.call_once(|| {
        ffmpeg::init().expect("Failed to initialize FFmpeg");
    });
}

/// FFmpeg-based hardware decoder.
///
/// Uses FFmpeg's libavcodec for H.264 decoding on Linux.
pub struct FFmpegDecoder {
    config: VideoDecoderConfig,
    decoder: ffmpeg::decoder::video::Video,
    scaler: Option<scaling::Context>,
    frame_count: u64,
}

impl FFmpegDecoder {
    /// Create a new FFmpeg decoder.
    pub fn new(config: VideoDecoderConfig, _ctx: &RuntimeContext) -> Result<Self> {
        ensure_ffmpeg_initialized();

        let codec = ffmpeg::decoder::find(ffmpeg::codec::Id::H264).ok_or_else(|| {
            StreamError::Configuration("H.264 decoder not found in FFmpeg".into())
        })?;

        let mut context = CodecContext::new_with_codec(codec);

        // Set expected dimensions
        unsafe {
            (*context.as_mut_ptr()).width = config.width as i32;
            (*context.as_mut_ptr()).height = config.height as i32;
        }

        let decoder = context.decoder().video().map_err(|e| {
            StreamError::Configuration(format!("Failed to create video decoder context: {e}"))
        })?;

        Ok(Self {
            config,
            decoder,
            scaler: None,
            frame_count: 0,
        })
    }

    /// Update decoder format with SPS/PPS parameter sets.
    pub fn update_format(&mut self, sps: &[u8], pps: &[u8]) -> Result<()> {
        // Build extradata in Annex B format: [start_code + SPS + start_code + PPS]
        let start_code: &[u8] = &[0x00, 0x00, 0x00, 0x01];
        let extradata_len = start_code.len() + sps.len() + start_code.len() + pps.len();
        let mut extradata = Vec::with_capacity(extradata_len);
        extradata.extend_from_slice(start_code);
        extradata.extend_from_slice(sps);
        extradata.extend_from_slice(start_code);
        extradata.extend_from_slice(pps);

        // Set extradata on the decoder context
        unsafe {
            let ctx = self.decoder.as_mut_ptr();
            // Free existing extradata if any
            if !(*ctx).extradata.is_null() {
                ffmpeg::ffi::av_free((*ctx).extradata as *mut _);
                (*ctx).extradata = std::ptr::null_mut();
                (*ctx).extradata_size = 0;
            }
            // Allocate and copy new extradata
            let ptr = ffmpeg::ffi::av_malloc(
                extradata_len + ffmpeg::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize,
            ) as *mut u8;
            if ptr.is_null() {
                return Err(StreamError::Configuration(
                    "Failed to allocate extradata for decoder".into(),
                ));
            }
            std::ptr::copy_nonoverlapping(extradata.as_ptr(), ptr, extradata_len);
            // Zero padding bytes
            std::ptr::write_bytes(
                ptr.add(extradata_len),
                0,
                ffmpeg::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize,
            );
            (*ctx).extradata = ptr;
            (*ctx).extradata_size = extradata_len as i32;
        }

        // Parse SPS for dimensions (simplified: first byte after NAL header)
        // The decoder will parse the full SPS internally when it receives the data.
        // We just need the extradata set correctly.

        Ok(())
    }

    /// Decode H.264 NAL units to a video frame.
    pub fn decode(
        &mut self,
        nal_units_annex_b: &[u8],
        timestamp_ns: i64,
        gpu: &GpuContext,
    ) -> Result<Option<Videoframe>> {
        // Create packet from NAL unit data
        let mut packet = ffmpeg::Packet::copy(nal_units_annex_b);
        let pts = self.frame_count as i64;
        packet.set_pts(Some(pts));
        packet.set_dts(Some(pts));

        // Send packet to decoder
        self.decoder.send_packet(&packet).map_err(|e| {
            StreamError::Configuration(format!("Failed to send packet to decoder: {e}"))
        })?;

        // Try to receive decoded frame
        let mut decoded_frame = ffmpeg::frame::Video::empty();
        match self.decoder.receive_frame(&mut decoded_frame) {
            Ok(()) => {}
            Err(ffmpeg::Error::Other {
                errno: libc::EAGAIN,
            }) => {
                // Decoder needs more data before outputting a frame
                return Ok(None);
            }
            Err(e) => {
                return Err(StreamError::Configuration(format!(
                    "Failed to receive decoded frame: {e}"
                )));
            }
        }

        let dec_width = decoded_frame.width();
        let dec_height = decoded_frame.height();
        let dec_format = decoded_frame.format();

        // Create or update scaler if needed (decoded format → BGRA)
        if self.scaler.is_none()
            || dec_width != self.config.width
            || dec_height != self.config.height
        {
            self.config.width = dec_width;
            self.config.height = dec_height;

            self.scaler = Some(
                scaling::Context::get(
                    dec_format,
                    dec_width,
                    dec_height,
                    Pixel::BGRA,
                    dec_width,
                    dec_height,
                    scaling::Flags::BILINEAR,
                )
                .map_err(|e| {
                    StreamError::Configuration(format!("Failed to create decoder scaler: {e}"))
                })?,
            );
        }

        // Convert to BGRA
        let mut bgra_frame = ffmpeg::frame::Video::new(Pixel::BGRA, dec_width, dec_height);
        if let Some(ref mut scaler) = self.scaler {
            scaler
                .run(&decoded_frame, &mut bgra_frame)
                .map_err(|e| StreamError::GpuError(format!("YUV to BGRA scaling failed: {e}")))?;
        }

        // Acquire a pixel buffer from the GPU context
        let (pool_id, dest_buffer) =
            gpu.acquire_pixel_buffer(dec_width, dec_height, PixelFormat::Bgra32)?;
        let dest_ref = dest_buffer.buffer_ref();
        let vulkan_dest = &dest_ref.inner;
        let dest_ptr = vulkan_dest.mapped_ptr();
        let dest_bpp = vulkan_dest.bytes_per_pixel();
        let dest_row_bytes = (dec_width * dest_bpp) as usize;

        // Copy BGRA frame data to the VulkanPixelBuffer
        let bgra_stride = bgra_frame.stride(0);
        let src_data = bgra_frame.data(0);
        unsafe {
            for row in 0..dec_height as usize {
                let src_offset = row * bgra_stride;
                let dst_offset = row * dest_row_bytes;
                std::ptr::copy_nonoverlapping(
                    src_data[src_offset..].as_ptr(),
                    dest_ptr.add(dst_offset),
                    dest_row_bytes,
                );
            }
        }

        self.frame_count += 1;

        Ok(Some(Videoframe {
            width: dec_width,
            height: dec_height,
            surface_id: pool_id.to_string(),
            timestamp_ns: timestamp_ns.to_string(),
            frame_index: self.frame_count.to_string(),
        }))
    }

    /// Get the decoder configuration.
    pub fn config(&self) -> &VideoDecoderConfig {
        &self.config
    }
}

// FFmpegDecoder is Send because FFmpeg contexts can be used from any thread
// (with proper synchronization, which we handle internally)
unsafe impl Send for FFmpegDecoder {}
