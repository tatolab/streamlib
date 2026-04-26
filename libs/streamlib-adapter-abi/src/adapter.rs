// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `SurfaceAdapter` trait, ABI version constant, and capability marker traits.

use crate::error::AdapterError;
use crate::guard::{ReadGuard, WriteGuard};
use crate::surface::{StreamlibSurface, SurfaceId};

/// Major version of the surface-adapter ABI.
///
/// The runtime refuses adapters whose [`SurfaceAdapter::trait_version`]
/// reports a different major. Adding methods to the trait is a minor
/// (non-breaking) change; renaming/removing one is a major.
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

    /// Acquire scoped read access to `surface`.
    ///
    /// Returns a [`ReadGuard`] whose `Drop` releases the access. Several
    /// concurrent readers are permitted; a writer is exclusive.
    #[allow(clippy::missing_errors_doc)]
    fn acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'g, Self>, AdapterError>;

    /// Acquire scoped exclusive write access to `surface`.
    #[allow(clippy::missing_errors_doc)]
    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError>;

    /// ABI version this adapter was compiled against.
    ///
    /// Defaults to [`STREAMLIB_ADAPTER_ABI_VERSION`] — the runtime
    /// refuses adapters whose major doesn't match.
    fn trait_version(&self) -> u32 {
        STREAMLIB_ADAPTER_ABI_VERSION
    }

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
/// Outer adapters that compose on Vulkan (Skia, custom RHIs) constrain
/// their inner adapter via `for<'g> Inner::WriteView<'g>: VulkanWritable`
/// — the inner view is consumed by the outer adapter only. Customers
/// of the outer adapter never see the inner view.
///
/// The [`Self::vk_image_layout`] accessor is a deliberate escape hatch
/// for the Skia adapter: Skia's `GrVkImageInfo` requires the current
/// layout to build a backend context. Customers of `SurfaceAdapter`
/// itself never see it.
pub trait VulkanWritable {
    fn vk_image(&self) -> VkImageHandle;
    fn vk_image_layout(&self) -> VkImageLayoutValue;
}

/// Capability marker for views that expose an OpenGL texture id.
pub trait GlWritable {
    fn gl_texture_id(&self) -> u32;
}

/// Capability marker for views that expose a CPU-readable byte slice.
pub trait CpuReadable {
    fn read_bytes(&self) -> &[u8];
}

/// Capability marker for views that expose a CPU-writable byte slice.
pub trait CpuWritable {
    fn write_bytes(&mut self) -> &mut [u8];
}
