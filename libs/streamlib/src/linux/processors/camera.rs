// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::rhi::PixelFormat;
use crate::core::{GpuContext, Result, RuntimeContext, StreamError};
use crate::iceoryx2::OutputWriter;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use v4l::buffer::Type;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::FourCC;

/// Number of V4L2 mmap buffers to request.
const V4L2_BUFFER_COUNT: u32 = 4;

/// Default V4L2 device path.
const DEFAULT_DEVICE_PATH: &str = "/dev/video0";

#[derive(Debug, Clone)]
pub struct LinuxCameraDevice {
    pub id: String,
    pub name: String,
}

#[crate::processor("com.tatolab.camera")]
pub struct LinuxCameraProcessor {
    camera_name: String,
    gpu_context: Option<GpuContext>,
    is_capturing: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    capture_thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl crate::core::ManualProcessor for LinuxCameraProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.gpu_context = Some(ctx.gpu.clone());
        tracing::info!("Camera: setup() complete");
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        let frame_count = self.frame_counter.load(Ordering::Relaxed);
        tracing::info!(
            "Camera {}: Teardown (generated {} frames)",
            self.camera_name,
            frame_count
        );
        self.is_capturing.store(false, Ordering::Release);
        if let Some(handle) = self.capture_thread_handle.take() {
            let _ = handle.join();
        }
        std::future::ready(Ok(()))
    }

    fn start(&mut self) -> Result<()> {
        let gpu_context = self.gpu_context.clone().ok_or_else(|| {
            StreamError::Configuration("GPU context not initialized. Call setup() first.".into())
        })?;

        let device_path = self
            .config
            .device_id
            .clone()
            .unwrap_or_else(|| DEFAULT_DEVICE_PATH.to_string());

        // Open the V4L2 device
        let mut dev = v4l::Device::with_path(&device_path).map_err(|e| {
            StreamError::Configuration(format!("Failed to open V4L2 device '{}': {}", device_path, e))
        })?;

        // Query device capabilities
        let caps = dev.query_caps().map_err(|e| {
            StreamError::Configuration(format!("Failed to query device capabilities: {}", e))
        })?;
        self.camera_name = caps.card.clone();
        tracing::info!(
            "Camera: opened '{}' (driver: {}, bus: {})",
            caps.card,
            caps.driver,
            caps.bus
        );

        // Get current format to read the device's native resolution
        let current_fmt = dev.format().map_err(|e| {
            StreamError::Configuration(format!("Failed to read current format: {}", e))
        })?;

        // Set format to NV12 at the current resolution (preferred for CamLink 4K)
        let mut fmt = current_fmt;
        fmt.fourcc = FourCC::new(b"NV12");
        let fmt = dev.set_format(&fmt).map_err(|e| {
            StreamError::Configuration(format!("Failed to set NV12 format: {}", e))
        })?;

        let capture_width = fmt.width;
        let capture_height = fmt.height;
        let capture_fourcc = fmt.fourcc;

        tracing::info!(
            "Camera {}: capturing {}x{} {:?}",
            self.camera_name,
            capture_width,
            capture_height,
            capture_fourcc
        );

        // Create mmap stream with V4L2_BUFFER_COUNT buffers
        let stream =
            v4l::io::mmap::Stream::with_buffers(&mut dev, Type::VideoCapture, V4L2_BUFFER_COUNT)
                .map_err(|e| {
                    StreamError::Configuration(format!(
                        "Failed to create V4L2 mmap stream: {}",
                        e
                    ))
                })?;

        self.is_capturing.store(true, Ordering::Release);

        let is_capturing = Arc::clone(&self.is_capturing);
        let frame_counter = Arc::clone(&self.frame_counter);
        let outputs: Arc<OutputWriter> = self.outputs.clone();
        let camera_name = self.camera_name.clone();

        let handle = std::thread::Builder::new()
            .name(format!("v4l2-capture-{}", device_path))
            .spawn(move || {
                capture_thread_loop(
                    stream,
                    is_capturing,
                    frame_counter,
                    outputs,
                    gpu_context,
                    camera_name,
                    capture_width,
                    capture_height,
                    capture_fourcc,
                );
            })
            .map_err(|e| {
                StreamError::Configuration(format!("Failed to spawn capture thread: {}", e))
            })?;

        self.capture_thread_handle = Some(handle);

        tracing::info!(
            "Camera {}: V4L2 capture started ({}x{} NV12, {} mmap buffers)",
            self.camera_name,
            capture_width,
            capture_height,
            V4L2_BUFFER_COUNT
        );
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.is_capturing.store(false, Ordering::Release);

        if let Some(handle) = self.capture_thread_handle.take() {
            let _ = handle.join();
        }

        tracing::info!(
            "Camera {}: Stopped ({} frames)",
            self.camera_name,
            self.frame_counter.load(Ordering::Relaxed)
        );
        Ok(())
    }
}

/// V4L2 capture thread main loop.
///
/// Polls for frames from the mmap stream, converts NV12 to BGRA,
/// writes to a Vulkan pixel buffer, and publishes via OutputWriter.
fn capture_thread_loop(
    mut stream: v4l::io::mmap::Stream,
    is_capturing: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    outputs: Arc<OutputWriter>,
    gpu_context: GpuContext,
    camera_name: String,
    width: u32,
    height: u32,
    fourcc: FourCC,
) {
    while is_capturing.load(Ordering::Acquire) {
        let (buf, meta) = match stream.next() {
            Ok(frame) => frame,
            Err(e) => {
                if is_capturing.load(Ordering::Acquire) {
                    eprintln!("[Camera {}] V4L2 stream error: {}", camera_name, e);
                }
                break;
            }
        };

        if !is_capturing.load(Ordering::Acquire) {
            break;
        }

        let frame_num = frame_counter.fetch_add(1, Ordering::Relaxed);

        // Acquire a BGRA pixel buffer from the pool
        let (pool_id, pooled_buffer) =
            match gpu_context.acquire_pixel_buffer(width, height, PixelFormat::Bgra32) {
                Ok(result) => result,
                Err(e) => {
                    if frame_num == 0 {
                        eprintln!(
                            "[Camera {}] Failed to acquire pixel buffer: {}",
                            camera_name, e
                        );
                    }
                    continue;
                }
            };

        // Convert captured frame to BGRA and write into the pixel buffer
        let dst_ptr = pooled_buffer.buffer_ref().inner.mapped_ptr();
        let dst_size = (width * height * 4) as usize;

        let fourcc_bytes = fourcc.repr;
        match &fourcc_bytes {
            b"NV12" => {
                convert_nv12_to_bgra(buf, dst_ptr, dst_size, width, height);
            }
            b"YUYV" => {
                convert_yuyv_to_bgra(buf, dst_ptr, dst_size, width, height);
            }
            _ => {
                if frame_num == 0 {
                    eprintln!(
                        "[Camera {}] Unsupported capture format: {:?}, writing zeros",
                        camera_name, fourcc
                    );
                }
                unsafe {
                    std::ptr::write_bytes(dst_ptr, 0, dst_size);
                }
            }
        }

        let surface_id = pool_id.to_string();
        let timestamp_ns = crate::core::media_clock::MediaClock::now().as_nanos() as i64;

        let ipc_frame = crate::_generated_::Videoframe {
            surface_id,
            width,
            height,
            timestamp_ns: timestamp_ns.to_string(),
            frame_index: frame_num.to_string(),
        };

        if let Err(e) = outputs.write("video", &ipc_frame) {
            eprintln!("[Camera {}] Failed to write frame: {}", camera_name, e);
            continue;
        }
        // _pooled_buffer is dropped here after the write, releasing the ring slot

        if frame_num == 0 {
            eprintln!(
                "[Camera {}] First frame captured (seq={}, {}x{} {:?})",
                camera_name, meta.sequence, width, height, fourcc
            );
        } else if frame_num % 300 == 0 {
            eprintln!("[Camera {}] Frame #{}", camera_name, frame_num);
        }
    }
}

/// Convert NV12 (YUV 4:2:0 bi-planar) to BGRA.
///
/// NV12 layout: Y plane (width*height bytes) followed by interleaved UV plane (width*height/2 bytes).
/// Uses BT.601 full-range conversion coefficients.
fn convert_nv12_to_bgra(src: &[u8], dst_ptr: *mut u8, dst_size: usize, width: u32, height: u32) {
    let y_plane_size = (width * height) as usize;
    let expected_nv12_size = y_plane_size + y_plane_size / 2;

    if src.len() < expected_nv12_size || dst_size < (width * height * 4) as usize {
        return;
    }

    let y_plane = &src[..y_plane_size];
    let uv_plane = &src[y_plane_size..];

    unsafe {
        for row in 0..height {
            for col in 0..width {
                let y_idx = (row * width + col) as usize;
                let uv_idx = ((row / 2) * width + (col & !1)) as usize;

                let y = y_plane[y_idx] as f32;
                let u = uv_plane[uv_idx] as f32 - 128.0;
                let v = uv_plane[uv_idx + 1] as f32 - 128.0;

                let r = (y + 1.402 * v).clamp(0.0, 255.0);
                let g = (y - 0.344136 * u - 0.714136 * v).clamp(0.0, 255.0);
                let b = (y + 1.772 * u).clamp(0.0, 255.0);

                let px_offset = ((row * width + col) * 4) as usize;
                *dst_ptr.add(px_offset) = b as u8;
                *dst_ptr.add(px_offset + 1) = g as u8;
                *dst_ptr.add(px_offset + 2) = r as u8;
                *dst_ptr.add(px_offset + 3) = 255;
            }
        }
    }
}

/// Convert YUYV (YUV 4:2:2 packed) to BGRA.
///
/// YUYV layout: [Y0 U0 Y1 V0] per 2 pixels. Uses BT.601 full-range coefficients.
fn convert_yuyv_to_bgra(src: &[u8], dst_ptr: *mut u8, dst_size: usize, width: u32, height: u32) {
    let expected_yuyv_size = (width * height * 2) as usize;

    if src.len() < expected_yuyv_size || dst_size < (width * height * 4) as usize {
        return;
    }

    unsafe {
        for row in 0..height {
            for col in (0..width).step_by(2) {
                let yuyv_offset = ((row * width + col) * 2) as usize;
                let y0 = src[yuyv_offset] as f32;
                let u = src[yuyv_offset + 1] as f32 - 128.0;
                let y1 = src[yuyv_offset + 2] as f32;
                let v = src[yuyv_offset + 3] as f32 - 128.0;

                // First pixel
                let r0 = (y0 + 1.402 * v).clamp(0.0, 255.0);
                let g0 = (y0 - 0.344136 * u - 0.714136 * v).clamp(0.0, 255.0);
                let b0 = (y0 + 1.772 * u).clamp(0.0, 255.0);

                let px0 = ((row * width + col) * 4) as usize;
                *dst_ptr.add(px0) = b0 as u8;
                *dst_ptr.add(px0 + 1) = g0 as u8;
                *dst_ptr.add(px0 + 2) = r0 as u8;
                *dst_ptr.add(px0 + 3) = 255;

                // Second pixel
                if col + 1 < width {
                    let r1 = (y1 + 1.402 * v).clamp(0.0, 255.0);
                    let g1 = (y1 - 0.344136 * u - 0.714136 * v).clamp(0.0, 255.0);
                    let b1 = (y1 + 1.772 * u).clamp(0.0, 255.0);

                    let px1 = ((row * width + col + 1) * 4) as usize;
                    *dst_ptr.add(px1) = b1 as u8;
                    *dst_ptr.add(px1 + 1) = g1 as u8;
                    *dst_ptr.add(px1 + 2) = r1 as u8;
                    *dst_ptr.add(px1 + 3) = 255;
                }
            }
        }
    }
}

impl LinuxCameraProcessor::Processor {
    /// Enumerate available V4L2 camera devices.
    pub fn list_devices() -> Result<Vec<LinuxCameraDevice>> {
        let mut devices = Vec::new();

        // Scan /dev/video* devices
        for entry in std::fs::read_dir("/dev").map_err(|e| {
            StreamError::Configuration(format!("Failed to read /dev: {}", e))
        })? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };

            if !name.starts_with("video") {
                continue;
            }

            let dev = match v4l::Device::with_path(&path) {
                Ok(d) => d,
                Err(_) => continue,
            };

            let caps = match dev.query_caps() {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Only include devices with video capture capability
            if !caps
                .capabilities
                .contains(v4l::capability::Flags::VIDEO_CAPTURE)
            {
                continue;
            }

            devices.push(LinuxCameraDevice {
                id: path.to_string_lossy().to_string(),
                name: caps.card,
            });
        }

        Ok(devices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::_generated_::CameraConfig;
    use crate::core::GeneratedProcessor;

    #[test]
    fn test_list_devices() {
        let devices = LinuxCameraProcessor::Processor::list_devices();
        assert!(devices.is_ok());

        if let Ok(devices) = devices {
            println!("Found {} V4L2 camera devices:", devices.len());
            for device in &devices {
                println!("  [{}] {}", device.id, device.name);
            }
        }
    }

    #[test]
    fn test_create_default_processor() {
        let config = CameraConfig {
            device_id: None,
            min_fps: None,
            max_fps: None,
        };

        let result = LinuxCameraProcessor::Processor::from_config(config);

        match result {
            Ok(_processor) => {
                println!("Successfully created camera processor from config");
            }
            Err(e) => {
                println!(
                    "Note: Could not create camera processor (may require permissions): {}",
                    e
                );
            }
        }
    }

    #[test]
    #[ignore] // Requires real V4L2 camera hardware - not available in CI
    fn test_capture_single_frame() {
        let mut dev = v4l::Device::with_path(DEFAULT_DEVICE_PATH)
            .expect("Failed to open /dev/video0");

        let caps = dev.query_caps().expect("Failed to query caps");
        println!("Device: {} ({})", caps.card, caps.driver);

        let mut fmt = dev.format().expect("Failed to read format");
        println!("Default format: {}x{} {:?}", fmt.width, fmt.height, fmt.fourcc);

        // Try NV12 first, fall back to YUYV if device is busy or doesn't support NV12
        fmt.fourcc = FourCC::new(b"NV12");
        let fmt = match dev.set_format(&fmt) {
            Ok(f) => f,
            Err(e) => {
                println!("NV12 not available ({}), trying YUYV", e);
                let mut fmt = dev.format().expect("Failed to read format");
                fmt.fourcc = FourCC::new(b"YUYV");
                match dev.set_format(&fmt) {
                    Ok(f) => f,
                    Err(e2) => {
                        println!(
                            "YUYV also not available ({}), skipping test (device likely busy)",
                            e2
                        );
                        return;
                    }
                }
            }
        };
        println!("Capture format: {}x{} {:?}", fmt.width, fmt.height, fmt.fourcc);

        let mut stream =
            v4l::io::mmap::Stream::with_buffers(&mut dev, Type::VideoCapture, 4)
                .expect("Failed to create mmap stream");

        let (buf, meta) = stream.next().expect("Failed to capture frame");

        println!(
            "Captured frame: {} bytes, seq={}, timestamp={}",
            buf.len(),
            meta.sequence,
            meta.timestamp
        );

        assert!(
            !buf.is_empty(),
            "Frame is empty - camera may not be producing frames"
        );

        // Verify frame has non-zero data
        let nonzero_count = buf.iter().filter(|&&b| b != 0).count();
        assert!(
            nonzero_count > 0,
            "Frame is all zeros - camera may not be producing frames"
        );

        println!(
            "Frame validation passed ({} bytes, {} non-zero)",
            buf.len(),
            nonzero_count
        );
    }
}
