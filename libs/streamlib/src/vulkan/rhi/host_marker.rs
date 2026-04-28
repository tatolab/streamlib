// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! [`HostMarker`] — privilege flavor for host-side Vulkan resources.
//!
//! `HostMarker` lives in `streamlib` (not in `streamlib-consumer-rhi`)
//! because its [`DevicePrivilege`] impl associates the marker with
//! [`HostVulkanTexture`] and [`HostVulkanTimelineSemaphore`] —
//! streamlib-side types that the consumer crate cannot name without a
//! circular dep. Putting the marker here also keeps the orphan rule
//! satisfied: at least one of the trait or the impl-target type is
//! local.
//!
//! [`crate::vulkan::rhi::ConsumerMarker`] (the consumer flavor) is
//! re-exported from `streamlib_consumer_rhi`.

#[cfg(target_os = "linux")]
use streamlib_consumer_rhi::{private as consumer_rhi_private, DevicePrivilege};

/// Privilege marker for host-side Vulkan resources — full RHI access
/// (allocation, queue submit, modifier choice, kernel construction,
/// swapchain).
pub struct HostMarker;

// Seal the [`DevicePrivilege`] hierarchy across the two crates: the
// supertrait `Sealed` lives in `streamlib-consumer-rhi::private` and
// is implemented here for the streamlib-side marker so external
// crates cannot invent their own privilege flavors.
#[cfg(target_os = "linux")]
impl consumer_rhi_private::Sealed for HostMarker {}

#[cfg(target_os = "linux")]
impl DevicePrivilege for HostMarker {
    type TimelineSemaphore = super::HostVulkanTimelineSemaphore;
    type Texture = super::HostVulkanTexture;
}

// Non-Linux: HostMarker still resolves but to phantom unit types for
// platforms where the DMA-BUF / OPAQUE_FD machinery isn't built.
// `ConsumerMarker` only exists on Linux today.
#[cfg(not(target_os = "linux"))]
mod placeholder {
    use streamlib_consumer_rhi::{
        DevicePrivilege, TextureFormat, VulkanTextureLike, VulkanTimelineSemaphoreLike,
    };

    use super::HostMarker;

    /// Phantom type for platforms where DMA-BUF / OPAQUE_FD primitives
    /// aren't built. Stays uninstantiable so trait bounds resolve at
    /// type-check time but no caller can construct one.
    pub enum NotAvailableOnThisPlatform {}

    impl DevicePrivilege for HostMarker {
        type TimelineSemaphore = NotAvailableOnThisPlatform;
        type Texture = NotAvailableOnThisPlatform;
    }

    impl VulkanTimelineSemaphoreLike for NotAvailableOnThisPlatform {
        fn wait(&self, _value: u64, _timeout_ns: u64) -> streamlib_consumer_rhi::Result<()> {
            match *self {}
        }
        fn signal_host(&self, _value: u64) -> streamlib_consumer_rhi::Result<()> {
            match *self {}
        }
    }

    impl VulkanTextureLike for NotAvailableOnThisPlatform {
        fn image(&self) -> Option<vulkanalia::vk::Image> {
            match *self {}
        }
        fn chosen_drm_format_modifier(&self) -> u64 {
            match *self {}
        }
        fn width(&self) -> u32 {
            match *self {}
        }
        fn height(&self) -> u32 {
            match *self {}
        }
        fn format(&self) -> TextureFormat {
            match *self {}
        }
    }
}
