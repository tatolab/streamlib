// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `SurfaceAdapter` trait, ABI version constant, and capability marker traits.

use crate::error::AdapterError;
use crate::guard::{ReadGuard, WriteGuard};
use crate::surface::{StreamlibSurface, SurfaceId};

/// Major version of the surface-adapter ABI.
///
/// Bumped on a breaking trait change (renamed/removed method, changed
/// signature, changed `#[repr(C)]` field of any associated type).
/// Adding new methods is non-breaking and does NOT bump.
///
/// Rust's vtable layout already enforces in-process compatibility at
/// compile time — a mismatched `streamlib-adapter-abi` rlib version
/// can't link into the runtime in the first place. This constant is
/// load-bearing only at the cdylib boundary, where it'll be checked
/// from a `#[repr(C)] AdapterDeclaration` shape (mirroring
/// `streamlib-plugin-abi`'s `PluginDeclaration`) when dynamic adapter
/// loading lands.
pub const STREAMLIB_ADAPTER_ABI_VERSION: u32 = 1;

/// Public ABI for a streamlib surface adapter.
///
/// Adapters implement this trait to expose a host-allocated GPU surface
/// to a customer in their framework's idiomatic shape (Vulkan, OpenGL,
/// Skia, CPU readback, …). Customers acquire scoped read or write
/// access through [`Self::acquire_read`] / [`Self::acquire_write`] —
/// synchronization (timeline semaphores, layout transitions) happens
/// inside the scope and never appears in the customer's API.
///
/// Two acquisition flavors are provided:
///
/// - Blocking [`Self::acquire_read`] / [`Self::acquire_write`] —
///   waits on the acquire-side timeline semaphore (and, for write,
///   for any contended reader/writer to release). Right shape for
///   batch / one-shot consumers.
/// - Non-blocking [`Self::try_acquire_read`] / [`Self::try_acquire_write`] —
///   returns `Ok(None)` immediately if the surface is contended,
///   never blocks the caller. Right shape for processor-graph nodes
///   that must not stall their thread runner while waiting on a
///   downstream consumer to finish.
///
/// The trait is `Send + Sync`. Concrete adapters typically wrap
/// interior-mutable state behind a `Mutex` or `RwLock` so the trait's
/// `&self` shape stays satisfiable without leaking a runtime-error
/// "wrong mode" path that the typestate already prevents.
pub trait SurfaceAdapter: Send + Sync {
    /// View handed out by [`Self::acquire_read`] for the guard's lifetime.
    type ReadView<'g>
    where
        Self: 'g;

    /// View handed out by [`Self::acquire_write`] for the guard's lifetime.
    type WriteView<'g>
    where
        Self: 'g;

    /// Acquire scoped read access to `surface`, blocking until ready.
    ///
    /// Returns a [`ReadGuard`] whose `Drop` releases the access. Several
    /// concurrent readers are permitted; a writer is exclusive.
    #[allow(clippy::missing_errors_doc)]
    fn acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'g, Self>, AdapterError>;

    /// Acquire scoped exclusive write access to `surface`, blocking until ready.
    #[allow(clippy::missing_errors_doc)]
    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError>;

    /// Try to acquire read access without blocking.
    ///
    /// Returns `Ok(Some(guard))` on success, `Ok(None)` if a writer
    /// currently holds the surface (no error — caller decides whether
    /// to back off, retry, or skip the frame), or `Err(...)` on a
    /// genuine adapter failure.
    #[allow(clippy::missing_errors_doc)]
    fn try_acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'g, Self>>, AdapterError>;

    /// Try to acquire exclusive write access without blocking.
    ///
    /// Returns `Ok(Some(guard))` on success, `Ok(None)` if any reader
    /// or another writer currently holds the surface, or `Err(...)`
    /// on a genuine adapter failure.
    #[allow(clippy::missing_errors_doc)]
    fn try_acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'g, Self>>, AdapterError>;

    /// Sealed: signal the release-side timeline semaphore for a read.
    ///
    /// Called by [`ReadGuard::drop`]; not part of the public API.
    /// Implementations must be `&self`-callable and idempotent — the
    /// guard guarantees one call per acquired access.
    #[doc(hidden)]
    fn end_read_access(&self, surface_id: SurfaceId);

    /// Sealed: signal the release-side timeline semaphore for a write.
    ///
    /// Called by [`WriteGuard::drop`]; not part of the public API.
    #[doc(hidden)]
    fn end_write_access(&self, surface_id: SurfaceId);
}

/// Opaque Vulkan `VkImage` handle.
///
/// Vulkan handles are 64-bit by spec. Convert to your binding via
/// `ash::vk::Image::from_raw(handle.0)` or
/// `vulkanalia::vk::Image::from_raw(handle.0)`. Kept as a newtype here
/// so this crate stays binding-agnostic and dependency-light.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct VkImageHandle(pub u64);

/// Vulkan `VkImageLayout` enumerant value.
///
/// `VkImageLayout` is a 32-bit signed enum per the Vulkan spec.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct VkImageLayoutValue(pub i32);

/// Capability marker for views that expose a Vulkan `VkImage`.
///
/// Outer adapters that compose on Vulkan (custom RHIs, basic
/// Vulkan-on-Vulkan composition) constrain their inner adapter via
/// `for<'g> Inner::WriteView<'g>: VulkanWritable`. This marker is
/// minimal — image handle plus current layout — covering callers that
/// only need to issue Vulkan commands against the image.
///
/// Frameworks that need a richer description of the underlying
/// `VkImage` (Skia's `GrVkImageInfo`, debug snapshots, serialization)
/// should also require [`VulkanImageInfoExt`] on the inner view.
pub trait VulkanWritable {
    fn vk_image(&self) -> VkImageHandle;
    fn vk_image_layout(&self) -> VkImageLayoutValue;
}

/// Extended Vulkan image metadata required for compositing into
/// frameworks that need full `VkImage` description.
///
/// Skia's `GrVkImageInfo`, vulkano's `Image`, and similar APIs need
/// the memory binding plus tiling / format / usage / sample-count /
/// queue-family details to wrap an externally-allocated image. This
/// trait exposes a single `#[repr(C)] VkImageInfo` struct carrying
/// all of them, with reserved bytes for additive ABI extensions
/// before the next major bump.
///
/// Implement this on a view in addition to [`VulkanWritable`] when
/// the adapter has the full information; outer adapters that only
/// need the image handle keep the simpler `VulkanWritable` bound.
pub trait VulkanImageInfoExt: VulkanWritable {
    fn vk_image_info(&self) -> VkImageInfo;
}

/// Vulkan image description carried by [`VulkanImageInfoExt`].
///
/// Field types intentionally use raw integers (the underlying Vulkan
/// enums and bitmasks are all `u32` / `i32` / `u64` per the spec) so
/// this crate remains binding-agnostic.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VkImageInfo {
    /// `VkFormat` enumerant. `i32` per spec.
    pub format: i32,
    /// `VkImageTiling` enumerant. `i32` per spec.
    pub tiling: i32,
    /// `VkImageUsageFlags` bitmask.
    pub usage_flags: u32,
    /// `VkSampleCountFlagBits` bitmask (1 = `VK_SAMPLE_COUNT_1_BIT`).
    pub sample_count: u32,
    /// Number of mip levels.
    pub level_count: u32,
    /// Owning `VkQueue` family index.
    pub queue_family: u32,
    /// Opaque `VkDeviceMemory` handle.
    pub memory_handle: u64,
    /// Byte offset of the image within `memory_handle`.
    pub memory_offset: u64,
    /// Byte size of the image's region within `memory_handle`.
    pub memory_size: u64,
    /// `VkMemoryPropertyFlags` bitmask of the backing allocation.
    pub memory_property_flags: u32,
    /// 1 if the image was allocated `VK_IMAGE_CREATE_PROTECTED_BIT`.
    pub protected: u32,
    /// Opaque `VkSamplerYcbcrConversion` handle, or 0 if unused.
    pub ycbcr_conversion: u64,
    /// Reserved bytes for additive ABI extensions. MUST be zeroed.
    pub _reserved: [u8; 16],
}

/// Capability marker for views that expose an OpenGL texture id.
pub trait GlWritable {
    fn gl_texture_id(&self) -> u32;
}

/// Capability marker for views that expose CPU-readable bytes.
///
/// Strict capability marker — only `streamlib-adapter-cpu-readback`
/// implements this trait. Switching to that adapter is the contractual
/// signal that the customer has opted into a host-side GPU→CPU copy;
/// GPU adapters (`-vulkan`, `-opengl`, `-skia`) deliberately do not
/// satisfy it (enforced at compile time via `assert_not_impl_all!`).
///
/// Plane-aware shape: trait-generic callers iterating bytes through
/// `&dyn CpuReadable` should use [`Self::plane_count`] +
/// [`Self::plane_bytes`] to cover every plane of multi-plane formats
/// (NV12). [`Self::read_bytes`] returns plane 0 only — fine for
/// single-plane formats (BGRA/RGBA), silently drops chroma on NV12.
pub trait CpuReadable {
    /// Bytes of plane 0. Plane-0-only by design; use
    /// [`Self::plane_bytes`] for full multi-plane coverage.
    fn read_bytes(&self) -> &[u8];

    /// Number of planes in this view. Defaults to 1; multi-plane impls
    /// override.
    fn plane_count(&self) -> u32 {
        1
    }

    /// Bytes of plane `index`, row-major, tightly packed. Default impl
    /// preserves single-plane semantics: index `0` returns
    /// [`Self::read_bytes`] and any other index panics.
    fn plane_bytes(&self, index: u32) -> &[u8] {
        assert_eq!(
            index, 0,
            "plane_bytes: default impl only serves plane 0 (got {index})"
        );
        self.read_bytes()
    }
}

/// Capability marker for views that expose CPU-writable bytes.
///
/// Strict capability marker — see [`CpuReadable`] for the architectural
/// invariant. Plane-aware shape: trait-generic callers writing bytes
/// through `&mut dyn CpuWritable` should use [`Self::plane_bytes_mut`]
/// to cover every plane. [`Self::write_bytes`] is plane-0-only legacy.
pub trait CpuWritable {
    /// Mutable bytes of plane 0. Plane-0-only by design; use
    /// [`Self::plane_bytes_mut`] for full multi-plane coverage.
    fn write_bytes(&mut self) -> &mut [u8];

    /// Mutable bytes of plane `index`. Default impl preserves single-
    /// plane semantics: index `0` returns [`Self::write_bytes`] and any
    /// other index panics.
    fn plane_bytes_mut(&mut self, index: u32) -> &mut [u8] {
        assert_eq!(
            index, 0,
            "plane_bytes_mut: default impl only serves plane 0 (got {index})"
        );
        self.write_bytes()
    }
}
