// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::VideoFrame;
use streamlib_plugin_sdk::sdk::context::{GpuContextLimitedAccess, RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib_plugin_sdk::sdk::error::{Error, Result};
use streamlib_plugin_sdk::sdk::processors::ReactiveProcessor;

use std::io::Write;
use std::process::{Child, Command, Stdio};

#[streamlib_plugin_sdk::sdk::processor("LinuxMp4Writer")]
pub struct LinuxMp4WriterProcessor {
    gpu_context: Option<GpuContextLimitedAccess>,

    /// ffmpeg child process (spawned on first frame).
    ffmpeg_process: Option<Child>,

    /// Frames received counter.
    frames_received: u64,
}

impl ReactiveProcessor for LinuxMp4WriterProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        tracing::info!(
            "[LinuxMp4Writer] Initialized (output: {}, config fps: {})",
            self.config.output_path,
            self.config.fps,
        );
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if let Some(mut child) = self.ffmpeg_process.take() {
            // Closing stdin signals ffmpeg that input is done.
            drop(child.stdin.take());

            let output = child.wait_with_output().map_err(|e| {
                Error::Runtime(format!("Failed to wait for ffmpeg: {e}"))
            })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(Error::Runtime(format!(
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

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("video_in") {
            return Ok(());
        }
        let frame: VideoFrame = self.inputs.read("video_in")?;

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| Error::Runtime("GPU context not initialized".into()))?;

        let pixel_buffer = gpu_ctx.resolve_pixel_buffer_by_surface_id(&frame.surface_id)?;
        let raw_ptr = pixel_buffer.plane_base_address(0);
        let frame_byte_size = pixel_buffer.plane_size(0) as usize;
        if raw_ptr.is_null() || frame_byte_size == 0 {
            return Err(Error::Runtime(
                "VideoFrame pixel buffer has no mapped plane data".into(),
            ));
        }
        let raw_data = unsafe { std::slice::from_raw_parts(raw_ptr, frame_byte_size) };

        // ffmpeg spawns lazily on the first frame so width/height/fps come from the frame, not config.
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

            // Silent audio track: fixed duration when configured; otherwise -shortest trims to video length when stdin closes.
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
                .map_err(|e| Error::Runtime(format!("Failed to spawn ffmpeg: {e}")))?;

            self.ffmpeg_process = Some(child);
        }

        let child = self.ffmpeg_process.as_mut().unwrap();
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            Error::Runtime("ffmpeg stdin not available".into())
        })?;

        stdin.write_all(raw_data).map_err(|e| {
            Error::Runtime(format!("Failed to write frame to ffmpeg: {e}"))
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
