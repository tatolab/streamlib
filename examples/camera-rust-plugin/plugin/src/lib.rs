// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use streamlib::_generated_::Videoframe;
use streamlib::core::rhi::PixelFormat;
use streamlib::core::{GpuContext, ManualProcessor, Result, RuntimeContext, StreamError};
use streamlib_plugin_abi::export_plugin;

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferLockBaseAddress(pixel_buffer: *mut c_void, lock_flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pixel_buffer: *mut c_void, lock_flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(pixel_buffer: *mut c_void) -> *mut c_void;
    fn CVPixelBufferGetBytesPerRow(pixel_buffer: *mut c_void) -> usize;
}

#[streamlib::processor("schemas/processors/grayscale.yaml")]
pub struct GrayscaleProcessor {
    gpu_context: Option<GpuContext>,
    running: Arc<AtomicBool>,
    processing_thread: Option<JoinHandle<()>>,
}

impl ManualProcessor for GrayscaleProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.gpu_context = Some(ctx.gpu.clone());
        self.running = Arc::new(AtomicBool::new(false));
        tracing::info!("GrayscaleProcessor: setup complete");
        std::future::ready(Ok(()))
    }

    fn start(&mut self) -> Result<()> {
        let inputs = std::mem::take(&mut self.inputs);
        let outputs = std::mem::take(&mut self.outputs);
        let gpu = self
            .gpu_context
            .clone()
            .ok_or_else(|| StreamError::Configuration("GpuContext not initialized".into()))?;
        let running = Arc::clone(&self.running);

        running.store(true, Ordering::Release);

        let handle = std::thread::Builder::new()
            .name("grayscale-processing".into())
            .spawn(move || {
                tracing::info!("GrayscaleProcessor: processing thread started");

                while running.load(Ordering::Acquire) {
                    if !inputs.has_data("video_in") {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        continue;
                    }

                    let frame: Videoframe = match inputs.read("video_in") {
                        Ok(f) => f,
                        Err(e) => {
                            tracing::warn!("GrayscaleProcessor: failed to read frame: {}", e);
                            continue;
                        }
                    };

                    let input_buffer = match gpu.check_out_surface(&frame.surface_id) {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::warn!("GrayscaleProcessor: check_out_surface failed: {}", e);
                            continue;
                        }
                    };

                    let w = input_buffer.width;
                    let h = input_buffer.height;

                    let (pool_id, output_buffer) =
                        match gpu.acquire_pixel_buffer(w, h, PixelFormat::Bgra32) {
                            Ok(r) => r,
                            Err(e) => {
                                tracing::warn!(
                                    "GrayscaleProcessor: acquire_pixel_buffer failed: {}",
                                    e
                                );
                                continue;
                            }
                        };

                    let input_ptr = input_buffer.as_ptr();
                    let output_ptr = output_buffer.as_ptr();

                    // Lock both buffers for CPU access (kCVPixelBufferLock_ReadOnly = 1)
                    let lock_result = unsafe { CVPixelBufferLockBaseAddress(input_ptr, 1) };
                    if lock_result != 0 {
                        tracing::warn!(
                            "GrayscaleProcessor: failed to lock input buffer: {}",
                            lock_result
                        );
                        continue;
                    }

                    let lock_result = unsafe { CVPixelBufferLockBaseAddress(output_ptr, 0) };
                    if lock_result != 0 {
                        unsafe { CVPixelBufferUnlockBaseAddress(input_ptr, 1) };
                        tracing::warn!(
                            "GrayscaleProcessor: failed to lock output buffer: {}",
                            lock_result
                        );
                        continue;
                    }

                    let input_base = unsafe { CVPixelBufferGetBaseAddress(input_ptr) };
                    let input_bytes_per_row = unsafe { CVPixelBufferGetBytesPerRow(input_ptr) };
                    let output_base = unsafe { CVPixelBufferGetBaseAddress(output_ptr) };
                    let output_bytes_per_row = unsafe { CVPixelBufferGetBytesPerRow(output_ptr) };

                    // Grayscale conversion (BGRA order): gray = 0.114*B + 0.587*G + 0.299*R
                    for row in 0..h as usize {
                        let in_row_ptr =
                            unsafe { (input_base as *const u8).add(row * input_bytes_per_row) };
                        let out_row_ptr =
                            unsafe { (output_base as *mut u8).add(row * output_bytes_per_row) };

                        for col in 0..w as usize {
                            let pixel_offset = col * 4;
                            unsafe {
                                let b = *in_row_ptr.add(pixel_offset) as f32;
                                let g = *in_row_ptr.add(pixel_offset + 1) as f32;
                                let r = *in_row_ptr.add(pixel_offset + 2) as f32;

                                let gray = (0.114 * b + 0.587 * g + 0.299 * r) as u8;

                                *out_row_ptr.add(pixel_offset) = gray; // B
                                *out_row_ptr.add(pixel_offset + 1) = gray; // G
                                *out_row_ptr.add(pixel_offset + 2) = gray; // R
                                *out_row_ptr.add(pixel_offset + 3) = 255; // A
                            }
                        }
                    }

                    // Unlock both buffers
                    unsafe {
                        CVPixelBufferUnlockBaseAddress(output_ptr, 0);
                        CVPixelBufferUnlockBaseAddress(input_ptr, 1);
                    }

                    // Forward frame with new surface_id
                    let output_frame = Videoframe {
                        surface_id: pool_id.to_string(),
                        ..frame
                    };
                    if let Err(e) = outputs.write("video_out", &output_frame) {
                        tracing::warn!("GrayscaleProcessor: failed to write output: {}", e);
                    }
                }

                tracing::info!("GrayscaleProcessor: processing thread stopped");
            })
            .map_err(|e| {
                StreamError::Configuration(format!("Failed to spawn processing thread: {}", e))
            })?;

        self.processing_thread = Some(handle);
        tracing::info!("GrayscaleProcessor: started");
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.running.store(false, Ordering::Release);

        if let Some(handle) = self.processing_thread.take() {
            handle
                .join()
                .map_err(|_| StreamError::Runtime("Processing thread panicked".into()))?;
        }

        tracing::info!("GrayscaleProcessor: stopped");
        Ok(())
    }
}

export_plugin!(GrayscaleProcessor::Processor);
