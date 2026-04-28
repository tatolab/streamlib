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

/// Sealing supertrait module for [`DevicePrivilege`]. Re-exported so
/// `streamlib::vulkan::rhi::HostMarker` can `impl Sealed for HostMarker`
/// from the streamlib crate. See [`device_capability::private`].
#[cfg(target_os = "linux")]
#[doc(hidden)]
pub use device_capability::private;

/// Compile-time capability checks for the consumer-rhi boundary.
///
/// These doctests are a *narrow* layer of the boundary story:
/// they assert that **`streamlib-consumer-rhi` itself does not
/// depend on `streamlib`** — the doctest compilation context
/// inherits only this crate's `[dependencies]` and
/// `[dev-dependencies]`, so any `use streamlib::*` fails to
/// resolve. If a future change ever pulls `streamlib` into this
/// crate's deps, these `compile_fail` tests start compiling and
/// the suite fails, catching the regression.
///
/// What these doctests do **NOT** prove on their own: that the
/// polyglot cdylibs (which depend on this crate *plus*
/// `streamlib-adapter-vulkan`, `streamlib-adapter-opengl`,
/// `streamlib-adapter-abi`, etc.) can't reach `streamlib` through
/// a different path. That stronger property is asserted by:
///
/// - `cargo tree -p streamlib-{python,deno}-native | grep -c "^streamlib v"`
///   returning 0 (documented in `.claude/workflows/polyglot.md`).
/// - The polyglot adapter crates (`streamlib-adapter-vulkan`,
///   `streamlib-adapter-opengl`) holding `streamlib` in
///   `[dev-dependencies]` only, with the helper-bin moved to
///   `streamlib-adapter-vulkan-helpers`.
///
/// Together those two checks plus the doctests below give the full
/// boundary; this module is one piece of three.
///
/// ```compile_fail
/// // HostVulkanDevice is host-only. From this crate's dep graph the
/// // `streamlib` crate is not present and the import does not
/// // resolve.
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
/// The seal on [`DevicePrivilege`] keeps external crates from
/// **accidentally** inventing their own privilege flavors. `Sealed`
/// lives in [`private`] (`pub` so streamlib can implement it on
/// `HostMarker` across the crate boundary, but `#[doc(hidden)]` so
/// rustdoc doesn't surface it). The default failure mode for an
/// outside crate is an unsatisfied trait bound:
///
/// ```compile_fail
/// // External marker doesn't implement Sealed → can't impl
/// // DevicePrivilege on it.
/// use streamlib_consumer_rhi::{
///     ConsumerVulkanTexture, ConsumerVulkanTimelineSemaphore, DevicePrivilege,
/// };
///
/// pub struct OutsideMarker;
///
/// impl DevicePrivilege for OutsideMarker {
///     type TimelineSemaphore = ConsumerVulkanTimelineSemaphore;
///     type Texture = ConsumerVulkanTexture;
/// }
/// ```
///
/// This is the standard cross-crate seal trade-off (serde / std use
/// the same pattern): the seal is **convention with friction** —
/// `private` is reachable, named with an `__`-style hint that it's
/// not part of the API, and a determined external crate can still
/// write `impl streamlib_consumer_rhi::private::Sealed for MyMarker`
/// before implementing `DevicePrivilege`. The hard guarantee is
/// against accidental implementation; the looser guarantee is that
/// any deliberate breach is visible at the call site.
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
