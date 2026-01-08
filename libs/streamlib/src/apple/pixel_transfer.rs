// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::apple::iosurface;
use crate::core::rhi::{GpuDevice, RhiCommandQueue, RhiPixelBuffer, StreamTexture};
use crate::core::{Result, StreamError};
use metal::foreign_types::ForeignTypeRef;
use objc2_core_video::CVPixelBuffer;
use objc2_io_surface::IOSurface;
use std::sync::Arc;

// Local FFI declarations for functions not in objc2_core_video
mod ffi {
    use super::*;
    use std::ffi::c_void;

    #[link(name = "CoreVideo", kind = "framework")]
    extern "C" {
        pub fn CVPixelBufferGetIOSurface(pixelBuffer: *const CVPixelBuffer) -> *mut IOSurface;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFRelease(cf: *const c_void);
    }

    // Re-export VideoToolbox types from webrtc.rs
    // Since videotoolbox module is private, we need these types locally
    pub type VTPixelTransferSessionRef = *mut c_void;
    pub type CVPixelBufferRef = *mut c_void;
    pub type OSStatus = i32;
    pub const NO_ERR: OSStatus = 0;

    #[link(name = "VideoToolbox", kind = "framework")]
    extern "C" {
        pub fn VTPixelTransferSessionCreate(
            allocator: *const c_void,
            pixel_transfer_session_out: *mut VTPixelTransferSessionRef,
        ) -> OSStatus;

        pub fn VTPixelTransferSessionTransferImage(
            session: VTPixelTransferSessionRef,
            source_buffer: CVPixelBufferRef,
            destination_buffer: CVPixelBufferRef,
        ) -> OSStatus;

        pub fn VTPixelTransferSessionInvalidate(session: VTPixelTransferSessionRef);
    }
}

/// GPU-accelerated pixel format converter using VTPixelTransferSession.
pub struct PixelTransferSession {
    session: ffi::VTPixelTransferSessionRef,
    device: Arc<GpuDevice>,
    command_queue: RhiCommandQueue,
}

impl PixelTransferSession {
    /// Creates a new pixel transfer session.
    pub fn new(device: Arc<GpuDevice>) -> Result<Self> {
        // Create VTPixelTransferSession
        let mut session: ffi::VTPixelTransferSessionRef = std::ptr::null_mut();

        unsafe {
            let status = ffi::VTPixelTransferSessionCreate(
                std::ptr::null(), // allocator (use default)
                &mut session,
            );

            if status != ffi::NO_ERR {
                return Err(StreamError::Runtime(format!(
                    "VTPixelTransferSessionCreate failed: {}",
                    status
                )));
            }
        }

        // Configure color space properties (BT.709, limited range for H.264)
        // TODO: Set VTPixelTransferSession properties if needed:
        // - kVTPixelTransferPropertyKey_ScalingMode
        // - kVTPixelTransferPropertyKey_DestinationColorPrimaries
        // - kVTPixelTransferPropertyKey_DestinationYCbCrMatrix (BT.709)
        // For now, defaults should work for our use case

        // Use shared command queue from device
        let command_queue = device.command_queue().clone();

        Ok(Self {
            session,
            device,
            command_queue,
        })
    }

    /// Converts an RGBA texture to NV12 CVPixelBuffer.
    pub fn convert_to_nv12(
        &self,
        texture: &StreamTexture,
        width: u32,
        height: u32,
    ) -> Result<*mut CVPixelBuffer> {
        // Step 1: Metal Blit - texture → RGBA CVPixelBuffer
        let source_rgba_buffer = self.blit_to_rgba_pixel_buffer(texture, width, height)?;

        // Step 2: VTPixelTransferSession - RGBA → NV12
        let dest_nv12_buffer = self.transfer_rgba_to_nv12(source_rgba_buffer, width, height)?;

        // Release the temporary RGBA buffer (we only need the NV12 result)
        unsafe {
            ffi::CFRelease(source_rgba_buffer as *const _);
        }

        Ok(dest_nv12_buffer)
    }

    /// Converts an RhiPixelBuffer (containing CVPixelBuffer) to NV12 CVPixelBuffer.
    ///
    /// This is the buffer-centric path for VideoFrame encoding. The source buffer
    /// is typically BGRA from camera capture or BGRA from video decoder.
    pub fn convert_buffer_to_nv12(&self, buffer: &RhiPixelBuffer) -> Result<*mut CVPixelBuffer> {
        let source_ptr = buffer.ref_.as_ptr() as *mut CVPixelBuffer;
        self.transfer_rgba_to_nv12(source_ptr, buffer.width, buffer.height)
    }

    /// Step 1: Uses Metal blit to copy texture data into RGBA CVPixelBuffer
    fn blit_to_rgba_pixel_buffer(
        &self,
        texture: &StreamTexture,
        width: u32,
        height: u32,
    ) -> Result<*mut CVPixelBuffer> {
        // Get Metal texture directly from StreamTexture
        let source_metal = texture.as_metal_texture();

        // Create destination CVPixelBuffer in BGRA format (32BGRA is standard for Metal/CoreVideo interop)
        let mut pixel_buffer: *mut CVPixelBuffer = std::ptr::null_mut();
        let bgra_format: u32 = 0x42475241; // 'BGRA' fourCC (kCVPixelFormatType_32BGRA)

        unsafe {
            use core_foundation::base::TCFType;
            use core_foundation::dictionary::CFMutableDictionary;
            use core_foundation::string::CFString;

            // Create attributes dictionary to request IOSurface-backed CVPixelBuffer
            // This is required for Metal texture interop
            use core_foundation::base::CFTypeRef;
            let io_surface_props: CFMutableDictionary<CFString, CFTypeRef> =
                CFMutableDictionary::new();
            let mut pixel_buffer_attrs_mut: CFMutableDictionary<CFString, CFTypeRef> =
                CFMutableDictionary::new();

            let io_surface_key = CFString::from_static_string("IOSurfaceProperties");
            pixel_buffer_attrs_mut.set(io_surface_key, io_surface_props.as_CFTypeRef());

            // Convert to immutable CFDictionary for CVPixelBufferCreate
            let pixel_buffer_attrs = pixel_buffer_attrs_mut.to_immutable();

            // objc2_core_video expects Option<&CFDictionary> but we have core_foundation::CFDictionary
            // They're both wrappers around the same __CFDictionary opaque type, so we can safely transmute
            // the pointer. This avoids adding objc2_core_foundation as a dependency.
            let attrs_ptr = pixel_buffer_attrs.as_concrete_TypeRef();
            // Cast the raw pointer to match objc2's expected type
            // SAFETY: Both core_foundation and objc2_core_foundation wrap the same C type
            let attrs_ref = &*(attrs_ptr as *const _ as *const std::ffi::c_void as *const _);

            let status = objc2_core_video::CVPixelBufferCreate(
                None, // allocator
                width as usize,
                height as usize,
                bgra_format,
                Some(attrs_ref),
                std::ptr::NonNull::from(&mut pixel_buffer),
            );

            if status != 0 {
                return Err(StreamError::GpuError(format!(
                    "CVPixelBufferCreate (BGRA) failed: {}",
                    status
                )));
            }
        }

        // Get IOSurface from CVPixelBuffer
        let iosurface_ptr = unsafe { ffi::CVPixelBufferGetIOSurface(pixel_buffer) };

        if iosurface_ptr.is_null() {
            return Err(StreamError::GpuError(
                "Failed to get IOSurface from CVPixelBuffer".into(),
            ));
        }

        let iosurface = unsafe { &*iosurface_ptr };

        // Create Metal texture from IOSurface (destination for blit)
        let dest_metal = iosurface::create_metal_texture_from_iosurface(
            self.device.as_metal_device().device(),
            iosurface,
            0, // plane 0
        )?;

        // Perform Metal blit (GPU copy)
        let command_buffer = self.command_queue.metal_queue_ref().new_command_buffer();
        let blit_encoder = command_buffer.new_blit_command_encoder();

        let origin = metal::MTLOrigin { x: 0, y: 0, z: 0 };
        let size = metal::MTLSize {
            width: width as u64,
            height: height as u64,
            depth: 1,
        };

        // Convert objc2 Metal texture to metal crate texture for blitting
        let dest_metal_ptr = &*dest_metal as *const _ as *mut std::ffi::c_void;
        let dest_metal_ref = unsafe { metal::TextureRef::from_ptr(dest_metal_ptr as *mut _) };

        blit_encoder.copy_from_texture(
            source_metal,
            0, // source slice
            0, // source level
            origin,
            size,
            dest_metal_ref,
            0, // dest slice
            0, // dest level
            origin,
        );

        blit_encoder.end_encoding();
        command_buffer.commit();
        command_buffer.wait_until_completed();

        Ok(pixel_buffer)
    }

    /// Step 2: Uses VTPixelTransferSession to convert RGBA → NV12
    fn transfer_rgba_to_nv12(
        &self,
        source_buffer: *mut CVPixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<*mut CVPixelBuffer> {
        // Create destination CVPixelBuffer in NV12 format
        let mut dest_buffer: *mut CVPixelBuffer = std::ptr::null_mut();
        let nv12_format: u32 = 0x34323076; // '420v' - kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange

        unsafe {
            let status = objc2_core_video::CVPixelBufferCreate(
                None, // allocator
                width as usize,
                height as usize,
                nv12_format,
                None, // attributes
                std::ptr::NonNull::from(&mut dest_buffer),
            );

            if status != 0 {
                return Err(StreamError::GpuError(format!(
                    "CVPixelBufferCreate (NV12) failed: {}",
                    status
                )));
            }
        }

        // Perform GPU-accelerated format conversion
        unsafe {
            let status = ffi::VTPixelTransferSessionTransferImage(
                self.session,
                source_buffer as ffi::CVPixelBufferRef,
                dest_buffer as ffi::CVPixelBufferRef,
            );

            if status != ffi::NO_ERR {
                // Clean up destination buffer on error
                ffi::CFRelease(dest_buffer as *const _);
                return Err(StreamError::Runtime(format!(
                    "VTPixelTransferSessionTransferImage failed: {}",
                    status
                )));
            }
        }

        Ok(dest_buffer)
    }
}

impl Drop for PixelTransferSession {
    fn drop(&mut self) {
        unsafe {
            ffi::VTPixelTransferSessionInvalidate(self.session);
        }
    }
}

// SAFETY: VTPixelTransferSession is thread-safe after creation
// Metal command queues are also thread-safe
unsafe impl Send for PixelTransferSession {}
unsafe impl Sync for PixelTransferSession {}
