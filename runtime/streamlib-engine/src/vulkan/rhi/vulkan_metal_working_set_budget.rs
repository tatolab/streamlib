// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Metal's recommended working set as the bound on frame allocations under
//! MoltenVK, and the IOSurface bytes this runtime mints that Metal does not
//! see until a Metal object wraps them.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice};
use vulkanalia::vk::{self, HasBuilder as _};

use super::vulkan_frame_allocation_device_memory_budget::{
    FrameDeviceMemoryPlacement, FrameDeviceMemoryRequest, MetalWorkingSetBudgetReading,
};

/// The size IOSurface rounds a surface's allocation up to on Apple silicon.
const IOSURFACE_ALLOCATION_GRANULE_BYTES: u64 = 16 * 1024;

/// Whether Metal's `currentAllocatedSize` already counts an IOSurface-backed
/// MoltenVK image; the macOS lane's frame-allocation budget step measures it.
const METAL_COUNTS_AN_IOSURFACE_BACKED_VULKAN_IMAGE_IN_ITS_CURRENT_ALLOCATED_SIZE: bool = false;

/// Whether Metal's `currentAllocatedSize` already counts an IOSurface imported
/// as a host-pointer buffer; the macOS lane's frame-allocation budget step measures it.
const METAL_COUNTS_AN_IOSURFACE_IMPORTED_AS_A_HOST_POINTER_BUFFER_IN_ITS_CURRENT_ALLOCATED_SIZE:
    bool = false;

/// The request a private IOSurface of `height` rows of `width` elements makes:
/// its packed bytes rounded up to IOSurface's allocation granule, since the
/// surface allocates when it is created.
pub(crate) fn frame_device_memory_request_for_a_private_iosurface(
    width: u32,
    height: u32,
    bytes_per_element: u32,
    frame_allocation_site: &'static str,
) -> FrameDeviceMemoryRequest {
    let packed_bytes = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(u64::from(bytes_per_element));
    FrameDeviceMemoryRequest {
        requested_bytes: packed_bytes.div_ceil(IOSURFACE_ALLOCATION_GRANULE_BYTES)
            * IOSURFACE_ALLOCATION_GRANULE_BYTES,
        placement: FrameDeviceMemoryPlacement::PrivateIOSurface,
        frame_allocation_site,
    }
}

/// The Metal device MoltenVK drives, held to read its working set.
pub(crate) struct MetalDeviceWhoseWorkingSetBoundsFrameAllocations(
    Retained<ProtocolObject<dyn MTLDevice>>,
);

impl MetalDeviceWhoseWorkingSetBoundsFrameAllocations {
    /// The `MTLDevice` behind `device`, through `VK_EXT_metal_objects`; `None`
    /// when the extension is not enabled or MoltenVK hands back none.
    pub(crate) fn exported_from_moltenvk(device: &vulkanalia::Device) -> Option<Self> {
        use vulkanalia::vk::ExtMetalObjectsExtensionDeviceCommands;

        if !device
            .extensions()
            .contains(&vk::EXT_METAL_OBJECTS_EXTENSION.name)
        {
            return None;
        }
        let mut metal_device_info = vk::ExportMetalDeviceInfoEXT::builder().build();
        let mut metal_objects_info = vk::ExportMetalObjectsInfoEXT::builder().build();
        metal_objects_info.next = (&mut metal_device_info as *mut _) as *const std::ffi::c_void;
        // SAFETY: the extension is enabled (checked above) and the instance
        // declared the MTLDevice export; MoltenVK writes the pointer back.
        unsafe { device.export_metal_objects_ext(&mut metal_objects_info) };
        let metal_device_pointer =
            metal_device_info.mtl_device as *mut ProtocolObject<dyn MTLDevice>;
        // SAFETY: MoltenVK returns its own `MTLDevice`, alive for the
        // `VkDevice`'s lifetime and not retained for the caller, so it is
        // retained here.
        unsafe { Retained::retain(metal_device_pointer) }.map(Self)
    }

    /// The system's default Metal device.
    pub(crate) fn the_systems_default() -> Option<Self> {
        MTLCreateSystemDefaultDevice().map(Self)
    }

    /// `recommendedMaxWorkingSetSize`: the bytes this device can use with good performance.
    pub(crate) fn recommended_max_working_set_bytes(&self) -> u64 {
        self.0.recommendedMaxWorkingSetSize()
    }

    /// `currentAllocatedSize`: the bytes of every resource this device has allocated.
    pub(crate) fn current_allocated_bytes(&self) -> u64 {
        self.0.currentAllocatedSize() as u64
    }
}

/// Which kind of resource a runtime-minted IOSurface backs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeMintedIOSurfaceKind {
    /// An image created over the surface.
    BackingAVulkanImage,
    /// A buffer importing the surface's pages as host memory.
    BackingAHostPointerImportedBuffer,
}

/// Bytes of the private IOSurfaces this runtime minted and still holds, per kind.
#[derive(Default)]
pub(crate) struct RuntimeMintedIOSurfaceByteTally {
    backing_vulkan_images: AtomicU64,
    backing_host_pointer_imported_buffers: AtomicU64,
}

impl RuntimeMintedIOSurfaceByteTally {
    fn bytes_of_kind(&self, kind: RuntimeMintedIOSurfaceKind) -> &AtomicU64 {
        match kind {
            RuntimeMintedIOSurfaceKind::BackingAVulkanImage => &self.backing_vulkan_images,
            RuntimeMintedIOSurfaceKind::BackingAHostPointerImportedBuffer => {
                &self.backing_host_pointer_imported_buffers
            }
        }
    }

    /// The tallied bytes of every kind Metal's `currentAllocatedSize` misses.
    fn bytes_metal_does_not_count(&self) -> u64 {
        let backing_vulkan_images =
            if METAL_COUNTS_AN_IOSURFACE_BACKED_VULKAN_IMAGE_IN_ITS_CURRENT_ALLOCATED_SIZE {
                0
            } else {
                self.backing_vulkan_images.load(Ordering::Relaxed)
            };
        let backing_host_pointer_imported_buffers =
            if METAL_COUNTS_AN_IOSURFACE_IMPORTED_AS_A_HOST_POINTER_BUFFER_IN_ITS_CURRENT_ALLOCATED_SIZE
            {
                0
            } else {
                self.backing_host_pointer_imported_buffers
                    .load(Ordering::Relaxed)
            };
        backing_vulkan_images.saturating_add(backing_host_pointer_imported_buffers)
    }
}

/// One minted IOSurface's bytes in the tally, taken back when it drops.
pub(crate) struct IOSurfaceBytesCountedInTheMetalWorkingSet {
    tally: Arc<RuntimeMintedIOSurfaceByteTally>,
    kind: RuntimeMintedIOSurfaceKind,
    iosurface_alloc_size_bytes: u64,
}

impl Drop for IOSurfaceBytesCountedInTheMetalWorkingSet {
    fn drop(&mut self) {
        self.tally
            .bytes_of_kind(self.kind)
            .fetch_sub(self.iosurface_alloc_size_bytes, Ordering::Relaxed);
    }
}

/// The Metal working set frame allocations on this device are weighed against.
pub(crate) struct MetalWorkingSetOfTheDevice {
    metal_device: MetalDeviceWhoseWorkingSetBoundsFrameAllocations,
    runtime_minted_iosurface_byte_tally: Arc<RuntimeMintedIOSurfaceByteTally>,
}

impl MetalWorkingSetOfTheDevice {
    /// The working set of `metal_device`, with nothing minted yet.
    pub(crate) fn new(metal_device: MetalDeviceWhoseWorkingSetBoundsFrameAllocations) -> Self {
        Self {
            metal_device,
            runtime_minted_iosurface_byte_tally: Arc::new(
                RuntimeMintedIOSurfaceByteTally::default(),
            ),
        }
    }

    /// The working set and this runtime's use of it, read live.
    pub(crate) fn reading(&self) -> MetalWorkingSetBudgetReading {
        MetalWorkingSetBudgetReading {
            recommended_max_working_set_bytes: self
                .metal_device
                .recommended_max_working_set_bytes(),
            metal_current_allocated_bytes: self.metal_device.current_allocated_bytes(),
            runtime_minted_iosurface_bytes_metal_does_not_count: self
                .runtime_minted_iosurface_byte_tally
                .bytes_metal_does_not_count(),
        }
    }

    /// Count a just-minted IOSurface's bytes until the returned value drops.
    pub(crate) fn count_a_minted_iosurface(
        &self,
        kind: RuntimeMintedIOSurfaceKind,
        iosurface_alloc_size_bytes: u64,
    ) -> IOSurfaceBytesCountedInTheMetalWorkingSet {
        self.runtime_minted_iosurface_byte_tally
            .bytes_of_kind(kind)
            .fetch_add(iosurface_alloc_size_bytes, Ordering::Relaxed);
        IOSurfaceBytesCountedInTheMetalWorkingSet {
            tally: Arc::clone(&self.runtime_minted_iosurface_byte_tally),
            kind,
            iosurface_alloc_size_bytes,
        }
    }

    /// Metal's own `currentAllocatedSize`, unadjusted.
    #[cfg(test)]
    pub(crate) fn metal_current_allocated_bytes(&self) -> u64 {
        self.metal_device.current_allocated_bytes()
    }
}

#[cfg(test)]
mod tests;
