// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// JPEG Bytes Source Processor
//
// Reads a single JPEG file from disk at setup() and republishes its bytes
// as EncodedJpegFrame messages on a background thread at a configurable
// rate. The same bytes are reissued each tick with monotonically
// increasing frame_number / timestamp, simulating what a real producer
// (UDP depayloader, decoded MJPEG container, etc.) would emit.
//
// Use this for end-to-end testing of any processor that consumes
// EncodedJpegFrame — most notably @tatolab/jpeg::JpegDecoder.

use crate::_generated_::EncodedJpegFrame;
use streamlib_plugin_sdk::sdk::error::{Error, Result};
use streamlib_plugin_sdk::sdk::iceoryx2::OutputWriter;
use streamlib_plugin_sdk::sdk::processors::ManualProcessor;
use streamlib_plugin_sdk::sdk::context::RuntimeContextFullAccess;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

const DEFAULT_FPS: u32 = 10;

#[streamlib_plugin_sdk::sdk::processor(
    "@tatolab/debug-utilities/JpegBytesSource@1.0.0",
    execution = manual,
    config = crate::_generated_::JpegBytesSourceConfig,
    output("encoded_jpeg", "@tatolab/jpeg/EncodedJpegFrame@1.0.7"),
)]
pub struct JpegBytesSourceProcessor {
    /// JPEG bytes loaded from disk at setup time.
    jpeg_bytes: Option<Arc<Vec<u8>>>,
    is_running: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    source_thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl ManualProcessor for JpegBytesSourceProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let bytes = std::fs::read(&self.config.file_path).map_err(|e| {
            Error::Configuration(format!(
                "JpegBytesSource: failed to read {}: {e}",
                self.config.file_path
            ))
        })?;
        tracing::info!(
            path = %self.config.file_path,
            bytes = bytes.len(),
            "[JpegBytesSource] Loaded fixture JPEG"
        );
        self.jpeg_bytes = Some(Arc::new(bytes));
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let frames = self.frame_counter.load(Ordering::Relaxed);
        tracing::info!("[JpegBytesSource] Teardown ({frames} frames emitted)");
        self.is_running.store(false, Ordering::Release);
        if let Some(handle) = self.source_thread_handle.take() {
            let _ = handle.join();
        }
        self.jpeg_bytes.take();
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let bytes = self.jpeg_bytes.clone().ok_or_else(|| {
            Error::Configuration("JpegBytesSource: setup() did not load JPEG bytes".into())
        })?;

        let fps = self.config.fps.unwrap_or(DEFAULT_FPS).max(1);
        let frame_count = self.config.frame_count.unwrap_or(0);

        self.is_running.store(true, Ordering::Release);

        let is_running = Arc::clone(&self.is_running);
        let frame_counter = Arc::clone(&self.frame_counter);
        let outputs: OutputWriter = self.outputs.clone();

        let handle = std::thread::Builder::new()
            .name("jpeg-bytes-source".into())
            .spawn(move || {
                source_thread_loop(
                    bytes,
                    fps,
                    frame_count,
                    is_running,
                    frame_counter,
                    outputs,
                );
            })
            .map_err(|e| {
                Error::Configuration(format!("JpegBytesSource: failed to spawn thread: {e}"))
            })?;

        self.source_thread_handle = Some(handle);
        tracing::info!(fps = fps, frame_count = frame_count, "[JpegBytesSource] Started");
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.is_running.store(false, Ordering::Release);
        if let Some(handle) = self.source_thread_handle.take() {
            let _ = handle.join();
        }
        tracing::info!("[JpegBytesSource] Stopped");
        Ok(())
    }
}

fn source_thread_loop(
    bytes: Arc<Vec<u8>>,
    fps: u32,
    frame_count: u32,
    is_running: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    outputs: OutputWriter,
) {
    let frame_interval_ns = 1_000_000_000i64 / fps as i64;
    let clock_start = std::time::Instant::now();
    let mut frame_idx: u64 = 0;

    loop {
        if !is_running.load(Ordering::Acquire) {
            break;
        }
        if frame_count != 0 && frame_idx >= frame_count as u64 {
            break;
        }

        let timestamp_ns = clock_start.elapsed().as_nanos() as i64;
        let frame = EncodedJpegFrame {
            data: (*bytes).clone(),
            timestamp_ns: timestamp_ns.to_string(),
            frame_number: frame_idx.to_string(),
            fps: Some(fps),
        };

        if let Err(e) = outputs.write("encoded_jpeg", &frame) {
            tracing::error!("[JpegBytesSource] Failed to write frame: {e}");
            break;
        }

        frame_idx += 1;
        frame_counter.store(frame_idx, Ordering::Relaxed);

        // Pace at the requested FPS so downstream mailboxes don't drown.
        let target_elapsed = std::time::Duration::from_nanos(frame_idx * frame_interval_ns as u64);
        let actual_elapsed = clock_start.elapsed();
        if actual_elapsed < target_elapsed {
            std::thread::sleep(target_elapsed - actual_elapsed);
        }
    }

    is_running.store(false, Ordering::Release);
    tracing::info!(
        "[JpegBytesSource] Source thread done ({} frames)",
        frame_counter.load(Ordering::Relaxed)
    );
}
