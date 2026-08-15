// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pixel buffer pool for efficient buffer allocation.

use super::{PixelBuffer, PixelFormat};
use crate::core::Result;

/// Platform-agnostic identifier for one reusable slot of a pixel-buffer pool.
///
/// Uses UUID for global uniqueness across parallel runtimes. Names the
/// allocation, never any frame written into it: per-slot resources
/// (surface-share registration, device-export stagings, texture caches) key
/// on this, while the id a bag publishes is [`PublishedPixelBufferFrameId`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PixelBufferPoolSlotId(String);

impl PixelBufferPoolSlotId {
    /// Generate a new unique ID.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Create from an existing string (e.g., from IPC deserialization).
    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    /// Create from a string slice.
    pub fn from_str(s: &str) -> Self {
        Self(s.to_string())
    }

    /// Get the ID as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for PixelBufferPoolSlotId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for PixelBufferPoolSlotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The surface id one pool acquisition publishes — it names the frame, not
/// the slot backing it.
///
/// Travels as `<slot>#<generation>`; consumers treat it as an opaque string.
/// Recycling the slot mints the next generation and retires this one, so a
/// stale id stops resolving instead of naming somebody else's pixels
/// (`docs/decisions/surface-id-lifetime-contract.md`). The `#<digits>` suffix
/// is reserved for this grammar: no other surface id may carry one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PublishedPixelBufferFrameId {
    pool_slot_id: PixelBufferPoolSlotId,
    frame_generation: u64,
}

impl PublishedPixelBufferFrameId {
    /// The id `frame_generation` publishes over `pool_slot_id`.
    pub fn new(pool_slot_id: PixelBufferPoolSlotId, frame_generation: u64) -> Self {
        Self {
            pool_slot_id,
            frame_generation,
        }
    }

    /// The slot this frame was written into — what per-slot resources key on.
    pub fn pool_slot_id(&self) -> &PixelBufferPoolSlotId {
        &self.pool_slot_id
    }

    /// Which acquisition of the slot this id names.
    pub fn frame_generation(&self) -> u64 {
        self.frame_generation
    }

    /// Read a published frame id back out of a surface-id string; `None` for
    /// ids that carry no `#<generation>` suffix (non-pool surfaces).
    pub fn parse(surface_id: &str) -> Option<Self> {
        let (pool_slot, generation_digits) = surface_id.rsplit_once('#')?;
        if pool_slot.is_empty()
            || generation_digits.is_empty()
            || !generation_digits.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        Some(Self {
            pool_slot_id: PixelBufferPoolSlotId::from_str(pool_slot),
            frame_generation: generation_digits.parse().ok()?,
        })
    }
}

impl std::fmt::Display for PublishedPixelBufferFrameId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}", self.pool_slot_id, self.frame_generation)
    }
}

/// The pool-slot portion of any surface id: strips a published frame id's
/// `#<generation>` suffix and returns every other id whole. What per-slot
/// caches normalize their keys through.
pub fn pool_slot_key_of_surface_id(surface_id: &str) -> &str {
    match PublishedPixelBufferFrameId::parse(surface_id) {
        Some(_) => surface_id
            .rsplit_once('#')
            .map(|(pool_slot, _)| pool_slot)
            .unwrap_or(surface_id),
        None => surface_id,
    }
}

/// Descriptor for creating pixel buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelBufferDescriptor {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Pixel format.
    pub format: PixelFormat,
}

impl PixelBufferDescriptor {
    /// Create a new descriptor.
    pub fn new(width: u32, height: u32, format: PixelFormat) -> Self {
        Self {
            width,
            height,
            format,
        }
    }
}

/// Pool for reusable pixel buffers.
///
/// Wraps the platform's buffer pool (CVPixelBufferPool on macOS).
/// Buffers are automatically recycled when their refcount drops to zero.
pub struct RhiPixelBufferPool {
    #[cfg(target_os = "macos")]
    pub(crate) inner: crate::metal::rhi::pixel_buffer_pool::PixelBufferPoolMacOS,

    #[cfg(target_os = "linux")]
    pub(crate) inner: crate::vulkan::rhi::VulkanPixelBufferPool,

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(crate) _marker: std::marker::PhantomData<()>,
}

impl RhiPixelBufferPool {
    /// Acquire a buffer from the pool.
    ///
    /// Returns (id, buffer) where id is the platform-agnostic identifier.
    /// Returns a recycled buffer if available, or allocates a new one.
    pub fn acquire(&self) -> Result<(PixelBufferPoolSlotId, PixelBuffer)> {
        #[cfg(target_os = "macos")]
        {
            self.inner.acquire()
        }
        #[cfg(target_os = "linux")]
        {
            self.inner.acquire()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(crate::core::Error::Configuration(
                "RhiPixelBufferPool not implemented for this platform".into(),
            ))
        }
    }

    /// Grow the pool by one buffer and hand it out.
    ///
    /// What a manager calls when every existing slot is held — by an
    /// in-process reader or by a cross-process checkout lease — and the
    /// producer still needs somewhere to write. What comes back must be a
    /// slot no caller has seen before: the manager appends it to its ring,
    /// so a recycled buffer would enter the ring twice under one id.
    ///
    /// On macOS this refuses rather than growing: `CVPixelBufferPool` may
    /// recycle, and `PixelBufferPoolMacOS::acquire` deliberately returns the
    /// existing id for a recycled IOSurface — exactly the duplicate the ring
    /// must never hold. Growth there needs a dedicated fresh-allocation path.
    pub fn allocate_additional_buffer(&mut self) -> Result<(PixelBufferPoolSlotId, PixelBuffer)> {
        #[cfg(target_os = "macos")]
        {
            Err(crate::core::Error::NotSupported(
                "macOS pool growth needs a fresh-allocation path: CVPixelBufferPool recycling \
                 would re-enter the ring under an existing id"
                    .into(),
            ))
        }
        #[cfg(target_os = "linux")]
        {
            self.inner.allocate_additional_buffer()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(crate::core::Error::Configuration(
                "RhiPixelBufferPool not implemented for this platform".into(),
            ))
        }
    }
}

impl std::fmt::Debug for RhiPixelBufferPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiPixelBufferPool").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_published_frame_id_round_trips_through_its_string_form() {
        let published = PublishedPixelBufferFrameId::new(PixelBufferPoolSlotId::new(), 17);
        let reparsed = PublishedPixelBufferFrameId::parse(&published.to_string())
            .expect("its own string form parses");
        assert_eq!(reparsed, published);
        assert_eq!(reparsed.frame_generation(), 17);
    }

    #[test]
    fn the_slot_key_of_a_published_id_is_its_slot() {
        let slot = PixelBufferPoolSlotId::new();
        let published = PublishedPixelBufferFrameId::new(slot.clone(), 3).to_string();
        assert_eq!(pool_slot_key_of_surface_id(&published), slot.as_str());
    }

    /// Non-pool ids — check-in surfaces, texture uuids — carry no generation
    /// and must key to themselves, or every existing surface consumer breaks.
    #[test]
    fn an_id_without_a_generation_suffix_keys_to_itself_and_parses_to_none() {
        for plain in [
            "surface-42",
            "ee69c28e-fef2-4a13-b444-27c7e03202c2",
            "a-name-with#no-digit-suffix",
            "#7",
            "trailing-hash#",
        ] {
            assert!(PublishedPixelBufferFrameId::parse(plain).is_none(), "{plain}");
            assert_eq!(pool_slot_key_of_surface_id(plain), plain);
        }
    }

    /// A generation too large for u64 cannot have been minted by this
    /// process; treating the id as non-pool beats a panic on hostile input.
    #[test]
    fn an_unmintable_generation_reads_as_a_plain_id() {
        let hostile = "slot#99999999999999999999999999";
        assert!(PublishedPixelBufferFrameId::parse(hostile).is_none());
        assert_eq!(pool_slot_key_of_surface_id(hostile), hostile);
    }
}
