// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// BGRA File Source Processor
//
// Reads a raw BGRA file frame-by-frame and publishes each frame as a
// Videoframe through the processor graph. Used for testing encode/decode
// pipelines with pre-generated fixture files.

use crate::_generated_::Videoframe;
use crate::core::context::GpuContextLimitedAccess;
use crate::core::rhi::PixelFormat;
use crate::core::{Result, RuntimeContextFullAccess, StreamError};
use crate::iceoryx2::OutputWriter;

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.bgra_file_source")]
pub struct BgraFileSourceProcessor {
    gpu_context: Option<GpuContextLimitedAccess>,
    is_running: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    source_thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl crate::core::ManualProcessor for BgraFileSourceProcessor::Processor {
    fn setup(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        tracing::info!(
            "[BgraFileSource] Setup (file: {}, {}x{}@{}fps, {} frames)",
            self.config.file_path,
            self.config.width,
            self.config.height,
            self.config.fps,
            self.config.frame_count
        );
        std::future::ready(Ok(()))
    }

    fn teardown(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let frames = self.frame_counter.load(Ordering::Relaxed);
        tracing::info!("[BgraFileSource] Teardown ({frames} frames streamed)");
        self.is_running.store(false, Ordering::Release);
        if let Some(handle) = self.source_thread_handle.take() {
            let _ = handle.join();
        }
        std::future::ready(Ok(()))
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let gpu_context = self.gpu_context.clone().ok_or_else(|| {
            StreamError::Configuration("GPU context not initialized".into())
        })?;

        self.is_running.store(true, Ordering::Release);

        let is_running = Arc::clone(&self.is_running);
        let frame_counter = Arc::clone(&self.frame_counter);
        let outputs: Arc<OutputWriter> = self.outputs.clone();
        let file_path = self.config.file_path.clone();
        let width = self.config.width;
        let height = self.config.height;
        let fps = self.config.fps;
        let frame_count = self.config.frame_count;

        let handle = std::thread::Builder::new()
            .name("bgra-file-source".into())
            .spawn(move || {
                source_thread_loop(
                    file_path,
                    width,
                    height,
                    fps,
                    frame_count,
                    is_running,
                    frame_counter,
                    outputs,
                    gpu_context,
                );
            })
            .map_err(|e| {
                StreamError::Configuration(format!("Failed to spawn source thread: {e}"))
            })?;

        self.source_thread_handle = Some(handle);
        tracing::info!("[BgraFileSource] Streaming started");
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.is_running.store(false, Ordering::Release);
        if let Some(handle) = self.source_thread_handle.take() {
            let _ = handle.join();
        }
        tracing::info!("[BgraFileSource] Stopped");
        Ok(())
    }
}

fn source_thread_loop(
    file_path: String,
    width: u32,
    height: u32,
    fps: u32,
    frame_count: u32,
    is_running: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    outputs: Arc<OutputWriter>,
    gpu_context: GpuContextLimitedAccess,
) {
    let frame_size = (width * height * 4) as usize;

    let mut file = match std::fs::File::open(&file_path) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("[BgraFileSource] Failed to open {file_path}: {e}");
            return;
        }
    };

    let mut frame_buf = vec![0u8; frame_size];
    let frame_interval_ns = 1_000_000_000i64 / fps as i64;
    let clock_start = std::time::Instant::now();

    for frame_idx in 0..frame_count {
        if !is_running.load(Ordering::Acquire) {
            break;
        }

        if file.read_exact(&mut frame_buf).is_err() {
            tracing::warn!("[BgraFileSource] Reached end of file at frame {frame_idx}");
            break;
        }

        // Acquire pixel buffer and write BGRA data
        let (pool_id, pixel_buffer) =
            match gpu_context.acquire_pixel_buffer(width, height, PixelFormat::Rgba32) {
                Ok(result) => result,
                Err(e) => {
                    tracing::error!("[BgraFileSource] Failed to acquire pixel buffer: {e}");
                    break;
                }
            };

        let dst_ptr = pixel_buffer.buffer_ref().inner.mapped_ptr();
        unsafe {
            std::ptr::copy_nonoverlapping(frame_buf.as_ptr(), dst_ptr, frame_size);
        }

        // Upload the pixel buffer as a GPU texture so downstream encoder
        // processors (which read via `resolve_videoframe_texture`) can
        // consume the frame. Without this, the encoder fails with
        // "No texture or pixel buffer found for surface_id ...".
        let surface_id = pool_id.to_string();
        if let Err(e) =
            gpu_context.upload_pixel_buffer_as_texture(&surface_id, &pixel_buffer, width, height)
        {
            tracing::error!("[BgraFileSource] Failed to upload frame texture: {e}");
            break;
        }

        let timestamp_ns =
            clock_start.elapsed().as_nanos() as i64 + frame_idx as i64 * frame_interval_ns;

        let video_frame = Videoframe {
            surface_id,
            width,
            height,
            timestamp_ns: timestamp_ns.to_string(),
            frame_index: frame_idx.to_string(),
            fps: Some(fps),
        };

        if let Err(e) = outputs.write("video", &video_frame) {
            tracing::error!("[BgraFileSource] Failed to write frame: {e}");
            break;
        }

        frame_counter.store(frame_idx as u64 + 1, Ordering::Relaxed);

        // Throttle to real-time FPS to avoid overflowing downstream mailboxes.
        // The encoder processes frames reactively — if we blast faster than it
        // can encode, the mailbox fills and frames are lost.
        let target_elapsed =
            std::time::Duration::from_nanos((frame_idx as u64 + 1) * frame_interval_ns as u64);
        let actual_elapsed = clock_start.elapsed();
        if actual_elapsed < target_elapsed {
            std::thread::sleep(target_elapsed - actual_elapsed);
        }

        if frame_idx == 0 {
            tracing::info!("[BgraFileSource] First frame published");
        } else if (frame_idx + 1) % (fps * 1) == 0 {
            tracing::info!(
                "[BgraFileSource] {}/{} frames ({:.1}s)",
                frame_idx + 1,
                frame_count,
                (frame_idx + 1) as f64 / fps as f64
            );
        }
    }

    is_running.store(false, Ordering::Release);
    tracing::info!(
        "[BgraFileSource] Source thread done ({} frames)",
        frame_counter.load(Ordering::Relaxed)
    );
}
