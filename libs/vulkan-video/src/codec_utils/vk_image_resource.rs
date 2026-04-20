// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkCodecUtils/VkImageResource.h + VkImageResource.cpp
//!
//! Wraps a VkImage + VkDeviceMemory with optional VkImageView(s).
//! The C++ original uses VkVideoRefCountBase / VkSharedBaseObj for ref counting;
//! in Rust we use `Arc<VkImageResource>` and `Arc<VkImageResourceView>`.
//!
//! Key divergences from C++:
//! - VulkanDeviceMemoryImpl is represented as an optional (VkDeviceMemory, VkDeviceSize) pair
//!   since that type is not yet ported. When it is, this can be swapped in.
//! - YCbCr format info (VkMpFormatInfo / YcbcrVkFormatInfo) is represented via a local
//!   helper module since the nvidia_utils ycbcr tables are not yet ported.
//! - The `VulkanDeviceContext` wrapper pointer is replaced by `vulkanalia::Device` + `vulkanalia::Instance`
//!   handles, which is how ash exposes Vulkan calls.
//! - pNext chain building for exportable images uses ash's builder pattern where possible,
//!   but raw struct construction where chaining is complex.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc};
#[cfg(unix)]
use vulkanalia::vk::KhrExternalMemoryFdExtensionDeviceCommands;

// ---------------------------------------------------------------------------
// YCbCr format helpers (subset of nvidia_utils/vulkan/ycbcrvkinfo.h)
// ---------------------------------------------------------------------------

/// Planes memory layout — mirrors C++ `YCBCR_PLANES_LAYOUT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum YcbcrPlanesLayout {
    SinglePlaneUnnormalized = 0,
    SinglePlaneInterleaved = 1,
    SemiPlanarCbCrInterleaved = 2,
    PlanarCbCrStrideInterleaved = 3,
    PlanarStridePadded = 4,
    PlanarCbCrBlockJoined = 5,
}

/// Bits per channel — mirrors C++ `Ycbcr_BPP`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum YcbcrBpp {
    Bpp8 = 0,
    Bpp10 = 1,
    Bpp12 = 2,
    Bpp14 = 3,
    Bpp16 = 4,
}

/// Planes layout info — mirrors C++ `YcbcrPlanesLayoutInfo` (bit-field struct).
#[derive(Debug, Clone, Copy)]
pub struct YcbcrPlanesLayoutInfo {
    pub layout: YcbcrPlanesLayout,
    pub disjoint: bool,
    pub bpp: YcbcrBpp,
    pub secondary_plane_subsampled_x: bool,
    pub secondary_plane_subsampled_y: bool,
    pub number_of_extra_planes: u32,
}

/// Multi-planar format info — mirrors C++ `VkMpFormatInfo`.
#[derive(Debug, Clone, Copy)]
pub struct VkMpFormatInfo {
    pub vk_format: vk::Format,
    pub planes_layout: YcbcrPlanesLayoutInfo,
    /// Per-plane VkFormat values (up to 4 planes).
    pub vk_plane_format: [vk::Format; 4],
}

/// Lookup YCbCr multi-planar format info for a given VkFormat.
/// Returns `None` for non-YCbCr formats.
///
/// Mirrors C++ `YcbcrVkFormatInfo()`.
pub fn ycbcr_vk_format_info(format: vk::Format) -> Option<VkMpFormatInfo> {
    // Table of common YCbCr formats used in video decode/encode.
    // This is a subset — extend as more formats are needed.
    match format {
        // NV12: 8-bit 4:2:0 semi-planar (Y + interleaved CbCr)
        vk::Format::G8_B8R8_2PLANE_420_UNORM => Some(VkMpFormatInfo {
            vk_format: format,
            planes_layout: YcbcrPlanesLayoutInfo {
                layout: YcbcrPlanesLayout::SemiPlanarCbCrInterleaved,
                disjoint: false,
                bpp: YcbcrBpp::Bpp8,
                secondary_plane_subsampled_x: true,
                secondary_plane_subsampled_y: true,
                number_of_extra_planes: 1,
            },
            vk_plane_format: [
                vk::Format::R8_UNORM,
                vk::Format::R8G8_UNORM,
                vk::Format::UNDEFINED,
                vk::Format::UNDEFINED,
            ],
        }),
        // P010: 10-bit 4:2:0 semi-planar
        vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16 => Some(VkMpFormatInfo {
            vk_format: format,
            planes_layout: YcbcrPlanesLayoutInfo {
                layout: YcbcrPlanesLayout::SemiPlanarCbCrInterleaved,
                disjoint: false,
                bpp: YcbcrBpp::Bpp10,
                secondary_plane_subsampled_x: true,
                secondary_plane_subsampled_y: true,
                number_of_extra_planes: 1,
            },
            vk_plane_format: [
                vk::Format::R10X6_UNORM_PACK16,
                vk::Format::R10X6G10X6_UNORM_2PACK16,
                vk::Format::UNDEFINED,
                vk::Format::UNDEFINED,
            ],
        }),
        // P016 / P012: 16-bit 4:2:0 semi-planar
        vk::Format::G16_B16R16_2PLANE_420_UNORM => Some(VkMpFormatInfo {
            vk_format: format,
            planes_layout: YcbcrPlanesLayoutInfo {
                layout: YcbcrPlanesLayout::SemiPlanarCbCrInterleaved,
                disjoint: false,
                bpp: YcbcrBpp::Bpp16,
                secondary_plane_subsampled_x: true,
                secondary_plane_subsampled_y: true,
                number_of_extra_planes: 1,
            },
            vk_plane_format: [
                vk::Format::R16_UNORM,
                vk::Format::R16G16_UNORM,
                vk::Format::UNDEFINED,
                vk::Format::UNDEFINED,
            ],
        }),
        // 8-bit 4:2:0 3-plane (I420 / YV12)
        vk::Format::G8_B8_R8_3PLANE_420_UNORM => Some(VkMpFormatInfo {
            vk_format: format,
            planes_layout: YcbcrPlanesLayoutInfo {
                layout: YcbcrPlanesLayout::PlanarCbCrBlockJoined,
                disjoint: false,
                bpp: YcbcrBpp::Bpp8,
                secondary_plane_subsampled_x: true,
                secondary_plane_subsampled_y: true,
                number_of_extra_planes: 2,
            },
            vk_plane_format: [
                vk::Format::R8_UNORM,
                vk::Format::R8_UNORM,
                vk::Format::R8_UNORM,
                vk::Format::UNDEFINED,
            ],
        }),
        // 8-bit 4:2:2 semi-planar
        vk::Format::G8_B8R8_2PLANE_422_UNORM => Some(VkMpFormatInfo {
            vk_format: format,
            planes_layout: YcbcrPlanesLayoutInfo {
                layout: YcbcrPlanesLayout::SemiPlanarCbCrInterleaved,
                disjoint: false,
                bpp: YcbcrBpp::Bpp8,
                secondary_plane_subsampled_x: true,
                secondary_plane_subsampled_y: false,
                number_of_extra_planes: 1,
            },
            vk_plane_format: [
                vk::Format::R8_UNORM,
                vk::Format::R8G8_UNORM,
                vk::Format::UNDEFINED,
                vk::Format::UNDEFINED,
            ],
        }),
        // 8-bit 4:4:4 semi-planar
        vk::Format::G8_B8R8_2PLANE_444_UNORM => Some(VkMpFormatInfo {
            vk_format: format,
            planes_layout: YcbcrPlanesLayoutInfo {
                layout: YcbcrPlanesLayout::SemiPlanarCbCrInterleaved,
                disjoint: false,
                bpp: YcbcrBpp::Bpp8,
                secondary_plane_subsampled_x: false,
                secondary_plane_subsampled_y: false,
                number_of_extra_planes: 1,
            },
            vk_plane_format: [
                vk::Format::R8_UNORM,
                vk::Format::R8G8_UNORM,
                vk::Format::UNDEFINED,
                vk::Format::UNDEFINED,
            ],
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// VkImageResource
// ---------------------------------------------------------------------------

/// Information about device memory backing an image resource.
///
/// Stands in for C++ `VulkanDeviceMemoryImpl`. When that type is ported,
/// this struct can be replaced or wrapped.
#[derive(Debug)]
pub struct DeviceMemoryInfo {
    pub memory: vk::DeviceMemory,
    pub size: vk::DeviceSize,
    pub memory_property_flags: vk::MemoryPropertyFlags,
    pub memory_type_index: u32,
    pub export_handle_types: vk::ExternalMemoryHandleTypeFlags,
}

/// Wraps a `VkImage` + backing device memory + creation metadata.
///
/// Mirrors C++ `VkImageResource`. Use `Arc<VkImageResource>` where the C++
/// uses `VkSharedBaseObj<VkImageResource>`.
pub struct VkImageResource {
    device: vulkanalia::Device,
    image: vk::Image,
    image_create_info: vk::ImageCreateInfo,
    image_offset: vk::DeviceSize,
    image_size: vk::DeviceSize,
    device_memory: Option<DeviceMemoryInfo>,

    /// VMA allocator handle — present when this resource was created via `create()`.
    /// `None` for external/import paths that don't use VMA.
    allocator: Option<Arc<vma::Allocator>>,
    /// VMA allocation handle — present when this resource was allocated via VMA.
    /// `None` for external/import paths.
    allocation: Option<vma::Allocation>,

    /// Per-plane subresource layout for LINEAR images (up to 3 color planes).
    layouts: [vk::SubresourceLayout; 3],
    /// Per-memory-plane layout for DRM format modifier images (up to 4 memory planes).
    memory_plane_layouts: [vk::SubresourceLayout; 4],

    drm_format_modifier: u64,
    memory_plane_count: u32,

    is_linear_image: bool,
    is_16bit: bool,
    is_subsampled_x: bool,
    is_subsampled_y: bool,
    uses_drm_format_modifier: bool,
    /// When false (CreateFromExternal), `drop` will NOT destroy the VkImage or free memory.
    owns_resources: bool,
}

impl std::fmt::Debug for VkImageResource {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("VkImageResource")
            .field("image", &self.image)
            .field("image_size", &self.image_size)
            .field("is_linear_image", &self.is_linear_image)
            .field("owns_resources", &self.owns_resources)
            .finish()
    }
}

/// Parameters for `VkImageResource::create`.
pub struct ImageResourceCreateInfo {
    pub image_create_info: vk::ImageCreateInfo,
    pub memory_property_flags: vk::MemoryPropertyFlags,
}

/// Parameters for `VkImageResource::create_exportable`.
pub struct ImageResourceExportCreateInfo {
    pub image_create_info: vk::ImageCreateInfo,
    pub memory_property_flags: vk::MemoryPropertyFlags,
    pub export_handle_types: vk::ExternalMemoryHandleTypeFlags,
    pub drm_format_modifier: u64,
}

impl VkImageResource {
    // -- Accessors (mirrors C++ inline getters) --

    pub fn get_image(&self) -> vk::Image {
        self.image
    }

    pub fn get_device_memory(&self) -> vk::DeviceMemory {
        self.device_memory
            .as_ref()
            .map_or(vk::DeviceMemory::null(), |m| m.memory)
    }

    pub fn get_image_device_memory_size(&self) -> vk::DeviceSize {
        self.image_size
    }

    pub fn get_image_device_memory_offset(&self) -> vk::DeviceSize {
        self.image_offset
    }

    pub fn get_image_create_info(&self) -> &vk::ImageCreateInfo {
        &self.image_create_info
    }

    pub fn get_image_tiling(&self) -> vk::ImageTiling {
        self.image_create_info.tiling
    }

    pub fn is_linear_image(&self) -> bool {
        self.is_linear_image
    }

    pub fn is_16bit(&self) -> bool {
        self.is_16bit
    }

    pub fn is_subsampled_x(&self) -> bool {
        self.is_subsampled_x
    }

    pub fn is_subsampled_y(&self) -> bool {
        self.is_subsampled_y
    }

    pub fn get_subresource_layout(&self) -> Option<&[vk::SubresourceLayout; 3]> {
        if self.is_linear_image {
            Some(&self.layouts)
        } else {
            None
        }
    }

    /// Get plane layout for LINEAR images.
    ///
    /// For DRM modifier images, use `get_memory_plane_layout` instead.
    pub fn get_plane_layout(&self, plane_index: u32) -> Option<vk::SubresourceLayout> {
        if !self.is_linear_image || plane_index > 2 {
            return None;
        }
        let layout = self.layouts[plane_index as usize];
        if layout.size > 0 || layout.row_pitch > 0 {
            Some(layout)
        } else {
            None
        }
    }

    pub fn is_exportable(&self) -> bool {
        self.device_memory
            .as_ref()
            .map_or(false, |m| !m.export_handle_types.is_empty())
    }

    pub fn get_drm_format_modifier(&self) -> u64 {
        self.drm_format_modifier
    }

    pub fn uses_drm_format_modifier(&self) -> bool {
        self.uses_drm_format_modifier
    }

    pub fn get_memory_plane_count(&self) -> u32 {
        self.memory_plane_count
    }

    /// Get plane layout using MEMORY_PLANE aspect bits (for DRM format modifier images).
    pub fn get_memory_plane_layout(&self, plane_index: u32) -> Option<vk::SubresourceLayout> {
        if !self.uses_drm_format_modifier || plane_index >= self.memory_plane_count || plane_index >= 4 {
            return None;
        }
        Some(self.memory_plane_layouts[plane_index as usize])
    }

    pub fn get_memory_type_index(&self) -> u32 {
        self.device_memory
            .as_ref()
            .map_or(0, |m| m.memory_type_index)
    }

    /// Check compatibility with an image create info (same logic as C++ `IsCompatible`).
    pub fn is_compatible(&self, image_create_info: &vk::ImageCreateInfo) -> bool {
        if image_create_info.extent.width > self.image_create_info.extent.width {
            return false;
        }
        if image_create_info.extent.height > self.image_create_info.extent.height {
            return false;
        }
        if image_create_info.array_layers > self.image_create_info.array_layers {
            return false;
        }
        if image_create_info.tiling != self.image_create_info.tiling {
            return false;
        }
        if image_create_info.image_type != self.image_create_info.image_type {
            return false;
        }
        if image_create_info.format != self.image_create_info.format {
            return false;
        }
        true
    }

    #[cfg(unix)]
    pub fn export_native_handle(
        &self,
        handle_type: vk::ExternalMemoryHandleTypeFlags,
    ) -> Result<i32, vk::Result> {
        let mem_info = self
            .device_memory
            .as_ref()
            .ok_or(vk::Result::ERROR_INITIALIZATION_FAILED)?;
        if mem_info.export_handle_types.is_empty() {
            return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
        }

        let get_fd_info = vk::MemoryGetFdInfoKHR::builder()
            .memory(mem_info.memory)
            .handle_type(handle_type.into());

        // Safety: the caller guarantees the device and memory are valid.
        let fd = unsafe {
            self.device.get_memory_fd_khr(&get_fd_info).map_err(vk::Result::from)?
        };
        Ok(fd)
    }

    // -- Creation functions --

    /// Create an image resource with device-local memory via VMA.
    ///
    /// Mirrors C++ `VkImageResource::Create`. Uses VMA to handle memory type
    /// selection, allocation, and binding in a single call.
    pub fn create(
        device: &vulkanalia::Device,
        allocator: &Arc<vma::Allocator>,
        image_create_info: &vk::ImageCreateInfo,
        memory_property_flags: vk::MemoryPropertyFlags,
    ) -> Result<Arc<Self>, vk::Result> {
        // Safety: caller ensures device/allocator are valid and image_create_info is well-formed.
        unsafe {
            let alloc_options = vma::AllocationOptions {
                required_flags: memory_property_flags,
                ..Default::default()
            };

            let (image, allocation) = allocator
                .create_image(*image_create_info, &alloc_options)?;

            let alloc_info = allocator.get_allocation_info(allocation);
            let image_offset = alloc_info.offset;
            let image_size = alloc_info.size;

            // Retrieve actual memory property flags from the chosen memory type.
            let mem_props = allocator.get_memory_properties();
            let memory_type_index = alloc_info.memoryType as u32;
            let actual_flags =
                mem_props.memory_types[memory_type_index as usize].property_flags;

            let device_memory_info = DeviceMemoryInfo {
                memory: alloc_info.deviceMemory,
                size: image_size,
                memory_property_flags: actual_flags,
                memory_type_index,
                export_handle_types: vk::ExternalMemoryHandleTypeFlags::empty(),
            };

            let mut resource = Self::new_inner(
                device.clone(),
                image_create_info,
                image,
                image_offset,
                image_size,
                Some(device_memory_info),
                Some(allocator.clone()),
                Some(allocation),
                0,
                0,
            );
            resource.query_subresource_layouts();
            Ok(Arc::new(resource))
        }
    }

    /// Create an image resource from an externally-owned VkImage (non-owning wrapper).
    ///
    /// The caller retains ownership of `image` and `memory`. When this
    /// `VkImageResource` is dropped it will NOT destroy the image or free the memory.
    ///
    /// Mirrors C++ `VkImageResource::CreateFromExternal`.
    pub fn create_from_external(
        device: &vulkanalia::Device,
        image: vk::Image,
        _memory: vk::DeviceMemory,
        image_create_info: &vk::ImageCreateInfo,
    ) -> Result<Arc<Self>, vk::Result> {
        let mut resource = Self::new_inner(
            device.clone(),
            image_create_info,
            image,
            0,
            0,
            None, // non-owning: no DeviceMemoryInfo
            None, // no VMA allocator
            None, // no VMA allocation
            0,
            0,
        );
        resource.owns_resources = false;
        // Still query mp_info for format flags, but skip layout queries (no memory object).
        resource.query_format_flags(image_create_info);
        Ok(Arc::new(resource))
    }

    /// Create an owning image resource from imported raw handles.
    ///
    /// Unlike `create_from_external` (non-owning), this takes ownership of both
    /// the VkImage and VkDeviceMemory. On drop, destroys the image and frees memory.
    ///
    /// Mirrors C++ `VkImageResource::CreateFromImport`.
    pub fn create_from_import(
        device: &vulkanalia::Device,
        image: vk::Image,
        memory: vk::DeviceMemory,
        memory_size: vk::DeviceSize,
        image_create_info: &vk::ImageCreateInfo,
    ) -> Result<Arc<Self>, vk::Result> {
        let device_memory_info = if memory != vk::DeviceMemory::null() {
            Some(DeviceMemoryInfo {
                memory,
                size: memory_size,
                memory_property_flags: vk::MemoryPropertyFlags::empty(),
                memory_type_index: 0,
                export_handle_types: vk::ExternalMemoryHandleTypeFlags::empty(),
            })
        } else {
            None
        };

        let mut resource = Self::new_inner(
            device.clone(),
            image_create_info,
            image,
            0,
            memory_size,
            device_memory_info,
            None, // no VMA allocator for imported resources
            None, // no VMA allocation for imported resources
            0,
            0,
        );
        resource.owns_resources = true;
        resource.query_subresource_layouts();
        Ok(Arc::new(resource))
    }

    // -- Internal helpers --

    /// Core constructor — mirrors the C++ private constructor.
    fn new_inner(
        device: vulkanalia::Device,
        image_create_info: &vk::ImageCreateInfo,
        image: vk::Image,
        image_offset: vk::DeviceSize,
        image_size: vk::DeviceSize,
        device_memory: Option<DeviceMemoryInfo>,
        allocator: Option<Arc<vma::Allocator>>,
        allocation: Option<vma::Allocation>,
        drm_format_modifier: u64,
        memory_plane_count: u32,
    ) -> Self {
        let uses_drm = drm_format_modifier != 0
            || image_create_info.tiling == vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT;

        // Build an 'static copy of ImageCreateInfo (pNext is zeroed — we do not
        // carry over extension chains; they are only needed at creation time).
        let stored_info = vk::ImageCreateInfo {
            s_type: vk::StructureType::IMAGE_CREATE_INFO,
            next: std::ptr::null(),
            flags: image_create_info.flags,
            image_type: image_create_info.image_type,
            format: image_create_info.format,
            extent: image_create_info.extent,
            mip_levels: image_create_info.mip_levels,
            array_layers: image_create_info.array_layers,
            samples: image_create_info.samples,
            tiling: image_create_info.tiling,
            usage: image_create_info.usage,
            sharing_mode: image_create_info.sharing_mode,
            queue_family_index_count: 0,
            queue_family_indices: std::ptr::null(),
            initial_layout: image_create_info.initial_layout,
        };

        Self {
            device,
            image,
            image_create_info: stored_info,
            image_offset,
            image_size,
            device_memory,
            allocator,
            allocation,
            layouts: [vk::SubresourceLayout::default(); 3],
            memory_plane_layouts: [vk::SubresourceLayout::default(); 4],
            drm_format_modifier,
            memory_plane_count,
            is_linear_image: false,
            is_16bit: false,
            is_subsampled_x: false,
            is_subsampled_y: false,
            uses_drm_format_modifier: uses_drm,
            owns_resources: true,
        }
    }

    /// Query subresource layouts for linear / multi-planar images.
    /// Mirrors the body of the C++ constructor.
    fn query_subresource_layouts(&mut self) {
        let format = self.image_create_info.format;

        // Query memory plane layouts for DRM format modifier images
        if self.uses_drm_format_modifier && self.memory_plane_count > 0 {
            let memory_plane_aspects = [
                vk::ImageAspectFlags::MEMORY_PLANE_0_EXT,
                vk::ImageAspectFlags::MEMORY_PLANE_1_EXT,
                vk::ImageAspectFlags::MEMORY_PLANE_2_EXT,
                vk::ImageAspectFlags::MEMORY_PLANE_3_EXT,
            ];
            for p in 0..std::cmp::min(self.memory_plane_count as usize, 4) {
                let sub_res = vk::ImageSubresource {
                    aspect_mask: memory_plane_aspects[p],
                    mip_level: 0,
                    array_layer: 0,
                };
                unsafe {
                    self.memory_plane_layouts[p] =
                        self.device
                            .get_image_subresource_layout(self.image, &sub_res);
                }
            }

            // NVIDIA WORKAROUND: If MEMORY_PLANE_1+ returned zeros, calculate offsets
            if let Some(mp_info) = ycbcr_vk_format_info(format) {
                if self.memory_plane_count >= 2
                    && self.memory_plane_layouts[1].size == 0
                    && self.memory_plane_layouts[1].row_pitch == 0
                {
                    let width = self.image_create_info.extent.width;
                    let height = self.image_create_info.extent.height;
                    let bytes_per_pixel: u64 =
                        if mp_info.planes_layout.bpp == YcbcrBpp::Bpp8 { 1 } else { 2 };

                    // Plane 0 (Y): Full resolution
                    if self.memory_plane_layouts[0].row_pitch == 0 {
                        self.memory_plane_layouts[0].row_pitch =
                            width as u64 * bytes_per_pixel;
                    }
                    if self.memory_plane_layouts[0].size == 0 {
                        self.memory_plane_layouts[0].size =
                            self.memory_plane_layouts[0].row_pitch * height as u64;
                    }
                    self.memory_plane_layouts[0].offset = 0;

                    // Plane 1 (UV/CbCr): Subsampled based on format
                    let chroma_width = if mp_info.planes_layout.secondary_plane_subsampled_x {
                        (width + 1) / 2
                    } else {
                        width
                    };
                    let chroma_height = if mp_info.planes_layout.secondary_plane_subsampled_y {
                        (height + 1) / 2
                    } else {
                        height
                    };

                    // For semi-planar (NV12, P010, etc.), UV plane has 2 components interleaved
                    let uv_bytes_per_pixel = bytes_per_pixel * 2;

                    self.memory_plane_layouts[1].offset = self.memory_plane_layouts[0].size;
                    self.memory_plane_layouts[1].row_pitch =
                        chroma_width as u64 * uv_bytes_per_pixel;
                    self.memory_plane_layouts[1].size =
                        self.memory_plane_layouts[1].row_pitch * chroma_height as u64;
                    self.memory_plane_layouts[1].array_pitch = 0;
                    self.memory_plane_layouts[1].depth_pitch = 0;

                    // For 3-plane formats (I420, etc.)
                    if self.memory_plane_count >= 3
                        && self.memory_plane_layouts[2].size == 0
                        && self.memory_plane_layouts[2].row_pitch == 0
                    {
                        // Cb and Cr are separate planes, each single component
                        self.memory_plane_layouts[1].row_pitch =
                            chroma_width as u64 * bytes_per_pixel;
                        self.memory_plane_layouts[1].size =
                            self.memory_plane_layouts[1].row_pitch * chroma_height as u64;

                        self.memory_plane_layouts[2].offset =
                            self.memory_plane_layouts[1].offset + self.memory_plane_layouts[1].size;
                        self.memory_plane_layouts[2].row_pitch =
                            chroma_width as u64 * bytes_per_pixel;
                        self.memory_plane_layouts[2].size =
                            self.memory_plane_layouts[2].row_pitch * chroma_height as u64;
                        self.memory_plane_layouts[2].array_pitch = 0;
                        self.memory_plane_layouts[2].depth_pitch = 0;
                    }
                }
            }
        }

        let mp_info = ycbcr_vk_format_info(format);

        if mp_info.is_none() {
            // Not a multi-planar format
            self.is_linear_image =
                self.image_create_info.tiling == vk::ImageTiling::LINEAR;
            if self.is_linear_image {
                let sub_resource = vk::ImageSubresource {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    array_layer: 0,
                };
                unsafe {
                    self.layouts[0] =
                        self.device
                            .get_image_subresource_layout(self.image, &sub_resource);
                }
            }
            return;
        }

        let mp_info = mp_info.unwrap();
        self.query_format_flags_from_mp_info(&mp_info);

        // External/non-owning wrapper has no device memory — skip host-visible queries
        let is_host_visible = self
            .device_memory
            .as_ref()
            .map_or(false, |m| {
                m.memory_property_flags
                    .contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
            });

        if !is_host_visible {
            return;
        }

        self.is_linear_image = true;
        let is_unnormalized_rgba = mp_info.planes_layout.layout
            == YcbcrPlanesLayout::SinglePlaneUnnormalized
            && !mp_info.planes_layout.disjoint;

        if !is_unnormalized_rgba {
            match mp_info.planes_layout.layout {
                YcbcrPlanesLayout::SinglePlaneUnnormalized
                | YcbcrPlanesLayout::SinglePlaneInterleaved => {
                    let sub = vk::ImageSubresource {
                        aspect_mask: vk::ImageAspectFlags::PLANE_0,
                        mip_level: 0,
                        array_layer: 0,
                    };
                    unsafe {
                        self.layouts[0] =
                            self.device.get_image_subresource_layout(self.image, &sub);
                    }
                }
                YcbcrPlanesLayout::SemiPlanarCbCrInterleaved => {
                    let sub0 = vk::ImageSubresource {
                        aspect_mask: vk::ImageAspectFlags::PLANE_0,
                        mip_level: 0,
                        array_layer: 0,
                    };
                    let sub1 = vk::ImageSubresource {
                        aspect_mask: vk::ImageAspectFlags::PLANE_1,
                        mip_level: 0,
                        array_layer: 0,
                    };
                    unsafe {
                        self.layouts[0] =
                            self.device.get_image_subresource_layout(self.image, &sub0);
                        self.layouts[1] =
                            self.device.get_image_subresource_layout(self.image, &sub1);
                    }
                }
                YcbcrPlanesLayout::PlanarCbCrStrideInterleaved
                | YcbcrPlanesLayout::PlanarCbCrBlockJoined
                | YcbcrPlanesLayout::PlanarStridePadded => {
                    let aspects = [
                        vk::ImageAspectFlags::PLANE_0,
                        vk::ImageAspectFlags::PLANE_1,
                        vk::ImageAspectFlags::PLANE_2,
                    ];
                    for (i, &aspect) in aspects.iter().enumerate() {
                        let sub = vk::ImageSubresource {
                            aspect_mask: aspect,
                            mip_level: 0,
                            array_layer: 0,
                        };
                        unsafe {
                            self.layouts[i] =
                                self.device.get_image_subresource_layout(self.image, &sub);
                        }
                    }
                }
            }
        } else {
            // Unnormalized RGBA — single subresource
            let sub = vk::ImageSubresource {
                aspect_mask: vk::ImageAspectFlags::empty(),
                mip_level: 0,
                array_layer: 0,
            };
            unsafe {
                self.layouts[0] =
                    self.device.get_image_subresource_layout(self.image, &sub);
            }
        }
    }

    /// Set format flags from image_create_info without querying layouts.
    /// Used for non-owning external wrappers.
    fn query_format_flags(&mut self, image_create_info: &vk::ImageCreateInfo) {
        if let Some(mp_info) = ycbcr_vk_format_info(image_create_info.format) {
            self.query_format_flags_from_mp_info(&mp_info);
        }
    }

    fn query_format_flags_from_mp_info(&mut self, mp_info: &VkMpFormatInfo) {
        self.is_subsampled_x = mp_info.planes_layout.secondary_plane_subsampled_x;
        self.is_subsampled_y = mp_info.planes_layout.secondary_plane_subsampled_y;
        self.is_16bit = mp_info.planes_layout.bpp != YcbcrBpp::Bpp8;
    }

    /// Internal destroy — called from `Drop`.
    fn destroy(&mut self) {
        if self.owns_resources {
            if let (Some(allocator), Some(allocation)) =
                (self.allocator.as_ref(), self.allocation.take())
            {
                // VMA path: destroy image + free memory in one call.
                if self.image != vk::Image::null() {
                    unsafe { allocator.destroy_image(self.image, allocation) };
                }
            } else {
                // Non-VMA path (import): separate destroy + free.
                unsafe {
                    if self.image != vk::Image::null() {
                        self.device.destroy_image(self.image, None);
                    }
                    if let Some(ref mem) = self.device_memory {
                        if mem.memory != vk::DeviceMemory::null() {
                            self.device.free_memory(mem.memory, None);
                        }
                    }
                }
            }
        }
        self.image = vk::Image::null();
        self.device_memory = None;
        self.allocator = None;
        self.allocation = None;
    }
}

impl Drop for VkImageResource {
    fn drop(&mut self) {
        self.destroy();
    }
}

#[cfg(test)]
impl VkImageResource {
    /// Create a test-only VkImageResource without a real Vulkan device.
    ///
    /// SAFETY: The returned resource has `owns_resources = false` and must never
    /// have device methods called on it. This is only for testing pure logic
    /// (is_compatible, get_plane_layout, etc.).
    /// Create a test-only VkImageResource that is wrapped in ManuallyDrop
    /// to prevent the uninitialized Device from being dropped.
    fn new_test_stub(image_create_info: vk::ImageCreateInfo) -> std::mem::ManuallyDrop<Self> {
        // SAFETY: dummy_device is never used for Vulkan calls; owns_resources is false.
        // ManuallyDrop prevents the uninitialized Device from being dropped.
        let device: vulkanalia::Device = unsafe { std::mem::MaybeUninit::uninit().assume_init() };
        std::mem::ManuallyDrop::new(Self {
            device,
            image: vk::Image::null(),
            image_create_info,
            image_offset: 0,
            image_size: 0,
            device_memory: None,
            allocator: None,
            allocation: None,
            layouts: [vk::SubresourceLayout::default(); 3],
            memory_plane_layouts: [vk::SubresourceLayout::default(); 4],
            drm_format_modifier: 0,
            memory_plane_count: 0,
            is_linear_image: false,
            is_16bit: false,
            is_subsampled_x: false,
            is_subsampled_y: false,
            uses_drm_format_modifier: false,
            owns_resources: false,
        })
    }
}

// ---------------------------------------------------------------------------
// VkImageResourceView
// ---------------------------------------------------------------------------

/// Wraps one or more `VkImageView` handles over a `VkImageResource`.
///
/// Mirrors C++ `VkImageResourceView`. Use `Arc<VkImageResourceView>` where the
/// C++ uses `VkSharedBaseObj<VkImageResourceView>`.
///
/// Layout of `image_views` array:
/// - `[0]` = combined view (may be null if skipped for storage-only)
/// - `[1..num_views]` = per-plane views
pub struct VkImageResourceView {
    device: vulkanalia::Device,
    image_resource: Arc<VkImageResource>,
    image_views: [vk::ImageView; 4],
    image_subresource_range: vk::ImageSubresourceRange,
    num_views: u32,
    num_planes: u32,
}

impl std::fmt::Debug for VkImageResourceView {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("VkImageResourceView")
            .field("num_views", &self.num_views)
            .field("num_planes", &self.num_planes)
            .finish()
    }
}

impl VkImageResourceView {
    // -- Accessors --

    /// Get the "primary" image view — combined view, or first plane view if combined is null.
    pub fn get_image_view(&self) -> vk::ImageView {
        if self.image_views[0] != vk::ImageView::null() {
            self.image_views[0]
        } else if self.num_planes > 0 {
            self.image_views[1]
        } else {
            vk::ImageView::null()
        }
    }

    pub fn get_number_of_planes(&self) -> u32 {
        self.num_planes
    }

    /// Get the image view for a specific plane.
    pub fn get_plane_image_view(&self, plane_index: u32) -> vk::ImageView {
        if self.num_planes == 1 {
            return self.image_views[0];
        }
        debug_assert!(plane_index < self.num_planes);
        self.image_views[(plane_index + 1) as usize]
    }

    pub fn get_image_subresource_range(&self) -> &vk::ImageSubresourceRange {
        &self.image_subresource_range
    }

    pub fn get_image_resource(&self) -> &Arc<VkImageResource> {
        &self.image_resource
    }

    // -- Creation --

    /// Create image view(s) for an image resource.
    ///
    /// Mirrors the C++ `VkImageResourceView::Create` (2-arg overload without planeUsageOverride).
    pub fn create(
        device: &vulkanalia::Device,
        image_resource: &Arc<VkImageResource>,
        image_subresource_range: vk::ImageSubresourceRange,
    ) -> Result<Arc<Self>, vk::Result> {
        Self::create_with_plane_usage(
            device,
            image_resource,
            image_subresource_range,
            vk::ImageUsageFlags::empty(),
        )
    }

    /// Create image view(s) with optional plane usage override.
    ///
    /// When `plane_usage_override` is non-empty, per-plane views will be created
    /// with a `VkImageViewUsageCreateInfo` specifying that usage. This is needed
    /// when the base format does not support storage but individual planes (R8, RG8) do
    /// via `VK_IMAGE_CREATE_EXTENDED_USAGE_BIT`.
    ///
    /// Mirrors C++ `VkImageResourceView::Create` with `planeUsageOverride`.
    pub fn create_with_plane_usage(
        device: &vulkanalia::Device,
        image_resource: &Arc<VkImageResource>,
        image_subresource_range: vk::ImageSubresourceRange,
        plane_usage_override: vk::ImageUsageFlags,
    ) -> Result<Arc<Self>, vk::Result> {
        let mut image_views = [vk::ImageView::null(); 4];
        let mut num_views: u32 = 0;
        let mut num_planes: u32 = 0;

        let format = image_resource.image_create_info.format;
        let mp_info = ycbcr_vk_format_info(format);
        let image_usage = image_resource.image_create_info.usage;

        let view_type = if image_subresource_range.layer_count > 1 {
            vk::ImageViewType::_2D_ARRAY
        } else {
            vk::ImageViewType::_2D
        };

        let mut view_info = vk::ImageViewCreateInfo::builder()
            .image(image_resource.get_image())
            .view_type(view_type)
            .format(format)
            .components(vk::ComponentMapping {
                r: vk::ComponentSwizzle::IDENTITY,
                g: vk::ComponentSwizzle::IDENTITY,
                b: vk::ComponentSwizzle::IDENTITY,
                a: vk::ComponentSwizzle::IDENTITY,
            })
            .subresource_range(image_subresource_range);

        // Determine whether to skip the combined view
        let mut skip_combined_view =
            mp_info.is_some() && !plane_usage_override.is_empty();

        let mut usage_create_info = vk::ImageViewUsageCreateInfo::builder()
            .usage(plane_usage_override);

        if !skip_combined_view {
            if mp_info.is_some() {
                let mut combined_usage = image_usage;
                combined_usage &= !(vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::SAMPLED);
                if combined_usage.is_empty() {
                    skip_combined_view = true;
                } else {
                    usage_create_info = usage_create_info.usage(combined_usage);
                    view_info = view_info.push_next(&mut usage_create_info);
                }
            }
        }

        if !skip_combined_view {
            unsafe {
                image_views[num_views as usize] =
                    device.create_image_view(&view_info, None)?;
            }
            num_views += 1;
            // Reset pNext
            view_info.next = std::ptr::null();
        } else {
            // Placeholder for combined view
            image_views[num_views as usize] = vk::ImageView::null();
            num_views += 1;
        }

        // Per-plane views use a separate plane_usage_info below.

        if let Some(mp) = mp_info {
            // Multi-planar format: create per-plane views

            let plane_usage = if !plane_usage_override.is_empty() {
                plane_usage_override
            } else {
                let mut pu = image_usage;
                pu &= !(vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR
                    | vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR
                    | vk::ImageUsageFlags::VIDEO_DECODE_SRC_KHR
                    | vk::ImageUsageFlags::VIDEO_ENCODE_DST_KHR
                    | vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR
                    | vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR);
                pu
            };
            let mut plane_usage_info = vk::ImageViewUsageCreateInfo::builder()
                .usage(plane_usage);

            // Skip per-plane views when usage is zero (video-only images like DPB)
            if !plane_usage_info.usage.is_empty() {
                view_info = view_info.push_next(&mut plane_usage_info);

                // Create Y plane view
                view_info.format = mp.vk_plane_format[num_planes as usize];
                view_info.subresource_range.aspect_mask =
                    vk::ImageAspectFlags::from_bits_truncate(
                        vk::ImageAspectFlags::PLANE_0.bits() << num_planes,
                    );
                unsafe {
                    image_views[num_views as usize] =
                        device.create_image_view(&view_info, None)?;
                }
                num_views += 1;
                num_planes += 1;

                if mp.planes_layout.number_of_extra_planes > 0 {
                    view_info.format = mp.vk_plane_format[num_planes as usize];
                    view_info.subresource_range.aspect_mask =
                        vk::ImageAspectFlags::from_bits_truncate(
                            vk::ImageAspectFlags::PLANE_0.bits() << num_planes,
                        );
                    unsafe {
                        image_views[num_views as usize] =
                            device.create_image_view(&view_info, None)?;
                    }
                    num_views += 1;
                    num_planes += 1;

                    if mp.planes_layout.number_of_extra_planes > 1 {
                        view_info.format = mp.vk_plane_format[num_planes as usize];
                        view_info.subresource_range.aspect_mask =
                            vk::ImageAspectFlags::from_bits_truncate(
                                vk::ImageAspectFlags::PLANE_0.bits() << num_planes,
                            );
                        unsafe {
                            image_views[num_views as usize] =
                                device.create_image_view(&view_info, None)?;
                        }
                        num_views += 1;
                        num_planes += 1;
                    }
                }

                view_info.next = std::ptr::null();
            }
        } else {
            // Non multi-planar: check for YCbCr plane aspects
            let ycbcr_aspect_mask = image_subresource_range.aspect_mask
                & (vk::ImageAspectFlags::PLANE_0
                    | vk::ImageAspectFlags::PLANE_1
                    | vk::ImageAspectFlags::PLANE_2);

            if !ycbcr_aspect_mask.is_empty() {
                // Possible single plane — check if this is a known single-plane format
                let possible_single_plane = matches!(
                    format,
                    vk::Format::R8_UNORM
                        | vk::Format::R16_UNORM
                        | vk::Format::R10X6_UNORM_PACK16
                        | vk::Format::R12X4_UNORM_PACK16
                        | vk::Format::R8G8_UNORM
                        | vk::Format::R16G16_UNORM
                        | vk::Format::R32_UINT
                        | vk::Format::R8_SINT
                        | vk::Format::R8G8_SINT
                );

                if possible_single_plane {
                    for plane_num in 0..3u32 {
                        if ycbcr_aspect_mask.contains(vk::ImageAspectFlags::from_bits_truncate(
                            vk::ImageAspectFlags::PLANE_0.bits() << plane_num,
                        )) {
                            num_planes += 1;
                        }
                    }
                    // Is this a single plane? If more than 1, reset to 0.
                    if num_planes > 1 {
                        num_planes = 0;
                    }
                }
            }

            // For regular single-plane formats, set num_planes = 1
            if num_planes == 0 {
                num_planes = 1;
            }
        }

        Ok(Arc::new(Self {
            device: device.clone(),
            image_resource: image_resource.clone(),
            image_views,
            image_subresource_range,
            num_views,
            num_planes,
        }))
    }

    /// Create image view(s) with YCbCr sampler conversion support.
    ///
    /// Creates both:
    /// 1. A combined YCbCr view with `VkSamplerYcbcrConversionInfo` (for display sampling)
    /// 2. Per-plane views with optional usage override (for compute storage)
    ///
    /// Mirrors C++ `VkImageResourceView::Create` (YCbCr overload).
    pub fn create_with_ycbcr(
        device: &vulkanalia::Device,
        image_resource: &Arc<VkImageResource>,
        image_subresource_range: vk::ImageSubresourceRange,
        plane_usage_override: vk::ImageUsageFlags,
        ycbcr_conversion: vk::SamplerYcbcrConversion,
        combined_view_usage: vk::ImageUsageFlags,
    ) -> Result<Arc<Self>, vk::Result> {
        let mut image_views = [vk::ImageView::null(); 4];
        let mut num_views: u32 = 0;
        let mut num_planes: u32 = 0;

        let format = image_resource.image_create_info.format;
        let mp_info = ycbcr_vk_format_info(format);

        let mut view_info = vk::ImageViewCreateInfo::builder()
            .image(image_resource.get_image())
            .view_type(vk::ImageViewType::_2D) // Combined view is always 2D for display
            .format(format)
            .components(vk::ComponentMapping {
                r: vk::ComponentSwizzle::IDENTITY,
                g: vk::ComponentSwizzle::IDENTITY,
                b: vk::ComponentSwizzle::IDENTITY,
                a: vk::ComponentSwizzle::IDENTITY,
            })
            .subresource_range(vk::ImageSubresourceRange {
                layer_count: 1, // Combined view uses single layer
                ..image_subresource_range
            });

        // Build pNext chain for combined view.
        // Chain order (when both present): viewInfo -> ycbcrConversionInfo -> usageCreateInfo
        // SamplerYcbcrConversionInfo doesn't have push_next in ash, so we chain manually.
        let mut ycbcr_conversion_info = vk::SamplerYcbcrConversionInfo::builder()
            .conversion(ycbcr_conversion);
        let mut combined_usage_info =
            vk::ImageViewUsageCreateInfo::builder().usage(combined_view_usage);

        if ycbcr_conversion != vk::SamplerYcbcrConversion::null() {
            if !combined_view_usage.is_empty() {
                // SamplerYcbcrConversionInfoBuilder doesn't support push_next
                // for ImageViewUsageCreateInfo, so chain via raw pointer.
                unsafe {
                    let ycbcr_ptr = &mut ycbcr_conversion_info
                        as *mut _ as *mut vk::SamplerYcbcrConversionInfo;
                    (*ycbcr_ptr).next =
                        &mut combined_usage_info as *mut _ as *const _;
                }
            }
            view_info = view_info.push_next(&mut ycbcr_conversion_info);
        } else if !combined_view_usage.is_empty() {
            view_info = view_info.push_next(&mut combined_usage_info);
        }

        // Create combined view (index 0)
        unsafe {
            image_views[num_views as usize] =
                device.create_image_view(&view_info, None)?;
        }
        num_views += 1;

        // Create per-plane views for compute storage
        if let Some(mp) = mp_info {
            view_info.next = std::ptr::null();
            let view_type = if image_subresource_range.layer_count > 1 {
                vk::ImageViewType::_2D_ARRAY
            } else {
                vk::ImageViewType::_2D
            };
            view_info.view_type = view_type;
            view_info.subresource_range = image_subresource_range;

            let plane_usage = if !plane_usage_override.is_empty() {
                plane_usage_override
            } else {
                let mut pu = image_resource.image_create_info.usage;
                pu &= !(vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR
                    | vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR
                    | vk::ImageUsageFlags::VIDEO_DECODE_SRC_KHR
                    | vk::ImageUsageFlags::VIDEO_ENCODE_DST_KHR
                    | vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR
                    | vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR);
                pu
            };
            let mut plane_usage_info = vk::ImageViewUsageCreateInfo::builder()
                .usage(plane_usage);
            view_info = view_info.push_next(&mut plane_usage_info);

            // Create Y plane view
            view_info.format = mp.vk_plane_format[num_planes as usize];
            view_info.subresource_range.aspect_mask = vk::ImageAspectFlags::from_bits_truncate(
                vk::ImageAspectFlags::PLANE_0.bits() << num_planes,
            );
            unsafe {
                match device.create_image_view(&view_info, None) {
                    Ok(v) => image_views[num_views as usize] = v,
                    Err(e) => {
                        cleanup_views(device, &image_views, num_views);
                        return Err(e.into());
                    }
                }
            }
            num_views += 1;
            num_planes += 1;

            if mp.planes_layout.number_of_extra_planes > 0 {
                view_info.format = mp.vk_plane_format[num_planes as usize];
                view_info.subresource_range.aspect_mask =
                    vk::ImageAspectFlags::from_bits_truncate(
                        vk::ImageAspectFlags::PLANE_0.bits() << num_planes,
                    );
                unsafe {
                    match device.create_image_view(&view_info, None) {
                        Ok(v) => image_views[num_views as usize] = v,
                        Err(e) => {
                            cleanup_views(device, &image_views, num_views);
                            return Err(e.into());
                        }
                    }
                }
                num_views += 1;
                num_planes += 1;

                if mp.planes_layout.number_of_extra_planes > 1 {
                    view_info.format = mp.vk_plane_format[num_planes as usize];
                    view_info.subresource_range.aspect_mask =
                        vk::ImageAspectFlags::from_bits_truncate(
                            vk::ImageAspectFlags::PLANE_0.bits() << num_planes,
                        );
                    unsafe {
                        match device.create_image_view(&view_info, None) {
                            Ok(v) => image_views[num_views as usize] = v,
                            Err(e) => {
                                cleanup_views(device, &image_views, num_views);
                                return Err(e.into());
                            }
                        }
                    }
                    num_views += 1;
                    num_planes += 1;
                }
            }
        }

        Ok(Arc::new(Self {
            device: device.clone(),
            image_resource: image_resource.clone(),
            image_views,
            image_subresource_range,
            num_views,
            num_planes,
        }))
    }
}

impl Drop for VkImageResourceView {
    fn drop(&mut self) {
        for i in 0..self.num_views as usize {
            if self.image_views[i] != vk::ImageView::null() {
                unsafe {
                    self.device
                        .destroy_image_view(self.image_views[i], None);
                }
                self.image_views[i] = vk::ImageView::null();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Destroy a slice of image views, skipping null handles.
fn cleanup_views(device: &vulkanalia::Device, views: &[vk::ImageView; 4], count: u32) {
    for i in 0..count as usize {
        if views[i] != vk::ImageView::null() {
            unsafe {
                device.destroy_image_view(views[i], None);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a dummy vulkanalia::Device for testing pure logic that never
    /// invokes device methods. Uses MaybeUninit to avoid the zeroed-initialization
    /// panic on function pointers.
    ///
    /// SAFETY: Caller must never call Vulkan methods on the returned device.
    unsafe fn dummy_device() -> vulkanalia::Device {
        std::mem::MaybeUninit::uninit().assume_init()
    }

    #[test]
    fn test_ycbcr_vk_format_info_nv12() {
        let info = ycbcr_vk_format_info(vk::Format::G8_B8R8_2PLANE_420_UNORM);
        assert!(info.is_some());
        let info = info.unwrap();
        assert_eq!(info.planes_layout.layout, YcbcrPlanesLayout::SemiPlanarCbCrInterleaved);
        assert_eq!(info.planes_layout.bpp, YcbcrBpp::Bpp8);
        assert!(info.planes_layout.secondary_plane_subsampled_x);
        assert!(info.planes_layout.secondary_plane_subsampled_y);
        assert_eq!(info.planes_layout.number_of_extra_planes, 1);
        assert_eq!(info.vk_plane_format[0], vk::Format::R8_UNORM);
        assert_eq!(info.vk_plane_format[1], vk::Format::R8G8_UNORM);
    }

    #[test]
    fn test_ycbcr_vk_format_info_p010() {
        let info = ycbcr_vk_format_info(vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16);
        assert!(info.is_some());
        let info = info.unwrap();
        assert_eq!(info.planes_layout.bpp, YcbcrBpp::Bpp10);
        assert_eq!(info.planes_layout.number_of_extra_planes, 1);
    }

    #[test]
    fn test_ycbcr_vk_format_info_3plane() {
        let info = ycbcr_vk_format_info(vk::Format::G8_B8_R8_3PLANE_420_UNORM);
        assert!(info.is_some());
        let info = info.unwrap();
        assert_eq!(info.planes_layout.number_of_extra_planes, 2);
        assert_eq!(info.planes_layout.layout, YcbcrPlanesLayout::PlanarCbCrBlockJoined);
    }

    #[test]
    fn test_ycbcr_vk_format_info_non_ycbcr() {
        assert!(ycbcr_vk_format_info(vk::Format::R8G8B8A8_UNORM).is_none());
        assert!(ycbcr_vk_format_info(vk::Format::B8G8R8A8_SRGB).is_none());
        assert!(ycbcr_vk_format_info(vk::Format::UNDEFINED).is_none());
    }

    #[test]
    fn test_is_16bit_detection() {
        // 8-bit format should not be 16-bit
        let nv12 = ycbcr_vk_format_info(vk::Format::G8_B8R8_2PLANE_420_UNORM).unwrap();
        assert_eq!(nv12.planes_layout.bpp, YcbcrBpp::Bpp8);

        // 10-bit format should report as 16-bit (non-8bpp treated as 16bpp)
        let p010 = ycbcr_vk_format_info(vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16).unwrap();
        assert_ne!(p010.planes_layout.bpp, YcbcrBpp::Bpp8);
    }

    #[test]
    fn test_is_compatible_logic() {
        // Test the pure logic of is_compatible by constructing two ImageCreateInfo structs.
        let base = vk::ImageCreateInfo {
            s_type: vk::StructureType::IMAGE_CREATE_INFO,
            next: std::ptr::null(),
            flags: vk::ImageCreateFlags::empty(),
            image_type: vk::ImageType::_2D,
            format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
            extent: vk::Extent3D {
                width: 1920,
                height: 1080,
                depth: 1,
            },
            mip_levels: 1,
            array_layers: 1,
            samples: vk::SampleCountFlags::_1,
            tiling: vk::ImageTiling::OPTIMAL,
            usage: vk::ImageUsageFlags::SAMPLED,
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            queue_family_index_count: 0,
            queue_family_indices: std::ptr::null(),
            initial_layout: vk::ImageLayout::UNDEFINED,

        };

        // Same or smaller should be compatible
        let smaller = vk::ImageCreateInfo {
            extent: vk::Extent3D {
                width: 1280,
                height: 720,
                depth: 1,
            },
            ..base
        };

        // Use a mock resource to test is_compatible
        let resource = VkImageResource::new_test_stub(base);

        assert!(resource.is_compatible(&smaller));
        assert!(resource.is_compatible(&base));

        // Larger width should NOT be compatible
        let larger = vk::ImageCreateInfo {
            extent: vk::Extent3D {
                width: 3840,
                height: 2160,
                depth: 1,
            },
            ..base
        };
        assert!(!resource.is_compatible(&larger));

        // Different format should NOT be compatible
        let diff_format = vk::ImageCreateInfo {
            format: vk::Format::R8G8B8A8_UNORM,
            ..base
        };
        assert!(!resource.is_compatible(&diff_format));

        // Different tiling should NOT be compatible
        let diff_tiling = vk::ImageCreateInfo {
            tiling: vk::ImageTiling::LINEAR,
            ..base
        };
        assert!(!resource.is_compatible(&diff_tiling));

        // Different image type should NOT be compatible
        let diff_type = vk::ImageCreateInfo {
            image_type: vk::ImageType::_3D,
            ..base
        };
        assert!(!resource.is_compatible(&diff_type));
    }

    #[test]
    fn test_drm_format_modifier_detection() {
        // Test that uses_drm_format_modifier is set correctly
        let base_info = vk::ImageCreateInfo {
            s_type: vk::StructureType::IMAGE_CREATE_INFO,
            next: std::ptr::null(),
            flags: vk::ImageCreateFlags::empty(),
            image_type: vk::ImageType::_2D,
            format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
            extent: vk::Extent3D { width: 1920, height: 1080, depth: 1 },
            mip_levels: 1,
            array_layers: 1,
            samples: vk::SampleCountFlags::_1,
            tiling: vk::ImageTiling::OPTIMAL,
            usage: vk::ImageUsageFlags::SAMPLED,
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            queue_family_index_count: 0,
            queue_family_indices: std::ptr::null(),
            initial_layout: vk::ImageLayout::UNDEFINED,
        };

        // No DRM modifier
        let r1 = std::mem::ManuallyDrop::new(VkImageResource::new_inner(
            unsafe { dummy_device() },
            &base_info,
            vk::Image::null(),
            0, 0, None, None, None, 0, 0,
        ));
        assert!(!r1.uses_drm_format_modifier);

        // With DRM modifier value
        let r2 = std::mem::ManuallyDrop::new(VkImageResource::new_inner(
            unsafe { dummy_device() },
            &base_info,
            vk::Image::null(),
            0, 0, None, None, None, 1, 2,
        ));
        assert!(r2.uses_drm_format_modifier);
        assert_eq!(r2.drm_format_modifier, 1);
        assert_eq!(r2.memory_plane_count, 2);

        // With DRM tiling
        let drm_info = vk::ImageCreateInfo {
            tiling: vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
            ..base_info
        };
        let r3 = std::mem::ManuallyDrop::new(VkImageResource::new_inner(
            unsafe { dummy_device() },
            &drm_info,
            vk::Image::null(),
            0, 0, None, None, None, 0, 0,
        ));
        assert!(r3.uses_drm_format_modifier);
    }

    #[test]
    fn test_plane_layout_bounds() {
        let mut resource = VkImageResource::new_test_stub(vk::ImageCreateInfo {
            s_type: vk::StructureType::IMAGE_CREATE_INFO,
            next: std::ptr::null(),
            flags: vk::ImageCreateFlags::empty(),
            image_type: vk::ImageType::_2D,
            format: vk::Format::R8G8B8A8_UNORM,
            extent: vk::Extent3D { width: 64, height: 64, depth: 1 },
            mip_levels: 1,
            array_layers: 1,
            samples: vk::SampleCountFlags::_1,
            tiling: vk::ImageTiling::LINEAR,
            usage: vk::ImageUsageFlags::SAMPLED,
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            queue_family_index_count: 0,
            queue_family_indices: std::ptr::null(),
            initial_layout: vk::ImageLayout::UNDEFINED,
        });
        resource.is_linear_image = true;

        // plane_index > 2 should return None
        assert!(resource.get_plane_layout(3).is_none());
        // Default zero layout should return None (size=0 and row_pitch=0)
        assert!(resource.get_plane_layout(0).is_none());

        // Non-linear should return None for get_subresource_layout
        let non_linear = VkImageResource::new_test_stub(vk::ImageCreateInfo::default());
        assert!(non_linear.get_subresource_layout().is_none());
        assert!(non_linear.get_plane_layout(0).is_none());
    }

    #[test]
    fn test_memory_plane_layout_bounds() {
        let resource = std::mem::ManuallyDrop::new(VkImageResource {
            device: unsafe { dummy_device() },
            image: vk::Image::null(),
            image_create_info: vk::ImageCreateInfo {
                s_type: vk::StructureType::IMAGE_CREATE_INFO,
                next: std::ptr::null(),
                flags: vk::ImageCreateFlags::empty(),
                image_type: vk::ImageType::_2D,
                format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
                extent: vk::Extent3D { width: 1920, height: 1080, depth: 1 },
                mip_levels: 1,
                array_layers: 1,
                samples: vk::SampleCountFlags::_1,
                tiling: vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
                usage: vk::ImageUsageFlags::SAMPLED,
                sharing_mode: vk::SharingMode::EXCLUSIVE,
                queue_family_index_count: 0,
                queue_family_indices: std::ptr::null(),
                initial_layout: vk::ImageLayout::UNDEFINED,
            },
            image_offset: 0,
            image_size: 0,
            device_memory: None,
            allocator: None,
            allocation: None,
            layouts: [vk::SubresourceLayout::default(); 3],
            memory_plane_layouts: [vk::SubresourceLayout {
                offset: 0,
                size: 1920 * 1080,
                row_pitch: 1920,
                array_pitch: 0,
                depth_pitch: 0,
            }, vk::SubresourceLayout {
                offset: 1920 * 1080,
                size: 1920 * 540,
                row_pitch: 1920,
                array_pitch: 0,
                depth_pitch: 0,
            }, vk::SubresourceLayout::default(), vk::SubresourceLayout::default()],
            drm_format_modifier: 1,
            memory_plane_count: 2,
            is_linear_image: false,
            is_16bit: false,
            is_subsampled_x: true,
            is_subsampled_y: true,
            uses_drm_format_modifier: true,
            owns_resources: false,
        });

        // Valid planes
        assert!(resource.get_memory_plane_layout(0).is_some());
        assert!(resource.get_memory_plane_layout(1).is_some());
        // Out of range
        assert!(resource.get_memory_plane_layout(2).is_none());
        assert!(resource.get_memory_plane_layout(4).is_none());

        // Verify layout values
        let l0 = resource.get_memory_plane_layout(0).unwrap();
        assert_eq!(l0.offset, 0);
        assert_eq!(l0.size, 1920 * 1080);
        let l1 = resource.get_memory_plane_layout(1).unwrap();
        assert_eq!(l1.offset, 1920 * 1080);
    }
}
