// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which backing answers for a published surface id, and the one-buffer pixel
//! shape it presents — shared by every door that reads a frame out.

use crate::core::context::GpuContext;
use crate::core::error::{Error, Result};
use streamlib_consumer_rhi::{PixelFormat, TextureFormat};

/// The pixel shape a texture-backed export presents to its consumer —
/// [`TextureFormat::host_view_pixel_format`], residency-neutral because a
/// staging is one buffer at either residency.
pub(crate) fn export_pixel_shape_for_texture(format: TextureFormat) -> Result<PixelFormat> {
    format.host_view_pixel_format().ok_or_else(|| {
        Error::GpuError(format!(
            "a surface export refuses {format:?}: it is planar, and a one-buffer export would \
             drop a plane"
        ))
    })
}

/// Bytes per pixel of a one-plane format, refusing one that is planar.
pub(crate) fn export_bytes_per_pixel_for_pixel_format(format: PixelFormat) -> Result<u32> {
    if format.plane_count() > 1 || format == PixelFormat::Unknown {
        return Err(Error::GpuError(format!(
            "a surface export refuses {format:?}: a one-buffer read carries one plane, and \
             reading only the first would hand out part of the image"
        )));
    }
    Ok(format.bits_per_pixel() / 8)
}

/// The backing a published surface id resolved to for one read — looked up
/// fresh on every read so a rotating producer's re-registration is honoured,
/// never a snapshot.
pub(crate) enum ResolvedSurfaceBacking {
    RegisteredTexture(crate::core::context::TextureRegistration),
    PixelBuffer(crate::core::rhi::PixelBuffer),
}

impl ResolvedSurfaceBacking {
    /// The pixel extent this backing carries, refusing a zero one by name.
    ///
    /// A zero extent means the backing resolved through a path that carries
    /// no shape — a cross-process `lookup` import, which hands back planes and
    /// no geometry.
    pub(crate) fn pixel_extent(&self, surface_id: &str) -> Result<(u32, u32)> {
        let (pixel_width, pixel_height) = match self {
            Self::PixelBuffer(pixel_buffer) => (pixel_buffer.width, pixel_buffer.height),
            Self::RegisteredTexture(registration) => {
                let texture = registration.texture();
                (texture.width(), texture.height())
            }
        };
        if pixel_width == 0 || pixel_height == 0 {
            return Err(Error::NotSupported(format!(
                "surface '{surface_id}' resolves to a backing this process holds no pixel extent \
                 for, so there are no pixels to read out of it here; read it from the runtime \
                 that owns its pool"
            )));
        }
        Ok((pixel_width, pixel_height))
    }

    /// The one-plane pixel format a host read of this backing presents, and
    /// its bytes per pixel.
    pub(crate) fn one_plane_pixel_format_and_bytes_per_pixel(&self) -> Result<(PixelFormat, u32)> {
        let pixel_format = match self {
            Self::PixelBuffer(pixel_buffer) => pixel_buffer.format(),
            Self::RegisteredTexture(registration) => {
                export_pixel_shape_for_texture(registration.texture().format())?
            }
        };
        Ok((
            pixel_format,
            export_bytes_per_pixel_for_pixel_format(pixel_format)?,
        ))
    }

    /// The IOSurface this backing's storage is, when it is one.
    #[cfg(target_os = "macos")]
    pub(crate) fn backing_iosurface(&self) -> Option<&objc2_io_surface::IOSurfaceRef> {
        use crate::host_rhi::HostTextureExt as _;

        match self {
            Self::PixelBuffer(pixel_buffer) => pixel_buffer.buffer_ref().inner.backing_iosurface(),
            Self::RegisteredTexture(registration) => registration
                .texture()
                .vulkan_inner()
                .backing_iosurface()
                .map(|iosurface| &**iosurface),
        }
    }
}

impl GpuContext {
    /// Resolve the current blit source for `surface_id` — the surface's
    /// pooled backing whenever it has one, the registered texture only
    /// for surfaces that have none.
    ///
    /// The pool member is the frame the bag named; a producer's own
    /// registered texture is a frames-in-flight transient holding
    /// whatever that producer has rendered since. Sourcing the transient
    /// hands a consumer a different frame under the id it asked for, so
    /// a producer-internal texture never backs a cross-process export.
    /// Texture-first survives for surfaces with no pooled member — kernel
    /// outputs, whose id↔backing binding is stable.
    /// Both same-process caches are consulted first, in that same
    /// priority order. Either composite lookup below would find them,
    /// but each reaches the surface-share service on the way — and a
    /// miss there is a blocking socket round trip, which for a
    /// texture-only surface would be paid on every refill to learn what
    /// the local pool already knows.
    pub(crate) fn resolve_device_export_source(
        &self,
        surface_id: &str,
    ) -> Result<ResolvedSurfaceBacking> {
        if let Some(pixel_buffer) = self.pooled_backing_held_in_this_process(surface_id) {
            return Ok(ResolvedSurfaceBacking::PixelBuffer(pixel_buffer));
        }
        if let Some(registration) = self.producer_registered_texture_for_surface_id(surface_id) {
            return Ok(ResolvedSurfaceBacking::RegisteredTexture(registration));
        }
        match self.resolve_pixel_buffer_by_surface_id(surface_id) {
            Ok(pixel_buffer) => Ok(ResolvedSurfaceBacking::PixelBuffer(pixel_buffer)),
            Err(buffer_miss) => {
                match self.resolve_texture_registration_by_surface_id(surface_id, None, 0, 0) {
                    Ok(registration) => Ok(ResolvedSurfaceBacking::RegisteredTexture(registration)),
                    Err(texture_miss) => Err(Error::GpuError(format!(
                        "surface {surface_id} resolves to neither a pixel buffer \
                         ({buffer_miss}) nor a registered texture ({texture_miss})"
                    ))),
                }
            }
        }
    }
}
