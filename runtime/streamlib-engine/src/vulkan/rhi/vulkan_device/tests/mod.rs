// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::rhi::TextureDescriptor;
#[cfg(target_os = "linux")]
use crate::core::test_support::rig_only_capability_skip::{
    RigOnlyCapability, skip_because_this_machine_lacks,
    skip_because_this_machine_lacks_naming_the_refusal,
};
#[cfg(target_os = "linux")]
use crate::vulkan::rhi::HostVulkanTexture;
#[cfg(target_os = "linux")]
use crate::vulkan::rhi::dma_buf_export_test_device_fixture::a_device_that_exports_dma_buf_or_skip;

mod device_bring_up_and_queue_submission;
mod driver_ray_tracing_and_device_memory_budgets;
mod expected_selected_device;
mod export_pool_refusals_and_dma_buf_checks;
mod export_pool_sentinels_and_dma_buf_import;
mod queue_family_selection;

/// Try to create a HostVulkanDevice; return None if GPU/Vulkan is unavailable (CI).
fn try_create_device() -> Option<Arc<HostVulkanDevice>> {
    match HostVulkanDevice::new() {
        Ok(d) => Some(d),
        Err(e) => {
            println!("Skipping test — Vulkan not available: {e}");
            None
        }
    }
}
