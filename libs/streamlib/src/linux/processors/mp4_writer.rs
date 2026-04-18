// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Linux MP4 Writer Processor
//
// Accumulates encoded H.264/H.265 NAL units during processing, then on
// teardown writes a raw bitstream file and muxes it into an MP4 container
// with a silent audio track via ffmpeg. The silent audio track makes the
// MP4 compatible with platforms that require audio (e.g., Telegram).

use crate::_generated_::Encodedvideoframe;
use crate::core::{Result, RuntimeContext, StreamError};

use std::io::Write;

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.linux_mp4_writer")]
pub struct LinuxMp4WriterProcessor {
    /// Accumulated raw bitstream (NAL units in Annex B format).
    bitstream: Vec<u8>,

    /// Frames received counter.
    frames_received: u64,

    /// FPS from first encoded frame (overrides config if present).
    frame_derived_fps: Option<u32>,
}

impl crate::core::ReactiveProcessor for LinuxMp4WriterProcessor::Processor {
    async fn setup(&mut self, _ctx: RuntimeContext) -> Result<()> {
        tracing::info!(
            "[LinuxMp4Writer] Initialized (output: {}, fps: {}, codec: {})",
            self.config.output_path,
            self.config.fps,
            self.config.codec.as_deref().unwrap_or("h264")
        );
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            frames = self.frames_received,
            bitstream_bytes = self.bitstream.len(),
            "[LinuxMp4Writer] Muxing to MP4..."
        );

        if self.bitstream.is_empty() {
            tracing::warn!("[LinuxMp4Writer] No frames received, skipping MP4 creation");
            return Ok(());
        }

        let codec = self.config.codec.as_deref().unwrap_or("h264");
        let extension = match codec {
            "hevc" | "h265" => "h265",
            _ => "h264",
        };

        // Use frame-derived fps if available (from camera via encoder pass-through),
        // otherwise fall back to config fps.
        let fps = self.frame_derived_fps.unwrap_or(self.config.fps);
        if self.frame_derived_fps.is_some() && self.frame_derived_fps != Some(self.config.fps) {
            tracing::info!(
                "[LinuxMp4Writer] Using frame-derived fps ({}) instead of config fps ({})",
                fps, self.config.fps
            );
        }

        // Write raw bitstream to temp file.
        let raw_path = std::env::temp_dir().join(format!("streamlib_mp4writer.{extension}"));
        {
            let mut raw_file = std::fs::File::create(&raw_path).map_err(|e| {
                StreamError::Runtime(format!("Failed to create temp bitstream file: {e}"))
            })?;
            raw_file.write_all(&self.bitstream).map_err(|e| {
                StreamError::Runtime(format!("Failed to write bitstream: {e}"))
            })?;
        }

        let duration_secs = self.config.duration_secs.unwrap_or(
            (self.frames_received as u32).checked_div(fps).unwrap_or(10)
        );

        // Mux to MP4 with silent audio track via ffmpeg.
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-fflags", "+genpts",
                "-framerate", &fps.to_string(),
                "-i", raw_path.to_str().unwrap(),
                "-f", "lavfi",
                "-t", &duration_secs.to_string(),
                "-i", &format!("anullsrc=r=48000:cl=stereo:d={duration_secs}"),
                "-c:v", "copy",
                "-r", &fps.to_string(),
                "-c:a", "aac",
                "-shortest",
                "-movflags", "+faststart",
                &self.config.output_path,
            ])
            .output()
            .map_err(|e| StreamError::Runtime(format!("Failed to run ffmpeg: {e}")))?;

        let _ = std::fs::remove_file(&raw_path);

        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            return Err(StreamError::Runtime(format!(
                "ffmpeg MP4 mux failed: {stderr}"
            )));
        }

        tracing::info!(
            "[LinuxMp4Writer] MP4 written to {}",
            self.config.output_path
        );

        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        if !self.inputs.has_data("encoded_video_in") {
            return Ok(());
        }
        let frame: Encodedvideoframe = self.inputs.read("encoded_video_in")?;

        // Capture fps from the first encoded frame (set by camera via encoder pass-through).
        if self.frame_derived_fps.is_none() {
            if let Some(fps) = frame.fps {
                tracing::info!("[LinuxMp4Writer] Using fps from encoded frame: {}", fps);
                self.frame_derived_fps = Some(fps);
            }
        }

        self.bitstream.extend_from_slice(&frame.data);
        self.frames_received += 1;

        if self.frames_received == 1 {
            tracing::info!("[LinuxMp4Writer] First encoded frame received");
        } else if self.frames_received % 300 == 0 {
            tracing::info!(
                frames = self.frames_received,
                bytes = self.bitstream.len(),
                "[LinuxMp4Writer] Progress"
            );
        }

        Ok(())
    }
}
