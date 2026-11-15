// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Consumer-side Vulkan device — sandboxed subset of the RHI for
//! subprocess (cdylib) use.
//!
//! The consumer-side device deliberately *does not* duplicate
//! `streamlib::vulkan::rhi::HostVulkanDevice`. Where the host device
//! owns the privileged state — VMA allocator + DMA-BUF export pools,
//! DRM modifier probe, transfer / encode / decode / compute queues,
//! the per-queue submit-mutex matrix — the consumer device holds only what the carve-out in
//! the subprocess RHI parity rule permits:
//!
//! - DMA-BUF FD import (`vkAllocateMemory` with `VkImportMemoryFdInfoKHR`).
//! - Tiled-image import via `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`
//!   (the modifier comes pre-chosen from the host's descriptor).
//! - Layout transitions on imported handles, single-shot at the
//!   acquire/release boundary — submitted via the device's one queue.
//! - `vkMapMemory` / `vkUnmapMemory` on imported memory.
//! - `vkWaitSemaphores` / `vkSignalSemaphore` on imported timeline
//!   semaphores (lives on
//!   [`crate::ConsumerVulkanTimelineSemaphore`]).
//!
//! Crucially, this struct is constructed via
//! [`ConsumerVulkanDevice::new_on_the_device_the_runtime_named`], which spins
//! up its *own* `VkInstance` + `VkDevice`. It does **not** share the
//! host's logical device — that's load-bearing for the
//! capability-boundary story (the cdylib never holds a reference to
//! the host's `vulkanalia::Device`). The two devices target the same
//! physical GPU but operate on disjoint Vulkan state objects, which
//! also sidesteps the dual-`VkDevice` crash on NVIDIA Linux as long as
//! their queue submissions don't overlap (the carve-out guarantees the
//! consumer only submits at acquire/release boundaries while the host
//! is paused).

use std::ffi::{CStr, c_char};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::vulkan_extension_names::vulkan_extension_names_borrowed_from_properties;
use crate::{ConsumerMarker, ConsumerRhiError, Result, VulkanRhiDevice};

mod consumer_vulkan_device_accessors_and_capabilities;
mod consumer_vulkan_device_creation;
#[cfg(target_os = "macos")]
mod consumer_vulkan_device_macos_metal_interop;
mod consumer_vulkan_device_memory_import;
mod consumer_vulkan_device_queue_submission;
mod consumer_vulkan_instance_and_device_selection;
#[cfg(test)]
mod import_device_extension_partition_tests;
#[cfg(test)]
mod selection_by_the_device_uuid_the_runtime_named_tests;
#[cfg(test)]
mod stated_memory_type_index_bind_tests;
#[cfg(test)]
mod tests;

pub use consumer_vulkan_instance_and_device_selection::vulkan_device_uuid_of_the_first_device_the_loader_enumerates;
use consumer_vulkan_instance_and_device_selection::{
    create_the_consumer_vulkan_instance, physical_devices_the_loader_enumerates_on,
};

/// Single-COLOR-aspect / single-mip / single-layer subresource range —
/// every surface-adapter-managed image fits this shape today.
fn default_color_subresource_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

/// Whether `device_extension` was enabled on `device`.
fn device_extension_is_enabled(device: &Device, device_extension: vk::Extension) -> bool {
    device.extensions().contains(&device_extension.name)
}

/// Device extensions every import on the platform needs; the device refuses
/// to construct without any of them.
#[cfg(target_os = "linux")]
const REQUIRED_DEVICE_EXTENSIONS: &[&CStr] =
    &[c"VK_KHR_external_memory", c"VK_KHR_external_memory_fd"];

/// Device extensions the platform's imports need. MoltenVK has no fd
/// handles; an IOSurface's pages import as host memory.
#[cfg(target_os = "macos")]
const REQUIRED_DEVICE_EXTENSIONS: &[&CStr] = &[c"VK_EXT_external_memory_host"];

/// Device extensions enabled where the driver advertises them, short of
/// [`PROBED_DEVICE_EXTENSIONS_WITHHELD_FROM_A_SOFTWARE_DEVICE`]. Each arm that
/// needs one refuses by name on a device without it rather than the whole
/// device refusing.
#[cfg(target_os = "linux")]
const PROBED_DEVICE_EXTENSIONS: &[&CStr] = &[
    c"VK_EXT_external_memory_dma_buf",
    c"VK_EXT_image_drm_format_modifier",
    c"VK_KHR_external_semaphore_fd",
];

/// Device extensions enabled where the driver advertises them.
#[cfg(target_os = "macos")]
const PROBED_DEVICE_EXTENSIONS: &[&CStr] = &[];

/// Probed extensions a CPU-type device never enables, whatever it advertises:
/// DMA-BUF is absent on the software device by rule, so whether `/dev/udmabuf`
/// is reachable never decides what a helper there can import.
const PROBED_DEVICE_EXTENSIONS_WITHHELD_FROM_A_SOFTWARE_DEVICE: &[&CStr] = &[
    c"VK_EXT_external_memory_dma_buf",
    c"VK_EXT_image_drm_format_modifier",
];

/// The import extensions to enable out of `advertised`: every `required` one,
/// then each `probed` one the driver advertises and `withheld` does not name.
/// `Err` carries the first required extension the driver lacks.
fn import_device_extensions_to_enable<'extension_name>(
    advertised: &[&CStr],
    required: &[&'extension_name CStr],
    probed: &[&'extension_name CStr],
    withheld: &[&CStr],
) -> std::result::Result<Vec<&'extension_name CStr>, &'extension_name CStr> {
    let mut enabled = Vec::with_capacity(required.len() + probed.len());
    for required_extension in required {
        if !advertised.contains(required_extension) {
            return Err(required_extension);
        }
        enabled.push(*required_extension);
    }
    enabled.extend(probed.iter().copied().filter(|probed_extension| {
        advertised.contains(probed_extension) && !withheld.contains(probed_extension)
    }));
    Ok(enabled)
}

/// Consumer-only Vulkan device — see module docs.
pub struct ConsumerVulkanDevice {
    entry: vulkanalia::Entry,
    instance: vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    device: vulkanalia::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    device_name: String,
    /// Whether the physical device is CPU-type — a software rasterizer.
    device_is_software: bool,
    /// Whether `VK_EXT_external_memory_acquire_unmodified` was enabled
    /// at device creation. Lets the consumer's acquire barrier chain
    /// `VkExternalMemoryAcquireUnmodifiedEXT` so the producer's
    /// post-release contents survive the QFOT acquire (instead of
    /// being discarded by the spec's UNDEFINED equivalence). When
    /// false, fall back to UNDEFINED → target on acquire — content
    /// preservation is then driver-empirical, not spec-guaranteed.
    /// `VK_QUEUE_FAMILY_EXTERNAL` (the queue family index used for the
    /// QFOT release/acquire src/dst) is core Vulkan 1.1 and always
    /// available; only this acquire-side extension is the meaningful
    /// gate.
    has_acquire_unmodified: bool,
    /// `VkPhysicalDeviceIDProperties::deviceUUID` — 16-byte unique
    /// identifier the driver assigns to the physical device. Mirrors
    /// the same field on the host-side device. CUDA / OpenCL interop
    /// matches this against `cudaDeviceProp::uuid` to pick the
    /// matching CUDA device on multi-GPU rigs; without the match
    /// CUDA's default device may differ from the Vulkan-selected
    /// physical device and OPAQUE_FD imports land on the wrong GPU.
    physical_device_uuid: [u8; 16],
    /// `minImportedHostPointerAlignment` when `VK_EXT_external_memory_host`
    /// is enabled — the Apple arm's IOSurface import — and `None` otherwise.
    imported_host_pointer_alignment: Option<vk::DeviceSize>,
    /// Live count of memory allocations made via [`Self::import_dma_buf_memory`]
    /// (raw `vkAllocateMemory`). Mirrors the host counterpart; surfaces leaks
    /// on drop via the warning emitted in [`Drop`].
    live_allocation_count: AtomicUsize,
    /// The video encode or decode queue an owned-tier device was created
    /// with, and the codec extensions enabled for it; `None` on an
    /// import-only device.
    video_codec_queue: Option<ConsumerVulkanVideoCodecQueue>,
    /// Mutex serializing `vkQueueSubmit2` and `vkDeviceWaitIdle` — Vulkan
    /// requires external synchronization on a `VkQueue` from multiple
    /// threads. One mutex covers the import queue and the video codec
    /// queue alike.
    queue_mutex: Mutex<()>,
    /// The loader's `vkGetInstanceProcAddr`, valid while [`Self::entry`]
    /// keeps the loader library open.
    #[cfg(target_os = "linux")]
    get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    /// The `VkPhysicalDeviceFeatures` the device was created with.
    #[cfg(target_os = "linux")]
    enabled_physical_device_features: vk::PhysicalDeviceFeatures,
}

/// Which way a Vulkan Video queue codes: pictures into a bitstream, or a
/// bitstream into pictures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VulkanVideoCodecDirection {
    /// A `VIDEO_ENCODE` queue family with the encode extensions.
    Encode,
    /// A `VIDEO_DECODE` queue family with the decode extensions.
    Decode,
}

/// The video queue of a device created for Vulkan Video encode or decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumerVulkanVideoCodecQueue {
    /// Whether the queue encodes or decodes.
    pub direction: VulkanVideoCodecDirection,
    /// The `VkQueue` codec work is submitted to.
    pub queue: vk::Queue,
    /// The family that owns [`Self::queue`].
    pub queue_family_index: u32,
    /// Whether the H.265 extension for [`Self::direction`] was enabled beside
    /// H.264's.
    pub supports_h265: bool,
}

#[cfg(target_os = "linux")]
impl VulkanVideoCodecDirection {
    /// The queue capability a family needs to code this way.
    fn queue_flag(self) -> vk::QueueFlags {
        match self {
            Self::Encode => vk::QueueFlags::VIDEO_ENCODE_KHR,
            Self::Decode => vk::QueueFlags::VIDEO_DECODE_KHR,
        }
    }

    /// Device extensions a device coding this way needs; it refuses to
    /// construct without any of them.
    fn required_device_extensions(self) -> &'static [&'static CStr] {
        match self {
            Self::Encode => &[
                c"VK_KHR_video_queue",
                c"VK_KHR_video_encode_queue",
                c"VK_KHR_video_encode_h264",
            ],
            Self::Decode => &[
                c"VK_KHR_video_queue",
                c"VK_KHR_video_decode_queue",
                c"VK_KHR_video_decode_h264",
            ],
        }
    }

    /// The H.265 extension enabled when the device advertises it.
    fn h265_device_extension(self) -> &'static CStr {
        match self {
            Self::Encode => c"VK_KHR_video_encode_h265",
            Self::Decode => c"VK_KHR_video_decode_h265",
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Self::Encode => "encode",
            Self::Decode => "decode",
        }
    }
}

/// What a consumer device is created for beyond importing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsumerVulkanDeviceQueues {
    /// The one GRAPHICS queue imports and layout transitions run on.
    ImportOnly,
    /// That queue plus a Vulkan Video queue coding the given way.
    #[cfg(target_os = "linux")]
    ImportAndVideoCodec(VulkanVideoCodecDirection),
    /// That queue, with the core rasterization features an embedded renderer
    /// draws with enabled wherever the device supports them.
    #[cfg(target_os = "linux")]
    ExternalRenderer,
}

/// The core features a device for an embedded renderer enables out of
/// `supported`: each one a vector renderer's pipelines may use, and none other.
#[cfg(target_os = "linux")]
fn physical_device_features_an_external_renderer_draws_with(
    supported: &vk::PhysicalDeviceFeatures,
) -> vk::PhysicalDeviceFeatures {
    vk::PhysicalDeviceFeatures {
        independent_blend: supported.independent_blend,
        fill_mode_non_solid: supported.fill_mode_non_solid,
        fragment_stores_and_atomics: supported.fragment_stores_and_atomics,
        shader_clip_distance: supported.shader_clip_distance,
        sampler_anisotropy: supported.sampler_anisotropy,
        depth_bias_clamp: supported.depth_bias_clamp,
        ..Default::default()
    }
}

/// A physical device the Vulkan loader enumerates, as selection by
/// `deviceUUID` sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VulkanDeviceTheLoaderEnumerates {
    pub(crate) device_name: String,
    pub(crate) device_uuid: [u8; 16],
    pub(crate) device_type: vk::PhysicalDeviceType,
}

/// Every physical device the Vulkan loader enumerates for this process, in
/// the loader's order.
pub(crate) fn vulkan_devices_the_loader_enumerates() -> Result<Vec<VulkanDeviceTheLoaderEnumerates>>
{
    let (_entry, instance, _get_instance_proc_addr) = create_the_consumer_vulkan_instance()?;
    Ok(physical_devices_the_loader_enumerates_on(&instance)?
        .into_iter()
        .map(|(_physical_device, enumerated_device)| enumerated_device)
        .collect())
}

impl VulkanRhiDevice for ConsumerVulkanDevice {
    type Privilege = ConsumerMarker;

    fn instance(&self) -> &vulkanalia::Instance {
        &self.instance
    }

    fn physical_device(&self) -> vk::PhysicalDevice {
        self.physical_device
    }

    fn device(&self) -> &vulkanalia::Device {
        &self.device
    }

    fn queue(&self) -> vk::Queue {
        self.queue
    }

    fn queue_family_index(&self) -> u32 {
        self.queue_family_index
    }

    unsafe fn submit_to_queue(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> Result<()> {
        unsafe { ConsumerVulkanDevice::submit_to_queue(self, queue, submits, fence) }
    }

    fn supports_qfot_acquire_unmodified(&self) -> bool {
        ConsumerVulkanDevice::supports_qfot_acquire_unmodified(self)
    }

    fn release_to_foreign(
        &self,
        image: vk::Image,
        src_layout: vk::ImageLayout,
        dst_layout: vk::ImageLayout,
    ) -> Result<()> {
        ConsumerVulkanDevice::release_to_foreign(self, image, src_layout, dst_layout)
    }

    fn acquire_from_foreign(&self, image: vk::Image, target: vk::ImageLayout) -> Result<()> {
        ConsumerVulkanDevice::acquire_from_foreign(self, image, target)
    }
}

impl Drop for ConsumerVulkanDevice {
    fn drop(&mut self) {
        let live = self.live_allocation_count.load(Ordering::Relaxed);
        if live > 0 {
            tracing::warn!(
                "ConsumerVulkanDevice dropping with {} live DMA-BUF imports (leak)",
                live
            );
        }
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

/// A consumer device on the first device the loader enumerates — for a test
/// with no runtime to name one.
#[cfg(test)]
pub(crate) fn consumer_vulkan_device_on_the_first_device_the_loader_enumerates()
-> Result<ConsumerVulkanDevice> {
    ConsumerVulkanDevice::new_on_the_device_the_runtime_named(
        vulkan_device_uuid_of_the_first_device_the_loader_enumerates()?,
    )
}

// Vulkan handles are thread-safe behind external synchronization which
// the per-queue mutex provides for submits.
unsafe impl Send for ConsumerVulkanDevice {}
unsafe impl Sync for ConsumerVulkanDevice {}

/// Refuse a stated memory type index an imported resource's own
/// `VkMemoryRequirements::memoryTypeBits` cannot take — binding it anyway is
/// undefined behaviour rather than a Vulkan error.
///
/// `checked_shl` rather than a bare shift: an index at or past
/// VK_MAX_MEMORY_TYPES names no memory type on any device, and must be
/// refused rather than overflow the bit test.
pub(crate) fn refuse_unless_the_imported_resource_can_bind_the_stated_memory_type_index(
    importing_resource: &str,
    memory_type_bits: u32,
    stated_memory_type_index: u32,
) -> Result<()> {
    let stated_memory_type_bit = 1u32.checked_shl(stated_memory_type_index).unwrap_or(0);
    if memory_type_bits & stated_memory_type_bit != 0 {
        return Ok(());
    }
    Err(ConsumerRhiError::Configuration(format!(
        "{importing_resource}: the exporter states memory type index \
         {stated_memory_type_index}, which the imported resource cannot bind \
         (memoryTypeBits=0x{memory_type_bits:x}) — the exporter and importer disagree \
         on its shape, and binding anyway is undefined behaviour"
    )))
}
