// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use pyo3::prelude::*;
use pyo3::types::PyBytes;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::python_helper_process_pixel_exchange::HelperSurfaceCheckOutLeaseDebt;

/// A claim on a published surface, held for exactly as long as this object is.
///
/// While a claim is outstanding the pool never rehands that surface's slot to
/// its producer, and dropping this object is the release — there is nothing to
/// call. Ownership being the whole protocol is what lets any object that holds
/// one in a field inherit the behaviour: the frame stops moving while the
/// object that named it lives.
///
/// Claims are counted, so holding one and resolving the same surface for its
/// pixels are independent — neither releases the other's.
#[pyclass(name = "GpuSurfaceCheckOutLease", module = "streamlib", frozen)]
pub(crate) struct PythonGpuSurfaceCheckOutLease {
    pub(super) claimed_surface_id: String,
    /// Settled by its own `Drop`; nothing reads it, and that is the point.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[expect(dead_code, reason = "the field is the claim; its Drop is the release")]
    pub(super) release_check_out_to_surface_share: HelperSurfaceCheckOutLeaseDebt,
}

#[pymethods]
impl PythonGpuSurfaceCheckOutLease {
    /// The surface this claim holds still.
    #[getter]
    fn surface_id(&self) -> String {
        self.claimed_surface_id.clone()
    }
}

/// A raw OPAQUE_FD texture handle: the allocation's memory fd plus the
/// allocation-stable shape a foreign Vulkan or CUDA external-memory import
/// must reproduce.
///
/// Deliberately outside the `GpuSurface*` family prefix: the object names
/// an allocation, never a frame-bearing surface — the surface-id lifetime
/// guarantees end at export.
#[pyclass(name = "OpaqueFdTextureExport", module = "streamlib", frozen)]
pub(crate) struct PythonOpaqueFdTextureExport {
    exported_memory_fd: i32,
    allocation_byte_size: u64,
    width: u32,
    height: u32,
    format_wire_name: &'static str,
    vk_image_creation_recipe: ExportedVkImageCreationRecipe,
    dedicated_allocation: bool,
    export_contract: OpaqueFdExportContract,
}

/// The `VkImageCreateInfo` recipe an OPAQUE_FD export carries — the shape
/// a conforming foreign re-import must reproduce byte-for-byte. Declared
/// once and held by value by every owner between the wire parse and the
/// Python object, so a field added here reaches all of them.
#[derive(Clone, Copy)]
pub(crate) struct ExportedVkImageCreationRecipe {
    pub(crate) vk_image_tiling: i32,
    pub(crate) vk_image_usage_flags: u32,
    pub(crate) vk_image_mip_levels: u32,
    pub(crate) vk_image_array_layers: u32,
    pub(crate) vk_image_samples: i32,
}

/// The allocation-binding half of the raw-handle export contract: the
/// exporter's memory type index and device UUID travel together — an
/// OPAQUE_FD registration carries both or its checkout is refused, so
/// one-without-the-other is unrepresentable.
#[derive(Clone, Copy)]
pub(crate) struct OpaqueFdExportContract {
    pub(crate) vk_memory_type_index: u32,
    pub(crate) exporting_device_uuid: [u8; 16],
}

#[cfg(target_os = "linux")]
impl From<crate::python_helper_process_pixel_exchange::OpaqueFdTextureExportDescription>
    for PythonOpaqueFdTextureExport
{
    fn from(
        description: crate::python_helper_process_pixel_exchange::OpaqueFdTextureExportDescription,
    ) -> Self {
        use std::os::unix::io::IntoRawFd;
        Self {
            exported_memory_fd: description.exported_memory_fd.into_raw_fd(),
            allocation_byte_size: description.allocation_byte_size,
            width: description.width,
            height: description.height,
            format_wire_name: description.format_wire_name,
            vk_image_creation_recipe: description.vk_image_creation_recipe,
            dedicated_allocation: description.dedicated_allocation,
            export_contract: description.export_contract,
        }
    }
}

#[pymethods]
impl PythonOpaqueFdTextureExport {
    /// The exported memory fd. The caller owns it: a successful foreign
    /// import adopts it — never close it after one; always close it after
    /// a failed one.
    #[getter]
    fn fd(&self) -> i32 {
        self.exported_memory_fd
    }

    /// Byte size of the whole `VkDeviceMemory` at offset zero — what the
    /// foreign import states, never a tight width x height x bpp figure.
    #[getter]
    fn allocation_byte_size(&self) -> u64 {
        self.allocation_byte_size
    }

    /// Texture width in pixels.
    #[getter]
    fn width(&self) -> u32 {
        self.width
    }

    /// Texture height in pixels.
    #[getter]
    fn height(&self) -> u32 {
        self.height
    }

    /// The engine's format name for the texture, e.g. `"rgba16_float"`.
    #[getter]
    fn format(&self) -> &'static str {
        self.format_wire_name
    }

    /// Raw `VkImageTiling` the exporter created the image with.
    #[getter]
    fn vk_image_tiling(&self) -> i32 {
        self.vk_image_creation_recipe.vk_image_tiling
    }

    /// Raw `VkImageUsageFlags` bitfield the exporter created the image with.
    #[getter]
    fn vk_image_usage_flags(&self) -> u32 {
        self.vk_image_creation_recipe.vk_image_usage_flags
    }

    /// `VkImageCreateInfo::mipLevels` of the exporter's image.
    #[getter]
    fn vk_image_mip_levels(&self) -> u32 {
        self.vk_image_creation_recipe.vk_image_mip_levels
    }

    /// `VkImageCreateInfo::arrayLayers` of the exporter's image.
    #[getter]
    fn vk_image_array_layers(&self) -> u32 {
        self.vk_image_creation_recipe.vk_image_array_layers
    }

    /// Raw `VkSampleCountFlagBits` of the exporter's image.
    #[getter]
    fn vk_image_samples(&self) -> i32 {
        self.vk_image_creation_recipe.vk_image_samples
    }

    /// Whether the allocation is dedicated — always true for this flavour;
    /// omitting the importer-side dedicated chain is undefined behaviour,
    /// not leniency.
    #[getter]
    fn dedicated_allocation(&self) -> bool {
        self.dedicated_allocation
    }

    /// The exporter's Vulkan memory type index, for the importer-side
    /// `vkAllocateMemory(VkImportMemoryFdInfoKHR)`.
    #[getter]
    fn vk_memory_type_index(&self) -> u32 {
        self.export_contract.vk_memory_type_index
    }

    /// The exporting device's `VkPhysicalDeviceIDProperties::deviceUUID`,
    /// 16 bytes — an OPAQUE_FD is device-bound.
    #[getter]
    fn exporting_device_uuid<'py>(&self, python: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(python, &self.export_contract.exporting_device_uuid)
    }
}
