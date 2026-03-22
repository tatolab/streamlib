// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! FFmpeg MP4 muxer using libavformat.

use std::sync::Once;

use ffmpeg::rescale::Rescale;
use ffmpeg_next as ffmpeg;

use crate::_generated_::{Encodedaudioframe, Encodedvideoframe};
use crate::core::codec::Mp4MuxerConfig;
use crate::core::{AudioCodec, Result, StreamError};

static FFMPEG_INIT: Once = Once::new();

fn ensure_ffmpeg_initialized() {
    FFMPEG_INIT.call_once(|| {
        ffmpeg::init().expect("Failed to initialize FFmpeg");
    });
}

/// FFmpeg MP4 muxer using libavformat.
///
/// Muxes pre-encoded H.264 video and AAC/Opus audio into MP4 container.
pub struct FFmpegMuxer {
    config: Mp4MuxerConfig,
    output_context: ffmpeg::format::context::Output,
    video_stream_index: usize,
    audio_stream_index: Option<usize>,
    video_time_base: ffmpeg::Rational,
    audio_time_base: Option<ffmpeg::Rational>,
    header_written: bool,
}

impl FFmpegMuxer {
    /// Create a new FFmpeg MP4 muxer.
    pub fn new(config: Mp4MuxerConfig, _ctx: &crate::core::RuntimeContext) -> Result<Self> {
        ensure_ffmpeg_initialized();

        let output_path = config.output_path.to_str().ok_or_else(|| {
            StreamError::Configuration("Output path contains invalid UTF-8".into())
        })?;

        let mut output_context = ffmpeg::format::output(output_path).map_err(|e| {
            StreamError::Configuration(format!("Failed to create MP4 output context: {e}"))
        })?;

        // Add video stream
        let video_codec = ffmpeg::encoder::find(ffmpeg::codec::Id::H264)
            .ok_or_else(|| StreamError::Configuration("H.264 codec not found for muxer".into()))?;
        let mut video_stream = output_context
            .add_stream(video_codec)
            .map_err(|e| StreamError::Configuration(format!("Failed to add video stream: {e}")))?;

        let video_time_base = ffmpeg::Rational::new(1, config.video_fps as i32);
        video_stream.set_time_base(video_time_base);

        // Set video codec parameters
        unsafe {
            let params = (*video_stream.as_mut_ptr()).codecpar;
            (*params).codec_type = ffmpeg::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
            (*params).codec_id = ffmpeg::ffi::AVCodecID::AV_CODEC_ID_H264;
            (*params).width = config.video_width as i32;
            (*params).height = config.video_height as i32;
        }

        let video_stream_index = video_stream.index();

        // Add audio stream if configured
        let mut audio_stream_index = None;
        let mut audio_time_base = None;
        if let Some(audio_codec_type) = &config.audio_codec {
            let ffmpeg_audio_codec_id = match audio_codec_type {
                AudioCodec::Aac => ffmpeg::codec::Id::AAC,
                AudioCodec::Opus => ffmpeg::codec::Id::OPUS,
            };
            let audio_codec = ffmpeg::encoder::find(ffmpeg_audio_codec_id).ok_or_else(|| {
                StreamError::Configuration(format!(
                    "Audio codec {:?} not found for muxer",
                    audio_codec_type
                ))
            })?;
            let mut audio_stream = output_context.add_stream(audio_codec).map_err(|e| {
                StreamError::Configuration(format!("Failed to add audio stream: {e}"))
            })?;

            let sample_rate = config.audio_sample_rate.unwrap_or(48000);
            let channels = config.audio_channels.unwrap_or(2);
            let tb = ffmpeg::Rational::new(1, sample_rate as i32);
            audio_stream.set_time_base(tb);

            unsafe {
                let params = (*audio_stream.as_mut_ptr()).codecpar;
                (*params).codec_type = ffmpeg::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO;
                (*params).codec_id = match audio_codec_type {
                    AudioCodec::Aac => ffmpeg::ffi::AVCodecID::AV_CODEC_ID_AAC,
                    AudioCodec::Opus => ffmpeg::ffi::AVCodecID::AV_CODEC_ID_OPUS,
                };
                (*params).sample_rate = sample_rate as i32;
                ffmpeg::ffi::av_channel_layout_default(&mut (*params).ch_layout, channels as i32);
            }

            audio_stream_index = Some(audio_stream.index());
            audio_time_base = Some(tb);
        }

        // Write file header with faststart for streaming-friendly moov atom placement
        let mut format_opts = ffmpeg::Dictionary::new();
        format_opts.set("movflags", "faststart");
        let _ = output_context
            .write_header_with(format_opts)
            .map_err(|e| StreamError::Configuration(format!("Failed to write MP4 header: {e}")))?;

        Ok(Self {
            config,
            output_context,
            video_stream_index,
            audio_stream_index,
            video_time_base,
            audio_time_base,
            header_written: true,
        })
    }

    /// Write an encoded video frame.
    pub fn write_video(&mut self, frame: &Encodedvideoframe) -> Result<()> {
        let mut packet = ffmpeg::Packet::copy(&frame.data);

        let timestamp_ns: i64 = frame.timestamp_ns.parse().unwrap_or(0);
        // Convert nanoseconds to microseconds (TIME_BASE is 1/1_000_000), then to stream time base
        let timestamp_us = timestamp_ns / 1_000;
        let pts = timestamp_us.rescale(ffmpeg::rescale::TIME_BASE, self.video_time_base);
        packet.set_pts(Some(pts));
        packet.set_dts(Some(pts));
        packet.set_stream(self.video_stream_index);

        if frame.is_keyframe {
            packet.set_flags(ffmpeg::codec::packet::Flags::KEY);
        }

        packet
            .write_interleaved(&mut self.output_context)
            .map_err(|e| {
                StreamError::Configuration(format!("Failed to write video packet: {e}"))
            })?;

        Ok(())
    }

    /// Write an encoded audio frame.
    pub fn write_audio(&mut self, frame: &Encodedaudioframe) -> Result<()> {
        let stream_index = self.audio_stream_index.ok_or_else(|| {
            StreamError::Configuration("No audio stream configured in muxer".into())
        })?;

        let audio_time_base = self
            .audio_time_base
            .ok_or_else(|| StreamError::Configuration("No audio time base configured".into()))?;

        let mut packet = ffmpeg::Packet::copy(&frame.data);

        // Convert nanoseconds to microseconds (TIME_BASE is 1/1_000_000), then to stream time base
        let timestamp_ns: i64 = frame.timestamp_ns.parse().unwrap_or(0);
        let timestamp_us = timestamp_ns / 1_000;
        let pts = timestamp_us.rescale(ffmpeg::rescale::TIME_BASE, audio_time_base);
        packet.set_pts(Some(pts));
        packet.set_dts(Some(pts));
        packet.set_stream(stream_index);

        packet
            .write_interleaved(&mut self.output_context)
            .map_err(|e| {
                StreamError::Configuration(format!("Failed to write audio packet: {e}"))
            })?;

        Ok(())
    }

    /// Finalize and close the MP4 file.
    pub fn finalize(&mut self) -> Result<()> {
        if self.header_written {
            self.output_context.write_trailer().map_err(|e| {
                StreamError::Configuration(format!("Failed to write MP4 trailer: {e}"))
            })?;
            self.header_written = false;
        }
        Ok(())
    }

    /// Get the muxer configuration.
    pub fn config(&self) -> &Mp4MuxerConfig {
        &self.config
    }
}

impl Drop for FFmpegMuxer {
    fn drop(&mut self) {
        if self.header_written {
            if let Err(e) = self.output_context.write_trailer() {
                tracing::error!("Failed to write MP4 trailer on drop: {e}");
            }
            self.header_written = false;
        }
    }
}

// FFmpegMuxer is Send because FFmpeg context is used from a single thread
unsafe impl Send for FFmpegMuxer {}
