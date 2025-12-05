// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/// VideoToolbox H.264 encoder for generating valid test video
///
/// Creates synthetic colored frames and encodes them with hardware H.264 encoder
#[cfg(target_os = "macos")]
use anyhow::{Context, Result};
#[cfg(target_os = "macos")]
use core_foundation::base::TCFType;
#[cfg(target_os = "macos")]
use objc2_core_video::*;
#[cfg(target_os = "macos")]
use objc2_foundation::*;
#[cfg(target_os = "macos")]
use std::ptr::NonNull;
#[cfg(target_os = "macos")]
use std::sync::{Arc, Mutex};

#[cfg(target_os = "macos")]
#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    fn VTCompressionSessionCreate(
        allocator: *const std::ffi::c_void,
        width: i32,
        height: i32,
        codec_type: u32,
        encoder_specification: *const std::ffi::c_void,
        source_image_buffer_attributes: *const std::ffi::c_void,
        compressed_data_allocator: *const std::ffi::c_void,
        output_callback: VTCompressionOutputCallback,
        refcon: *mut std::ffi::c_void,
        compression_session_out: *mut *mut std::ffi::c_void,
    ) -> i32;

    fn VTSessionSetProperty(
        session: *mut std::ffi::c_void,
        key: *const std::ffi::c_void,
        value: *const std::ffi::c_void,
    ) -> i32;

    fn VTCompressionSessionEncodeFrame(
        session: *mut std::ffi::c_void,
        image_buffer: *mut std::ffi::c_void,
        presentation_time_stamp: CMTime,
        duration: CMTime,
        frame_properties: *const std::ffi::c_void,
        source_frame_refcon: *mut std::ffi::c_void,
        info_flags_out: *mut u32,
    ) -> i32;

    fn VTCompressionSessionCompleteFrames(
        session: *mut std::ffi::c_void,
        complete_until_presentation_time_stamp: CMTime,
    ) -> i32;

    fn VTCompressionSessionInvalidate(session: *mut std::ffi::c_void);

    static kVTCompressionPropertyKey_RealTime: *const std::ffi::c_void;
    static kVTCompressionPropertyKey_ProfileLevel: *const std::ffi::c_void;
    static kVTCompressionPropertyKey_H264EntropyMode: *const std::ffi::c_void;
    static kVTCompressionPropertyKey_MaxKeyFrameInterval: *const std::ffi::c_void;
    static kVTCompressionPropertyKey_AverageBitRate: *const std::ffi::c_void;
    static kVTCompressionPropertyKey_ExpectedFrameRate: *const std::ffi::c_void;
    static kVTProfileLevel_H264_Baseline_AutoLevel: *const std::ffi::c_void;
    static kVTH264EntropyMode_CABAC: *const std::ffi::c_void;
}

#[cfg(target_os = "macos")]
type VTCompressionOutputCallback = unsafe extern "C" fn(
    output_callback_refcon: *mut std::ffi::c_void,
    source_frame_refcon: *mut std::ffi::c_void,
    status: i32,
    info_flags: u32,
    sample_buffer: *mut std::ffi::c_void,
);

#[cfg(target_os = "macos")]
pub struct VideoToolboxEncoder {
    session: *mut std::ffi::c_void,
    width: usize,
    height: usize,
    frame_count: u64,
    // Callback receives encoded H.264 NAL units
    output_callback: Arc<Mutex<Box<dyn FnMut(Vec<u8>, bool) + Send>>>,
}

#[cfg(target_os = "macos")]
unsafe impl Send for VideoToolboxEncoder {}

#[cfg(target_os = "macos")]
impl VideoToolboxEncoder {
    pub fn new<F>(width: usize, height: usize, mut callback: F) -> Result<Self>
    where
        F: FnMut(Vec<u8>, bool) + Send + 'static,
    {
        let output_callback = Arc::new(Mutex::new(Box::new(callback) as Box<dyn FnMut(Vec<u8>, bool) + Send>));
        let refcon = Arc::into_raw(output_callback.clone()) as *mut std::ffi::c_void;

        let mut session: *mut std::ffi::c_void = std::ptr::null_mut();

        unsafe {
            // Create compression session
            let status = VTCompressionSessionCreate(
                std::ptr::null(),
                width as i32,
                height as i32,
                0x61766331, // 'avc1' = H.264
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                compression_output_callback,
                refcon,
                &mut session,
            );

            if status != 0 {
                anyhow::bail!("VTCompressionSessionCreate failed: {}", status);
            }

            // Set real-time encoding
            VTSessionSetProperty(
                session,
                kVTCompressionPropertyKey_RealTime,
                kCFBooleanTrue as *const std::ffi::c_void,
            );

            // Set profile
            VTSessionSetProperty(
                session,
                kVTCompressionPropertyKey_ProfileLevel,
                kVTProfileLevel_H264_Baseline_AutoLevel,
            );

            // Set entropy mode (CABAC)
            VTSessionSetProperty(
                session,
                kVTCompressionPropertyKey_H264EntropyMode,
                kVTH264EntropyMode_CABAC,
            );

            // Set keyframe interval (every 60 frames = 2 seconds @ 30fps)
            let kf_interval = NSNumber::new_i32(60);
            VTSessionSetProperty(
                session,
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                kf_interval.as_ptr() as *const std::ffi::c_void,
            );

            // Set bitrate (1 Mbps)
            let bitrate = NSNumber::new_i32(1_000_000);
            VTSessionSetProperty(
                session,
                kVTCompressionPropertyKey_AverageBitRate,
                bitrate.as_ptr() as *const std::ffi::c_void,
            );

            // Set framerate
            let framerate = NSNumber::new_i32(30);
            VTSessionSetProperty(
                session,
                kVTCompressionPropertyKey_ExpectedFrameRate,
                framerate.as_ptr() as *const std::ffi::c_void,
            );
        }

        Ok(Self {
            session,
            width,
            height,
            frame_count: 0,
            output_callback,
        })
    }

    /// Encode a solid color frame
    pub fn encode_color_frame(&mut self, r: u8, g: u8, b: u8) -> Result<()> {
        // Create CVPixelBuffer with solid color
        let pixel_buffer = self.create_solid_color_buffer(r, g, b)?;

        let pts = CMTime {
            value: self.frame_count as i64,
            timescale: 30, // 30 fps
            flags: 1,
            epoch: 0,
        };

        let duration = CMTime {
            value: 1,
            timescale: 30,
            flags: 1,
            epoch: 0,
        };

        unsafe {
            let status = VTCompressionSessionEncodeFrame(
                self.session,
                pixel_buffer,
                pts,
                duration,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );

            // Release pixel buffer
            CVPixelBufferRelease(pixel_buffer as *mut CVPixelBuffer);

            if status != 0 {
                anyhow::bail!("VTCompressionSessionEncodeFrame failed: {}", status);
            }
        }

        self.frame_count += 1;
        Ok(())
    }

    fn create_solid_color_buffer(&self, r: u8, g: u8, b: u8) -> Result<*mut std::ffi::c_void> {
        unsafe {
            let mut pixel_buffer: *mut CVPixelBuffer = std::ptr::null_mut();

            // Create pixel buffer (BGRA format)
            let status = CVPixelBufferCreate(
                std::ptr::null(),
                self.width,
                self.height,
                kCVPixelFormatType_32BGRA,
                std::ptr::null(),
                &mut pixel_buffer,
            );

            if status != 0 {
                anyhow::bail!("CVPixelBufferCreate failed: {}", status);
            }

            // Lock base address
            CVPixelBufferLockBaseAddress(pixel_buffer, 0);

            let base_address = CVPixelBufferGetBaseAddress(pixel_buffer);
            let bytes_per_row = CVPixelBufferGetBytesPerRow(pixel_buffer);

            // Fill with solid color (BGRA format)
            let buffer = std::slice::from_raw_parts_mut(
                base_address as *mut u8,
                bytes_per_row * self.height,
            );

            for y in 0..self.height {
                for x in 0..self.width {
                    let offset = y * bytes_per_row + x * 4;
                    buffer[offset] = b;     // B
                    buffer[offset + 1] = g; // G
                    buffer[offset + 2] = r; // R
                    buffer[offset + 3] = 255; // A
                }
            }

            CVPixelBufferUnlockBaseAddress(pixel_buffer, 0);

            Ok(pixel_buffer as *mut std::ffi::c_void)
        }
    }

    pub fn flush(&mut self) -> Result<()> {
        unsafe {
            let status = VTCompressionSessionCompleteFrames(
                self.session,
                CMTime {
                    value: i64::MAX,
                    timescale: 1,
                    flags: 0,
                    epoch: 0,
                },
            );

            if status != 0 {
                anyhow::bail!("VTCompressionSessionCompleteFrames failed: {}", status);
            }
        }

        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl Drop for VideoToolboxEncoder {
    fn drop(&mut self) {
        unsafe {
            if !self.session.is_null() {
                VTCompressionSessionInvalidate(self.session);
            }
        }
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn compression_output_callback(
    output_callback_refcon: *mut std::ffi::c_void,
    _source_frame_refcon: *mut std::ffi::c_void,
    status: i32,
    _info_flags: u32,
    sample_buffer: *mut std::ffi::c_void,
) {
    if status != 0 {
        tracing::error!("Compression callback error: {}", status);
        return;
    }

    if sample_buffer.is_null() {
        return;
    }

    // Reconstruct Arc from raw pointer
    let callback = Arc::from_raw(output_callback_refcon as *const Mutex<Box<dyn FnMut(Vec<u8>, bool) + Send>>);

    // Extract H.264 NAL units from CMSampleBuffer
    if let Some(nal_data) = extract_h264_from_sample_buffer(sample_buffer) {
        let is_keyframe = is_sample_buffer_keyframe(sample_buffer);

        if let Ok(mut cb) = callback.lock() {
            cb(nal_data, is_keyframe);
        }
    }

    // Prevent Arc from being dropped (we still need it for future callbacks)
    std::mem::forget(callback);
}

#[cfg(target_os = "macos")]
unsafe fn extract_h264_from_sample_buffer(sample_buffer: *mut std::ffi::c_void) -> Option<Vec<u8>> {
    use core_foundation::base::CFRetain;

    // This is a simplified version - in production you'd need to handle format descriptions,
    // parameter sets (SPS/PPS), and properly convert AVCC to Annex B format

    // For now, return None to keep using synthetic frames until we implement full extraction
    None
}

#[cfg(target_os = "macos")]
unsafe fn is_sample_buffer_keyframe(sample_buffer: *mut std::ffi::c_void) -> bool {
    // Check if frame is a keyframe
    // This would require accessing CMSampleBuffer attachments
    // For now, return false
    false
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}
