// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Linux MP4 Writer Processor
//
// Accepts decoded Videoframe (raw RGBA pixels), pipes them to ffmpeg for
// encoding + muxing into an MP4 container with a silent audio track.
// The writer knows nothing about codecs — ffmpeg handles encoding.

use crate::_generated_::Videoframe;
use crate::core::context::GpuContext;
use crate::core::{Result, RuntimeContext, StreamError};

use std::io::Write;
use std::process::{Child, Command, Stdio};

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.linux_mp4_writer")]
pub struct LinuxMp4WriterProcessor {
    /// GPU context for resolving Videoframe pixel buffers.
    gpu_context: Option<GpuContext>,

    /// ffmpeg child process (spawned on first frame).
    ffmpeg_process: Option<Child>,

    /// Frames received counter.
    frames_received: u64,
}

impl crate::core::ReactiveProcessor for LinuxMp4WriterProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());
        tracing::info!(
            "[LinuxMp4Writer] Initialized (output: {}, config fps: {})",
            self.config.output_path,
            self.config.fps,
        );
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        if let Some(mut child) = self.ffmpeg_process.take() {
            // Close stdin to signal ffmpeg that input is done.
            drop(child.stdin.take());

            let output = child.wait_with_output().map_err(|e| {
                StreamError::Runtime(format!("Failed to wait for ffmpeg: {e}"))
            })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(StreamError::Runtime(format!(
                    "ffmpeg exited with status {}: {stderr}", output.status
                )));
            }

            tracing::info!(
                frames = self.frames_received,
                "[LinuxMp4Writer] MP4 written to {}",
                self.config.output_path
            );
        } else {
            tracing::warn!("[LinuxMp4Writer] No frames received, skipping MP4 creation");
        }

        self.gpu_context.take();
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        if !self.inputs.has_data("video_in") {
            return Ok(());
        }
        let frame: Videoframe = self.inputs.read("video_in")?;

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("GPU context not initialized".into()))?;

        // Resolve Videoframe to pixel buffer for decoded NV12 data.
        // Decoder outputs NV12 (Y + UV = W*H*3/2). ffmpeg converts to display RGB
        // internally — same as any consumer video player.
        let pixel_buffer = gpu_ctx.resolve_videoframe_buffer(&frame)?;
        let raw_ptr = pixel_buffer.buffer_ref().inner.mapped_ptr();
        let frame_byte_size = (frame.width * frame.height * 4) as usize;
        let raw_data = unsafe { std::slice::from_raw_parts(raw_ptr, frame_byte_size) };

        // Lazy init: spawn ffmpeg on first frame so we know width/height/fps.
        if self.ffmpeg_process.is_none() {
            let fps = frame.fps.unwrap_or(self.config.fps);
            let width = frame.width;
            let height = frame.height;

            tracing::info!(
                "[LinuxMp4Writer] First frame: {}x{}, {}fps{} — spawning ffmpeg",
                width, height, fps,
                if frame.fps.is_some() { " from camera" } else { " from config" }
            );

            let duration_secs = self.config.duration_secs.map(|d| d.to_string());
            let fps_str = fps.to_string();
            let size_str = format!("{width}x{height}");

            let mut args: Vec<&str> = vec![
                "-y",
                "-f", "rawvideo",
                "-pix_fmt", "rgba",
                "-s", &size_str,
                "-r", &fps_str,
                "-i", "pipe:0",
            ];

            // Silent audio track — use fixed duration if configured, otherwise
            // -shortest will trim to video length when stdin closes.
            if let Some(ref dur) = duration_secs {
                args.extend_from_slice(&["-f", "lavfi", "-t", dur,
                    "-i", "anullsrc=r=48000:cl=stereo"]);
            } else {
                args.extend_from_slice(&["-f", "lavfi",
                    "-i", "anullsrc=r=48000:cl=stereo"]);
            }

            args.extend_from_slice(&[
                "-c:v", "mpeg4",
                "-q:v", "1",
                "-c:a", "aac",
                "-shortest",
                "-movflags", "+faststart",
                &self.config.output_path,
            ]);

            let child = Command::new("ffmpeg")
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| StreamError::Runtime(format!("Failed to spawn ffmpeg: {e}")))?;

            self.ffmpeg_process = Some(child);
        }

        // Write raw RGBA frame to ffmpeg's stdin.
        let child = self.ffmpeg_process.as_mut().unwrap();
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            StreamError::Runtime("ffmpeg stdin not available".into())
        })?;

        stdin.write_all(raw_data).map_err(|e| {
            StreamError::Runtime(format!("Failed to write frame to ffmpeg: {e}"))
        })?;

        self.frames_received += 1;

        if self.frames_received == 1 {
            tracing::info!("[LinuxMp4Writer] First frame written to ffmpeg");
        } else if self.frames_received % 300 == 0 {
            tracing::info!(
                frames = self.frames_received,
                "[LinuxMp4Writer] Progress"
            );
        }

        Ok(())
    }
}
