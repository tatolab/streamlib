// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Consumer-side StreamLib RHI carve-out.
//!
//! Surface adapters and polyglot cdylibs depend on **this** crate
//! instead of the full `streamlib` so the `FullAccess` capability
//! boundary is enforced by the type system rather than by convention:
//! a cdylib whose dep graph excludes `streamlib` cannot reach
//! `HostVulkanDevice::new`, the host's VMA pools, the modifier probe,
//! or any other privileged primitive.
//!
//! What lives here:
//!
//! - [`ConsumerVulkanDevice`] — own `VkInstance` + `VkDevice` for the
//!   subprocess; only the carve-out methods listed in
//!   `docs/architecture/subprocess-rhi-parity.md` (DMA-BUF FD import +
//!   bind + map, single-shot layout transitions, sync wait/signal on
//!   imported timeline semaphores).
//! - [`ConsumerVulkanTexture`], [`ConsumerVulkanPixelBuffer`],
//!   [`ConsumerVulkanTimelineSemaphore`] — import-only resource
//!   wrappers paired with the consumer device.
//! - [`VulkanRhiDevice`], [`DevicePrivilege`], [`VulkanTextureLike`],
//!   [`VulkanTimelineSemaphoreLike`], [`ConsumerMarker`] — the trait
//!   machinery surface adapters (`streamlib-adapter-vulkan` etc.) use
//!   to abstract over device flavor.
//! - [`TextureFormat`], [`TextureUsages`], [`PixelFormat`] — RHI format
//!   primitives shared with the host side. `streamlib::core::rhi` and
//!   `streamlib::core::rhi::pixel_format` re-export these so existing
//!   call sites compile unchanged.
//! - [`ConsumerRhiError`] — thin error taxonomy for the carve-out.
//!   `streamlib::core::StreamError` provides a `From` impl, so the
//!   host side can wrap consumer errors with `?`.
//!
//! What does NOT live here: anything that calls `vkAllocateMemory`
//! without an `VkImportMemoryFdInfoKHR`, anything that builds a
//! `VkPipeline`, anything that owns a `VkQueue` typed for the host's
//! transfer / encode / decode / compute roles, anything tied to the
//! swapchain. Those stay in `streamlib::vulkan::rhi` and are reachable
//! only behind the host-side `GpuContextFullAccess` capability.

#![cfg_attr(docsrs, feature(doc_auto_cfg))]

mod error;
mod formats;
mod pixel_format;

#[cfg(target_os = "linux")]
mod consumer_vulkan_device;
#[cfg(target_os = "linux")]
mod consumer_vulkan_pixel_buffer;
#[cfg(target_os = "linux")]
mod consumer_vulkan_sync;
#[cfg(target_os = "linux")]
mod consumer_vulkan_texture;
#[cfg(target_os = "linux")]
mod device_capability;

pub use error::{ConsumerRhiError, Result};
pub use formats::{TextureFormat, TextureUsages};
pub use pixel_format::PixelFormat;

#[cfg(target_os = "linux")]
pub use consumer_vulkan_device::ConsumerVulkanDevice;
#[cfg(target_os = "linux")]
pub use consumer_vulkan_pixel_buffer::ConsumerVulkanPixelBuffer;
#[cfg(target_os = "linux")]
pub use consumer_vulkan_sync::ConsumerVulkanTimelineSemaphore;
#[cfg(target_os = "linux")]
pub use consumer_vulkan_texture::ConsumerVulkanTexture;
#[cfg(target_os = "linux")]
pub use device_capability::{
    ConsumerMarker, DevicePrivilege, VulkanRhiDevice, VulkanTextureLike,
    VulkanTimelineSemaphoreLike,
};

/// Compile-time capability checks proving FullAccess types are
/// unreachable from a `streamlib-consumer-rhi`-only dep graph.
///
/// These doctests verify (at compile time) that the type-system
/// boundary holds: a cdylib that depends on `streamlib-consumer-rhi`
/// alone — without `streamlib` in its `[dependencies]` *or*
/// `[dev-dependencies]` — cannot name host-only types like
/// `streamlib::vulkan::rhi::HostVulkanDevice` or
/// `streamlib::core::context::GpuContextFullAccess`. The doctest
/// compilation context inherits `streamlib-consumer-rhi`'s dep graph
/// only, so these imports failing to resolve is exactly the
/// cdylib-shaped consumer's view.
///
/// If `streamlib` is ever added to `streamlib-consumer-rhi`'s
/// `Cargo.toml`, these `compile_fail` tests will start compiling —
/// the `compile_fail` attribute then fires and the suite fails,
/// catching the regression.
///
/// ```compile_fail
/// // HostVulkanDevice is host-only. From a consumer-rhi-shaped dep
/// // graph the `streamlib` crate is not present and the import does
/// // not resolve.
/// use streamlib::vulkan::rhi::HostVulkanDevice;
/// fn _force(_: HostVulkanDevice) {}
/// ```
///
/// ```compile_fail
/// // GpuContextFullAccess is the privileged capability typestate.
/// use streamlib::core::context::GpuContextFullAccess;
/// fn _force(_: &GpuContextFullAccess) {}
/// ```
///
/// ```compile_fail
/// // The host's allocator-backed texture is host-only.
/// use streamlib::vulkan::rhi::HostVulkanTexture;
/// fn _force(_: HostVulkanTexture) {}
/// ```
///
/// ```compile_fail
/// // The transitional `streamlib::adapter_support` re-export this
/// // crate replaces was deleted alongside the crate landing.
/// use streamlib::adapter_support::HostVulkanDevice;
/// fn _force(_: HostVulkanDevice) {}
/// ```
///
/// Positive control: the consumer-side path compiles fine. Only
/// type-checked — no Vulkan loader call at test time.
///
/// ```
/// use streamlib_consumer_rhi::{
///     ConsumerMarker, ConsumerVulkanDevice, DevicePrivilege, VulkanRhiDevice,
/// };
///
/// fn _accepts_consumer<D: VulkanRhiDevice<Privilege = ConsumerMarker>>(_: &D) {}
///
/// #[cfg(target_os = "linux")]
/// type _ConsumerTexture = <ConsumerMarker as DevicePrivilege>::Texture;
///
/// // Constructor is reachable but not called — avoids hitting the
/// // Vulkan loader on doc-test runs.
/// fn _ctor_signature()
///     -> fn() -> Result<ConsumerVulkanDevice, streamlib_consumer_rhi::ConsumerRhiError>
/// {
///     ConsumerVulkanDevice::new
/// }
/// ```
#[doc(hidden)]
pub mod __capability_doctests {}
