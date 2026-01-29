// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Metal implementation of RhiBlitter with IOSurface texture caching.

use crate::apple::iosurface::create_metal_texture_from_iosurface;
use crate::apple::texture_pool_macos::get_iosurface_id;
use crate::core::rhi::blitter::RhiBlitter;
use crate::core::rhi::{RhiCommandQueue, RhiPixelBuffer};
use crate::core::{Result, StreamError};
use metal::foreign_types::ForeignTypeRef;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_io_surface::IOSurface;
use objc2_metal::MTLTexture;
use std::collections::HashMap;
use std::sync::Mutex;

/// Wrapper for Metal texture to make it Send+Sync.
/// Metal textures are thread-safe for read operations.
struct SendableTexture(Retained<ProtocolObject<dyn MTLTexture>>);

// SAFETY: Metal textures are thread-safe - they can be used from any thread
// as long as we don't modify their state concurrently. The blit operation
// only reads from source and writes to destination, which is safe.
unsafe impl Send for SendableTexture {}
unsafe impl Sync for SendableTexture {}

/// Metal blitter with IOSurfaceID-keyed texture cache.
///
/// Caches Metal textures created from IOSurfaces to avoid repeated texture
/// creation overhead when blitting to the same destination buffers.
pub struct MetalBlitter {
    command_queue: RhiCommandQueue,
    /// Texture cache keyed by IOSurfaceID.
    /// Reuses textures for repeated blits to same IOSurface.
    texture_cache: Mutex<HashMap<u32, SendableTexture>>,
}

impl MetalBlitter {
    /// Create a new Metal blitter with the given command queue.
    pub fn new(command_queue: RhiCommandQueue) -> Self {
        Self {
            command_queue,
            texture_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Get or create a cached texture for an IOSurface.
    fn get_or_create_texture(
        &self,
        iosurface: &IOSurface,
    ) -> Result<Retained<ProtocolObject<dyn MTLTexture>>> {
        // Use IOSurface ID (stable identifier) not seed (modification counter)
        let surface_id = get_iosurface_id(iosurface);
        let mut cache = self.texture_cache.lock().unwrap();

        if let Some(sendable) = cache.get(&surface_id) {
            return Ok(sendable.0.clone());
        }

        // Cache miss - create new texture
        let metal_queue = self.command_queue.metal_queue_ref();
        let device = metal_queue.device();

        // Convert metal crate device to objc2 device
        let device_ptr = device.as_ptr() as *const ProtocolObject<dyn objc2_metal::MTLDevice>;
        let device_ref = unsafe { &*device_ptr };

        let texture = create_metal_texture_from_iosurface(device_ref, iosurface, 0)?;
        cache.insert(surface_id, SendableTexture(texture.clone()));

        Ok(texture)
    }
}

impl RhiBlitter for MetalBlitter {
    fn blit_copy(&self, src: &RhiPixelBuffer, dest: &RhiPixelBuffer) -> Result<()> {
        // Get IOSurfaces from both buffers
        let src_iosurface = src
            .buffer_ref()
            .iosurface_ref()
            .ok_or_else(|| StreamError::GpuError("Source buffer not backed by IOSurface".into()))?;

        let dest_iosurface = dest
            .buffer_ref()
            .iosurface_ref()
            .ok_or_else(|| StreamError::GpuError("Dest buffer not backed by IOSurface".into()))?;

        // Cast to objc2 IOSurface references
        let src_iosurface_ref = unsafe { &*(src_iosurface as *const IOSurface) };
        let dest_iosurface_ref = unsafe { &*(dest_iosurface as *const IOSurface) };

        // Get or create textures (destination is cached, source typically isn't)
        let metal_queue = self.command_queue.metal_queue_ref();
        let device = metal_queue.device();

        let device_ptr = device.as_ptr() as *const ProtocolObject<dyn objc2_metal::MTLDevice>;
        let device_ref = unsafe { &*device_ptr };

        // Source texture - not cached (camera frames have unique IOSurfaces)
        let src_texture = create_metal_texture_from_iosurface(device_ref, src_iosurface_ref, 0)?;

        // Destination texture - cached (pooled buffers are reused)
        let dest_texture = self.get_or_create_texture(dest_iosurface_ref)?;

        // Perform blit
        let command_buffer = metal_queue.new_command_buffer();
        let blit_encoder = command_buffer.new_blit_command_encoder();

        let origin = metal::MTLOrigin { x: 0, y: 0, z: 0 };
        let size = metal::MTLSize {
            width: src.width as u64,
            height: src.height as u64,
            depth: 1,
        };

        // Convert objc2 textures to metal crate references for blit API
        let src_texture_ptr = &*src_texture as *const _ as *mut std::ffi::c_void;
        let src_texture_ref = unsafe { metal::TextureRef::from_ptr(src_texture_ptr as *mut _) };

        let dest_texture_ptr = &*dest_texture as *const _ as *mut std::ffi::c_void;
        let dest_texture_ref = unsafe { metal::TextureRef::from_ptr(dest_texture_ptr as *mut _) };

        blit_encoder.copy_from_texture(
            src_texture_ref,
            0, // source slice
            0, // source level
            origin,
            size,
            dest_texture_ref,
            0, // dest slice
            0, // dest level
            origin,
        );

        blit_encoder.end_encoding();
        command_buffer.commit();
        command_buffer.wait_until_completed();

        Ok(())
    }

    unsafe fn blit_copy_iosurface_raw(
        &self,
        src: *const std::ffi::c_void,
        dest: &RhiPixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        // Get destination IOSurface
        let dest_iosurface = dest
            .buffer_ref()
            .iosurface_ref()
            .ok_or_else(|| StreamError::GpuError("Dest buffer not backed by IOSurface".into()))?;

        // Cast pointers to objc2 IOSurface references
        let src_iosurface_ref = &*(src as *const IOSurface);
        let dest_iosurface_ref = &*(dest_iosurface as *const IOSurface);

        // Get Metal device
        let metal_queue = self.command_queue.metal_queue_ref();
        let device = metal_queue.device();

        let device_ptr = device.as_ptr() as *const ProtocolObject<dyn objc2_metal::MTLDevice>;
        let device_ref = &*device_ptr;

        // Create source texture (not cached - camera IOSurfaces are transient)
        let src_texture = create_metal_texture_from_iosurface(device_ref, src_iosurface_ref, 0)?;

        // Get or create destination texture (cached - pooled buffers are reused)
        let dest_texture = self.get_or_create_texture(dest_iosurface_ref)?;

        // Perform blit
        let command_buffer = metal_queue.new_command_buffer();
        let blit_encoder = command_buffer.new_blit_command_encoder();

        let origin = metal::MTLOrigin { x: 0, y: 0, z: 0 };
        let size = metal::MTLSize {
            width: width as u64,
            height: height as u64,
            depth: 1,
        };

        // Convert objc2 textures to metal crate references for blit API
        let src_texture_ptr = &*src_texture as *const _ as *mut std::ffi::c_void;
        let src_texture_ref = metal::TextureRef::from_ptr(src_texture_ptr as *mut _);

        let dest_texture_ptr = &*dest_texture as *const _ as *mut std::ffi::c_void;
        let dest_texture_ref = metal::TextureRef::from_ptr(dest_texture_ptr as *mut _);

        blit_encoder.copy_from_texture(
            src_texture_ref,
            0, // source slice
            0, // source level
            origin,
            size,
            dest_texture_ref,
            0, // dest slice
            0, // dest level
            origin,
        );

        blit_encoder.end_encoding();
        command_buffer.commit();
        command_buffer.wait_until_completed();

        Ok(())
    }

    fn clear_cache(&self) {
        let mut cache = self.texture_cache.lock().unwrap();
        cache.clear();
    }
}
