// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! FFmpeg-based H.264 encoder for Linux.

use std::sync::Once;

use ffmpeg::codec::context::Context as CodecContext;
use ffmpeg::software::scaling;
use ffmpeg::util::format::Pixel;
use ffmpeg_next as ffmpeg;

use crate::_generated_::{Encodedvideoframe, Videoframe};
use crate::core::codec::video_codec::H264Profile;
use crate::core::{GpuContext, Result, RuntimeContext, StreamError, VideoEncoderConfig};

static FFMPEG_INIT: Once = Once::new();

fn ensure_ffmpeg_initialized() {
    FFMPEG_INIT.call_once(|| {
        ffmpeg::init().expect("Failed to initialize FFmpeg");
    });
}

/// FFmpeg-based hardware encoder.
///
/// Uses FFmpeg's libavcodec for H.264 encoding on Linux.
pub struct FFmpegEncoder {
    config: VideoEncoderConfig,
    encoder: ffmpeg::encoder::video::Video,
    scaler: scaling::Context,
    frame_count: u64,
    force_next_keyframe: bool,
    codec_name: String,
}

impl FFmpegEncoder {
    /// Create a new FFmpeg encoder.
    pub fn new(
        config: VideoEncoderConfig,
        _gpu_context: Option<GpuContext>,
        _ctx: &RuntimeContext,
    ) -> Result<Self> {
        ensure_ffmpeg_initialized();

        // Try hardware encoders first, fall back to software
        let codec = Self::find_best_h264_encoder()?;

        let mut context = CodecContext::new_with_codec(codec);
        let mut video = context.encoder().video().map_err(|e| {
            StreamError::Configuration(format!("Failed to create video encoder context: {e}"))
        })?;

        video.set_width(config.width);
        video.set_height(config.height);
        video.set_format(Pixel::YUV420P);
        video.set_time_base(ffmpeg::Rational::new(1, config.fps as i32));
        video.set_frame_rate(Some(ffmpeg::Rational::new(config.fps as i32, 1)));
        video.set_bit_rate(config.bitrate_bps as usize);
        video.set_max_bit_rate(config.bitrate_bps as usize);
        video.set_gop(config.keyframe_interval_frames);

        // Set H.264 profile
        match config.codec {
            crate::core::VideoCodec::H264(profile) => {
                let profile_val = match profile {
                    H264Profile::Baseline => 66,
                    H264Profile::Main => 77,
                    H264Profile::High => 100,
                };
                unsafe {
                    (*video.as_mut_ptr()).profile = profile_val;
                }
            }
        }

        // Set low-latency options (codec-specific)
        let codec_name = codec.name().to_string();
        let mut opts = ffmpeg::Dictionary::new();
        if codec_name == "libx264" {
            if config.low_latency {
                opts.set("preset", "ultrafast");
                opts.set("tune", "zerolatency");
            } else {
                opts.set("preset", "medium");
            }
        } else if codec_name == "h264_nvenc" {
            if config.low_latency {
                opts.set("preset", "p1");
                opts.set("tune", "ull");
            }
        }
        // h264_vaapi has no equivalent preset/tune options

        let encoder = video.open_as_with(codec, opts).map_err(|e| {
            StreamError::Configuration(format!("Failed to open H.264 encoder: {e}"))
        })?;

        // Create BGRA → YUV420P scaler for pixel format conversion
        let scaler = scaling::Context::get(
            Pixel::BGRA,
            config.width,
            config.height,
            Pixel::YUV420P,
            config.width,
            config.height,
            scaling::Flags::BILINEAR,
        )
        .map_err(|e| {
            StreamError::Configuration(format!("Failed to create pixel format scaler: {e}"))
        })?;

        Ok(Self {
            config,
            encoder,
            scaler,
            frame_count: 0,
            force_next_keyframe: false,
            codec_name,
        })
    }

    fn find_best_h264_encoder() -> Result<ffmpeg::Codec> {
        // Try NVIDIA hardware encoder first
        if let Some(codec) = ffmpeg::encoder::find_by_name("h264_nvenc") {
            tracing::info!("Using NVIDIA NVENC H.264 encoder");
            return Ok(codec);
        }

        // Try VAAPI hardware encoder (Intel/AMD)
        if let Some(codec) = ffmpeg::encoder::find_by_name("h264_vaapi") {
            tracing::info!("Using VAAPI H.264 encoder");
            return Ok(codec);
        }

        // Fall back to libx264 software encoder
        if let Some(codec) = ffmpeg::encoder::find_by_name("libx264") {
            tracing::info!("Using libx264 software H.264 encoder");
            return Ok(codec);
        }

        Err(StreamError::Configuration(
            "No H.264 encoder found (tried h264_nvenc, h264_vaapi, libx264)".into(),
        ))
    }

    /// Encode a video frame.
    pub fn encode(&mut self, frame: &Videoframe, gpu: &GpuContext) -> Result<Encodedvideoframe> {
        // Resolve the Videoframe to get the underlying pixel buffer
        let pixel_buffer = gpu.resolve_videoframe_buffer(frame)?;
        let buffer_ref = pixel_buffer.buffer_ref();
        let vulkan_buffer = &buffer_ref.inner;

        let width = vulkan_buffer.width();
        let height = vulkan_buffer.height();
        let src_ptr = vulkan_buffer.mapped_ptr();
        let src_bpp = vulkan_buffer.bytes_per_pixel();
        let src_row_bytes = (width * src_bpp) as usize;

        // Create BGRA input frame from pixel buffer data
        let mut bgra_frame = ffmpeg::frame::Video::new(Pixel::BGRA, width, height);
        let bgra_stride = bgra_frame.stride(0);
        unsafe {
            let dst_data = bgra_frame.data_mut(0);
            for row in 0..height as usize {
                let src_offset = row * src_row_bytes;
                let dst_offset = row * bgra_stride;
                let src_slice = std::slice::from_raw_parts(src_ptr.add(src_offset), src_row_bytes);
                dst_data[dst_offset..dst_offset + src_row_bytes].copy_from_slice(src_slice);
            }
        }

        // Scale BGRA → YUV420P
        let mut yuv_frame = ffmpeg::frame::Video::new(Pixel::YUV420P, width, height);
        self.scaler
            .run(&bgra_frame, &mut yuv_frame)
            .map_err(|e| StreamError::GpuError(format!("BGRA to YUV420P scaling failed: {e}")))?;

        // Set presentation timestamp
        yuv_frame.set_pts(Some(self.frame_count as i64));

        // Force keyframe if requested
        if self.force_next_keyframe {
            yuv_frame.set_kind(ffmpeg::picture::Type::I);
            self.force_next_keyframe = false;
        }

        // Send frame to encoder
        self.encoder.send_frame(&yuv_frame).map_err(|e| {
            StreamError::Configuration(format!("Failed to send frame to encoder: {e}"))
        })?;

        // Receive encoded packet (encoder may buffer frames before producing output)
        let mut packet = ffmpeg::Packet::empty();
        let got_packet = match self.encoder.receive_packet(&mut packet) {
            Ok(()) => true,
            Err(ffmpeg::Error::Other {
                errno: libc::EAGAIN,
            }) => false,
            Err(e) => {
                return Err(StreamError::Configuration(format!(
                    "Failed to receive encoded packet: {e}"
                )));
            }
        };

        let timestamp_ns: i64 = frame.timestamp_ns.parse().unwrap_or(0);
        let frame_number = self.frame_count;
        self.frame_count += 1;

        Ok(Encodedvideoframe {
            data: if got_packet {
                packet.data().unwrap_or(&[]).to_vec()
            } else {
                Vec::new()
            },
            frame_number: frame_number.to_string(),
            is_keyframe: if got_packet { packet.is_key() } else { false },
            timestamp_ns: timestamp_ns.to_string(),
        })
    }

    /// Set the target bitrate.
    pub fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        self.config.bitrate_bps = bitrate_bps;
        // FFmpeg does not support dynamic bitrate changes on an open encoder context.
        // The new bitrate will take effect if the encoder is re-initialized.
        Ok(())
    }

    /// Force the next frame to be a keyframe.
    pub fn force_keyframe(&mut self) {
        self.force_next_keyframe = true;
    }

    /// Get the encoder configuration.
    pub fn config(&self) -> &VideoEncoderConfig {
        &self.config
    }
}

// FFmpegEncoder is Send because FFmpeg contexts can be used from any thread
// (with proper synchronization, which we handle internally)
unsafe impl Send for FFmpegEncoder {}
