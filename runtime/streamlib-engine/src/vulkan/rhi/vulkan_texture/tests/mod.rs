// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Fixtures shared by the Vulkan texture's tests.

#[cfg(target_os = "linux")]
use super::linux_dma_buf_export_and_import::dma_buf_plane_layout_query_aspect_mask;
use super::*;

use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};

#[cfg(target_os = "linux")]
use crate::core::test_support::rig_only_capability_skip::{
    RigOnlyCapability, skip_because_this_machine_lacks_naming_the_refusal,
    skip_unless_this_machine_has,
};
use crate::vulkan::rhi::HostVulkanDevice;
#[cfg(target_os = "linux")]
use crate::vulkan::rhi::dma_buf_export_test_device_fixture::a_device_that_exports_dma_buf_or_skip;
use vma::Alloc as _;

#[cfg(target_os = "linux")]
mod dma_buf_plane_layout_export_and_render_target_tests;
#[cfg(target_os = "linux")]
mod opaque_fd_export_and_import_tests;
mod texture_creation_lifetime_and_iosurface_tests;
