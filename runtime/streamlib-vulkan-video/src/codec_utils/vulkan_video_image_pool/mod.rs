// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#![allow(
    dead_code,
    reason = "ported from the C++ image pool; not every field and method is wired in yet"
)]

//! Port of VulkanVideoImagePool.h + VulkanVideoImagePool.cpp
//!
//! Manages a pool of VkImage resources for decoded picture buffers (DPB).
//! The pool tracks which images are in use, allocates on demand, and recycles
//! them when released.
//!
//! Key Rust divergences from C++:
//! - `VkSharedBaseObj<T>` / ref-counting is replaced by `Arc<Mutex<T>>` and
//!   `Option<Arc<...>>` where shared ownership is needed.
//! - The C++ custom deleter on `shared_ptr` that returns nodes to the pool is
//!   modeled via an explicit `PoolNodeHandle` wrapper that calls
//!   `release_image_to_pool` on `Drop`.
//! - `VulkanDeviceContext*` is replaced by `vulkanalia::Device` (the Vulkan dispatch
//!   table) since that is what we actually need for API calls.
//! - Bitfield flags (`m_usesImageArray : 1`) become plain `bool`.
//! - The C++ `std::mutex` becomes `std::sync::Mutex` guarding the mutable pool
//!   state.

use std::sync::{Arc, Mutex, Weak};

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

// ---------------------------------------------------------------------------
// Forward-reference: VkImageResource / VkImageResourceView are defined in a
// sibling module.  The pool stores indices and Vulkan handles rather than
// Arc references to those types so that we avoid a circular-dependency.  When
// the concrete resource types are ported the pool can be updated to use them
// directly.  For now we define minimal placeholder traits/types so that the
// pool logic compiles and is testable in isolation.
// ---------------------------------------------------------------------------

/// Placeholder for a concrete `VkImageResource` handle.
///
/// In the final integration this will be replaced by the real type from
/// `super::vk_image_resource`.  The pool only needs the image view handle
/// and the `VkImageCreateInfo` extent to populate
/// `VkVideoPictureResourceInfoKHR`.
#[derive(Clone, Debug, Default)]
pub struct ImageResourceHandle {
    pub image: vk::Image,
    pub image_view: vk::ImageView,
    pub create_info_extent: vk::Extent3D,
}

// ---------------------------------------------------------------------------
// VulkanVideoImagePoolNode
// ---------------------------------------------------------------------------

/// State for a single slot in the image pool.
///
/// Corresponds to C++ `VulkanVideoImagePoolNode`.
#[derive(Debug)]
pub struct VulkanVideoImagePoolNode {
    /// Current image layout.
    current_image_layout: vk::ImageLayout,

    /// Vulkan picture-resource info filled when the image is created / bound.
    picture_resource_info: vk::VideoPictureResourceInfoKHR,

    /// The image resource view handle for this slot.
    image_resource_view: Option<ImageResourceHandle>,

    /// Back-pointer to the owning pool (weak to avoid preventing pool drop).
    parent: Option<Weak<Mutex<VulkanVideoImagePoolInner>>>,

    /// Index within the parent pool, or -1 if not bound.
    parent_index: i32,

    /// If `true` the image must be recreated before next use.
    recreate_image: bool,

    /// Timeline semaphore (created lazily).
    timeline_semaphore: vk::Semaphore,

    /// Cached semaphore submit info.
    semaphore_submit_info: vk::SemaphoreSubmitInfo,
}

impl Default for VulkanVideoImagePoolNode {
    fn default() -> Self {
        Self {
            current_image_layout: vk::ImageLayout::UNDEFINED,
            picture_resource_info: vk::VideoPictureResourceInfoKHR {
                s_type: vk::StructureType::VIDEO_PICTURE_RESOURCE_INFO_KHR,
                next: std::ptr::null(),
                coded_offset: vk::Offset2D { x: 0, y: 0 },
                coded_extent: vk::Extent2D {
                    width: 0,
                    height: 0,
                },
                base_array_layer: 0,
                image_view_binding: vk::ImageView::null(),
            },
            image_resource_view: None,
            parent: None,
            parent_index: -1,
            recreate_image: false,
            timeline_semaphore: vk::Semaphore::null(),
            semaphore_submit_info: vk::SemaphoreSubmitInfo {
                s_type: vk::StructureType::SEMAPHORE_SUBMIT_INFO,
                next: std::ptr::null(),
                semaphore: vk::Semaphore::null(),
                value: 0,
                stage_mask: vk::PipelineStageFlags2::empty(),
                device_index: 0,
            },
        }
    }
}

impl VulkanVideoImagePoolNode {
    /// Create a new default (empty) node.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if this node contains a valid image view.
    pub fn image_exist(&self) -> bool {
        self.image_resource_view
            .as_ref()
            .is_some_and(|h| h.image_view != vk::ImageView::null())
    }

    /// Returns `true` if the image must be (re-)created before use.
    pub fn recreate_image(&self) -> bool {
        !self.image_exist() || self.recreate_image
    }

    /// Mark the image for recreation on next use.
    pub fn respec_image(&mut self) {
        self.recreate_image = true;
    }

    /// Transition the node to a new layout.  Returns `false` if the image is
    /// not in a usable state.
    pub fn set_new_layout(&mut self, new_layout: vk::ImageLayout) -> bool {
        if self.recreate_image() || !self.image_exist() {
            return false;
        }
        self.current_image_layout = new_layout;
        true
    }

    /// Get a reference to the picture resource info.
    pub fn get_picture_resource_info(&self) -> &vk::VideoPictureResourceInfoKHR {
        &self.picture_resource_info
    }

    /// Get a mutable reference to the picture resource info.
    pub fn get_picture_resource_info_mut(&mut self) -> &mut vk::VideoPictureResourceInfoKHR {
        &mut self.picture_resource_info
    }

    /// Index within the parent pool, or -1.
    pub fn get_image_index(&self) -> i32 {
        self.parent_index
    }

    /// Get a copy of the semaphore submit info.
    pub fn get_semaphore_submit_info(&self) -> vk::SemaphoreSubmitInfo {
        self.semaphore_submit_info
    }

    /// Get a reference to the image resource handle (if any).
    pub fn get_image_view(&self) -> Option<&ImageResourceHandle> {
        if self.image_exist() {
            self.image_resource_view.as_ref()
        } else {
            None
        }
    }

    /// Lazily create a timeline semaphore and record the given value / stage.
    ///
    /// Returns the updated `SemaphoreSubmitInfo`, or a zeroed structure on
    /// failure.
    ///
    /// # Safety
    ///
    /// `device` must be a valid `vulkanalia::Device` handle whose lifetime exceeds
    /// this node.
    pub unsafe fn set_timeline_semaphore_value(
        &mut self,
        device: &vulkanalia::Device,
        value: u64,
        stage_mask: vk::PipelineStageFlags2,
        device_index: u32,
    ) -> vk::SemaphoreSubmitInfo {
        unsafe {
            if self.timeline_semaphore == vk::Semaphore::null() {
                let mut type_info = vk::SemaphoreTypeCreateInfo::builder()
                    .semaphore_type(vk::SemaphoreType::TIMELINE)
                    .initial_value(0);

                let create_info = vk::SemaphoreCreateInfo::builder().push_next(&mut type_info);

                match device.create_semaphore(&create_info, None) {
                    Ok(sem) => {
                        self.timeline_semaphore = sem;
                        self.semaphore_submit_info = vk::SemaphoreSubmitInfo {
                            s_type: vk::StructureType::SEMAPHORE_SUBMIT_INFO,
                            next: std::ptr::null(),
                            semaphore: sem,
                            value: 0,
                            stage_mask: vk::PipelineStageFlags2::empty(),
                            device_index: 0,
                        };
                    }
                    Err(_) => {
                        return empty_semaphore_submit_info();
                    }
                }
            }

            self.semaphore_submit_info.value = value;
            self.semaphore_submit_info.stage_mask = stage_mask;
            self.semaphore_submit_info.device_index = device_index;

            self.semaphore_submit_info
        }
    }

    /// Populate this node with an image.
    ///
    /// Mirrors `VulkanVideoImagePoolNode::CreateImage`.  In the full port this
    /// will invoke `VkImageResource::Create` etc.  For now it accepts a
    /// pre-built `ImageResourceHandle`.
    pub fn create_image(
        &mut self,
        handle: ImageResourceHandle,
        image_create_extent: vk::Extent3D,
        image_index: u32,
        uses_image_array: bool,
        uses_image_view_array: bool,
    ) {
        let base_array_layer = if uses_image_array { image_index } else { 0 };

        if !uses_image_view_array {
            // Per-node view: baseArrayLayer in the picture resource is 0.
            self.picture_resource_info.base_array_layer = 0;
        } else {
            self.picture_resource_info.base_array_layer = base_array_layer;
        }

        self.image_resource_view = Some(handle);

        self.current_image_layout = vk::ImageLayout::UNDEFINED;
        self.recreate_image = false;
        self.picture_resource_info.s_type = vk::StructureType::VIDEO_PICTURE_RESOURCE_INFO_KHR;
        self.picture_resource_info.coded_offset = vk::Offset2D { x: 0, y: 0 };
        self.picture_resource_info.coded_extent = vk::Extent2D {
            width: image_create_extent.width,
            height: image_create_extent.height,
        };
        self.picture_resource_info.image_view_binding = self
            .image_resource_view
            .as_ref()
            .map_or(vk::ImageView::null(), |h| h.image_view);
    }

    /// Create an external (non-pooled) node wrapping a pre-existing image
    /// resource view.
    ///
    /// Mirrors `VulkanVideoImagePoolNode::CreateExternal`.
    pub fn create_external(
        handle: ImageResourceHandle,
        extent: vk::Extent2D,
        initial_layout: vk::ImageLayout,
    ) -> Self {
        let mut node = Self::new();
        node.current_image_layout = initial_layout;
        node.picture_resource_info.s_type = vk::StructureType::VIDEO_PICTURE_RESOURCE_INFO_KHR;
        node.picture_resource_info.coded_offset = vk::Offset2D { x: 0, y: 0 };
        node.picture_resource_info.coded_extent = extent;
        node.picture_resource_info.base_array_layer = 0;
        node.picture_resource_info.image_view_binding = handle.image_view;
        node.image_resource_view = Some(handle);
        // parent stays None (no pool to return to)
        // parent_index stays -1
        node
    }

    /// Initialize the node for a given device context.
    ///
    /// Mirrors `VulkanVideoImagePoolNode::Init`.  Currently a no-op beyond
    /// being the place where `m_vkDevCtx` was stored in C++.
    pub fn init(&mut self) {
        // In C++ this stored the device context pointer.  In Rust the device
        // handle is passed to methods that need it rather than cached.
    }

    /// Release resources held by this node.
    ///
    /// # Safety
    ///
    /// If a timeline semaphore was created, `device` must be the same device
    /// that was used to create it.
    pub unsafe fn deinit(&mut self, device: Option<&vulkanalia::Device>) {
        unsafe {
            self.image_resource_view = None;

            if self.timeline_semaphore != vk::Semaphore::null() {
                if let Some(dev) = device {
                    dev.destroy_semaphore(self.timeline_semaphore, None);
                }
                self.timeline_semaphore = vk::Semaphore::null();
            }

            self.semaphore_submit_info = empty_semaphore_submit_info();
        }
    }

    // -- private helpers --

    fn set_parent(&mut self, parent: Weak<Mutex<VulkanVideoImagePoolInner>>, index: i32) {
        debug_assert!(self.parent.is_none());
        self.parent = Some(parent);
        debug_assert_eq!(self.parent_index, -1);
        self.parent_index = index;
    }

    fn clear_parent(&mut self) {
        self.parent = None;
        self.parent_index = -1;
    }
}

/// Return a zeroed `SemaphoreSubmitInfo` (used as the "invalid" sentinel).
fn empty_semaphore_submit_info() -> vk::SemaphoreSubmitInfo {
    vk::SemaphoreSubmitInfo {
        s_type: vk::StructureType::SEMAPHORE_SUBMIT_INFO,
        next: std::ptr::null(),
        semaphore: vk::Semaphore::null(),
        value: 0,
        stage_mask: vk::PipelineStageFlags2::empty(),
        device_index: 0,
    }
}

// ---------------------------------------------------------------------------
// PoolNodeHandle — RAII wrapper that returns the node to the pool on drop
// ---------------------------------------------------------------------------

/// A handle to a checked-out pool node.
///
/// When dropped the node is returned to the pool automatically (mirrors the
/// C++ custom-deleter `std::shared_ptr` in `GetAvailableImage`).
pub struct PoolNodeHandle {
    /// Index into the pool's `image_resources` vector.
    index: u32,
    /// Weak ref to the pool inner state.  If the pool has been dropped we
    /// silently skip the return.
    pool: Weak<Mutex<VulkanVideoImagePoolInner>>,
}

impl Drop for PoolNodeHandle {
    fn drop(&mut self) {
        if let Some(arc) = self.pool.upgrade() {
            let mut inner = arc.lock().expect("pool lock poisoned");
            inner.image_resources[self.index as usize].clear_parent();
            inner.release_image_to_pool(self.index);
        }
    }
}

impl PoolNodeHandle {
    /// Access the underlying node through the pool.
    ///
    /// Returns `None` if the pool has been dropped.
    pub fn with_node<F, R>(&self, pool: &VulkanVideoImagePool, f: F) -> Option<R>
    where
        F: FnOnce(&VulkanVideoImagePoolNode) -> R,
    {
        let inner = pool.inner.lock().expect("pool lock poisoned");
        Some(f(&inner.image_resources[self.index as usize]))
    }

    /// Mutable access to the underlying node through the pool.
    pub fn with_node_mut<F, R>(&self, pool: &VulkanVideoImagePool, f: F) -> Option<R>
    where
        F: FnOnce(&mut VulkanVideoImagePoolNode) -> R,
    {
        let mut inner = pool.inner.lock().expect("pool lock poisoned");
        Some(f(&mut inner.image_resources[self.index as usize]))
    }

    /// The slot index of this node within the pool.
    pub fn index(&self) -> u32 {
        self.index
    }
}

// ---------------------------------------------------------------------------
// VulkanVideoImagePoolInner  (behind the Mutex)
// ---------------------------------------------------------------------------

/// The mutable interior of the pool, protected by a `Mutex`.
///
/// Separated from the outer `VulkanVideoImagePool` so that the mutex only
/// guards the fields that actually need synchronisation.
#[derive(Debug)]
struct VulkanVideoImagePoolInner {
    queue_family_index: u32,
    image_create_info: ImageCreateInfoSnapshot,
    required_mem_props: vk::MemoryPropertyFlags,
    pool_size: u32,
    next_node_to_use: u32,
    aspect_mask: vk::ImageAspectFlags,
    uses_image_array: bool,
    uses_image_view_array: bool,
    uses_linear_image: bool,
    /// Bitmask — bit `i` is set when slot `i` is available.
    available_pool_nodes: u64,
    image_resources: Vec<VulkanVideoImagePoolNode>,
}

impl VulkanVideoImagePoolInner {
    fn release_image_to_pool(&mut self, image_index: u32) -> bool {
        debug_assert!(
            self.available_pool_nodes & (1u64 << image_index) == 0,
            "releasing an already-available image (index {image_index})"
        );
        self.available_pool_nodes |= 1u64 << image_index;
        true
    }
}

// ---------------------------------------------------------------------------
// Snapshot of VkImageCreateInfo fields we store in the pool
// ---------------------------------------------------------------------------

/// We cannot store a raw `vk::ImageCreateInfo` because it contains pointers
/// (`pNext`, `pQueueFamilyIndices`).  Instead we keep the scalar fields we
/// need and reconstruct the struct on demand.
#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "the fields mirror VkImageCreateInfo for compatibility checks"
)]
pub(crate) struct ImageCreateInfoSnapshot {
    s_type: vk::StructureType,
    flags: vk::ImageCreateFlags,
    image_type: vk::ImageType,
    format: vk::Format,
    extent: vk::Extent3D,
    mip_levels: u32,
    array_layers: u32,
    samples: vk::SampleCountFlags,
    tiling: vk::ImageTiling,
    usage: vk::ImageUsageFlags,
    sharing_mode: vk::SharingMode,
    initial_layout: vk::ImageLayout,
}

impl Default for ImageCreateInfoSnapshot {
    fn default() -> Self {
        Self {
            s_type: vk::StructureType::IMAGE_CREATE_INFO,
            flags: vk::ImageCreateFlags::empty(),
            image_type: vk::ImageType::_2D,
            format: vk::Format::UNDEFINED,
            extent: vk::Extent3D {
                width: 0,
                height: 0,
                depth: 1,
            },
            mip_levels: 1,
            array_layers: 1,
            samples: vk::SampleCountFlags::_1,
            tiling: vk::ImageTiling::OPTIMAL,
            usage: vk::ImageUsageFlags::empty(),
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            initial_layout: vk::ImageLayout::UNDEFINED,
        }
    }
}

// ---------------------------------------------------------------------------
// VulkanVideoImagePool
// ---------------------------------------------------------------------------

/// Maximum number of images the pool supports (matches C++ `maxImages`).
pub const MAX_IMAGES: usize = 64;

/// A pool of Vulkan images for video decode / encode DPB usage.
///
/// Corresponds to C++ `VulkanVideoImagePool`.
///
/// Thread-safety: all mutable state is behind `Arc<Mutex<..>>` so the pool
/// can be shared across threads.
pub struct VulkanVideoImagePool {
    inner: Arc<Mutex<VulkanVideoImagePoolInner>>,
}

impl VulkanVideoImagePool {
    /// Create a new, empty pool.
    ///
    /// Mirrors `VulkanVideoImagePool::Create`.
    pub fn create() -> Self {
        let mut resources = Vec::with_capacity(MAX_IMAGES);
        for _ in 0..MAX_IMAGES {
            resources.push(VulkanVideoImagePoolNode::new());
        }

        Self {
            inner: Arc::new(Mutex::new(VulkanVideoImagePoolInner {
                queue_family_index: u32::MAX,
                image_create_info: ImageCreateInfoSnapshot::default(),
                required_mem_props: vk::MemoryPropertyFlags::DEVICE_LOCAL,
                pool_size: 0,
                next_node_to_use: 0,
                aspect_mask: vk::ImageAspectFlags::COLOR,
                uses_image_array: false,
                uses_image_view_array: false,
                uses_linear_image: false,
                available_pool_nodes: 0u64,
                image_resources: resources,
            })),
        }
    }

    /// Configure (or reconfigure) the pool.
    ///
    /// Mirrors `VulkanVideoImagePool::Configure`.
    ///
    /// `image_factory` is called for each slot that needs an image created.
    /// It receives `(image_index, &ImageCreateInfoSnapshot)` and must return
    /// an `ImageResourceHandle`.  This callback replaces the direct Vulkan
    /// calls that the C++ code makes via `VkImageResource::Create`, keeping
    /// the pool logic decoupled from concrete resource management.
    pub(crate) fn configure<F>(
        &self,
        num_images: u32,
        image_format: vk::Format,
        max_image_extent: vk::Extent2D,
        image_usage: vk::ImageUsageFlags,
        queue_family_index: u32,
        required_mem_props: vk::MemoryPropertyFlags,
        has_video_profile: bool,
        aspect_mask: vk::ImageAspectFlags,
        use_image_array: bool,
        use_image_view_array: bool,
        use_linear_image: bool,
        mut image_factory: F,
    ) -> vk::Result
    where
        F: FnMut(u32, &ImageCreateInfoSnapshot) -> Result<ImageResourceHandle, vk::Result>,
    {
        let mut inner = self.inner.lock().expect("pool lock poisoned");

        if num_images as usize > inner.image_resources.len() {
            tracing::error!(
                "Number of requested images ({}) exceeds the max size of the image array ({})",
                num_images,
                inner.image_resources.len()
            );
            return vk::Result::ERROR_TOO_MANY_OBJECTS;
        }

        let reconfigure_images = (inner.pool_size > 0)
            && (inner.image_create_info.s_type == vk::StructureType::IMAGE_CREATE_INFO)
            && ((inner.image_create_info.format != image_format)
                || (inner.image_create_info.extent.width < max_image_extent.width)
                || (inner.image_create_info.extent.height < max_image_extent.height));

        // Initialize newly added slots.
        for image_index in inner.pool_size..num_images {
            inner.image_resources[image_index as usize].init();
            inner.available_pool_nodes |= 1u64 << image_index;
        }

        let use_image_array = if use_image_view_array {
            true
        } else {
            use_image_array
        };

        // Store image create parameters.
        inner.queue_family_index = queue_family_index;
        inner.required_mem_props = required_mem_props;

        let mut flags = vk::ImageCreateFlags::MUTABLE_FORMAT;
        let has_video_usage = image_usage.intersects(
            vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR
                | vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR
                | vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR
                | vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR,
        );
        if has_video_usage && !has_video_profile {
            flags |= vk::ImageCreateFlags::EXTENDED_USAGE
                | vk::ImageCreateFlags::VIDEO_PROFILE_INDEPENDENT_KHR;
        }

        let tiling = if use_linear_image {
            vk::ImageTiling::LINEAR
        } else {
            vk::ImageTiling::OPTIMAL
        };

        inner.image_create_info = ImageCreateInfoSnapshot {
            s_type: vk::StructureType::IMAGE_CREATE_INFO,
            flags,
            image_type: vk::ImageType::_2D,
            format: image_format,
            extent: vk::Extent3D {
                width: max_image_extent.width,
                height: max_image_extent.height,
                depth: 1,
            },
            mip_levels: 1,
            array_layers: if use_image_array { num_images } else { 1 },
            samples: vk::SampleCountFlags::_1,
            tiling,
            usage: image_usage,
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            initial_layout: vk::ImageLayout::UNDEFINED,
        };

        // Create / recreate images.
        let first_index = if reconfigure_images {
            0
        } else {
            inner.pool_size
        };
        let max_num_images = std::cmp::max(inner.pool_size, num_images);

        for image_index in first_index..max_num_images {
            let idx = image_index as usize;
            if inner.image_resources[idx].image_exist() && reconfigure_images {
                inner.image_resources[idx].respec_image();
            } else if !inner.image_resources[idx].image_exist() {
                let snapshot = inner.image_create_info.clone();
                match image_factory(image_index, &snapshot) {
                    Ok(handle) => {
                        inner.image_resources[idx].create_image(
                            handle,
                            snapshot.extent,
                            image_index,
                            use_image_array,
                            use_image_view_array,
                        );
                    }
                    Err(e) => {
                        return e;
                    }
                }
            }
        }

        inner.pool_size = num_images;
        inner.uses_image_array = use_image_array;
        inner.uses_image_view_array = use_image_view_array;
        inner.aspect_mask = aspect_mask;
        inner.uses_linear_image = use_linear_image;

        vk::Result::SUCCESS
    }

    /// Acquire an available image from the pool.
    ///
    /// On success returns a `PoolNodeHandle` that will automatically return
    /// the image to the pool when dropped.  Returns `None` if no images are
    /// available.
    ///
    /// `image_factory` is called only if the selected slot needs its image
    /// (re-)created.
    ///
    /// Mirrors `VulkanVideoImagePool::GetAvailableImage`.
    pub(crate) fn get_available_image<F>(
        &self,
        new_image_layout: vk::ImageLayout,
        mut image_factory: F,
    ) -> Option<PoolNodeHandle>
    where
        F: FnMut(u32, &ImageCreateInfoSnapshot) -> Result<ImageResourceHandle, vk::Result>,
    {
        let mut inner = self.inner.lock().expect("pool lock poisoned");

        let available_index = Self::find_available_slot(&mut inner);

        if let Some(idx) = available_index {
            // Ensure the image is created / up-to-date.
            if Self::get_image_set_new_layout(&mut inner, idx, new_image_layout, &mut image_factory)
                .is_err()
            {
                // Creation failed — put the slot back.
                inner.available_pool_nodes |= 1u64 << idx;
                return None;
            }

            let weak = Arc::downgrade(&self.inner);
            inner.image_resources[idx as usize].set_parent(weak.clone(), idx as i32);

            Some(PoolNodeHandle {
                index: idx,
                pool: weak,
            })
        } else {
            None
        }
    }

    /// Release an image back to the pool by index.
    ///
    /// Normally this is done automatically via `PoolNodeHandle::drop` but the
    /// method is exposed for parity with the C++ API.
    pub fn release_image_to_pool(&self, image_index: u32) -> bool {
        let mut inner = self.inner.lock().expect("pool lock poisoned");
        inner.release_image_to_pool(image_index)
    }

    /// De-initialise the pool, releasing all images.
    ///
    /// # Safety
    ///
    /// `device` must be the device that was used to create any timeline
    /// semaphores in the pool nodes.
    pub unsafe fn deinit(&self, device: Option<&vulkanalia::Device>) {
        unsafe {
            let mut inner = self.inner.lock().expect("pool lock poisoned");
            for ndx in 0..inner.pool_size as usize {
                inner.image_resources[ndx].deinit(device);
            }
            inner.pool_size = 0;
        }
    }

    /// Number of images the pool is configured for.
    pub fn size(&self) -> u32 {
        let inner = self.inner.lock().expect("pool lock poisoned");
        inner.pool_size
    }

    /// Read-only access to a node by index.
    pub fn with_node<F, R>(&self, index: u32, f: F) -> R
    where
        F: FnOnce(&VulkanVideoImagePoolNode) -> R,
    {
        let inner = self.inner.lock().expect("pool lock poisoned");
        assert!((index as usize) < inner.image_resources.len());
        f(&inner.image_resources[index as usize])
    }

    /// Mutable access to a node by index.
    pub fn with_node_mut<F, R>(&self, index: u32, f: F) -> R
    where
        F: FnOnce(&mut VulkanVideoImagePoolNode) -> R,
    {
        let mut inner = self.inner.lock().expect("pool lock poisoned");
        assert!((index as usize) < inner.image_resources.len());
        f(&mut inner.image_resources[index as usize])
    }

    // -- private helpers --

    /// Scan the availability bitmask and claim a slot.
    ///
    /// Returns `Some(index)` if a free slot was found and cleared from the
    /// bitmask, or `None` if the pool is exhausted.
    fn find_available_slot(inner: &mut VulkanVideoImagePoolInner) -> Option<u32> {
        if inner.next_node_to_use >= inner.pool_size {
            inner.next_node_to_use = 0;
        }

        // First pass: search from next_node_to_use to end.
        for i in inner.next_node_to_use..inner.pool_size {
            if inner.available_pool_nodes & (1u64 << i) != 0 {
                inner.next_node_to_use = i + 1;
                inner.available_pool_nodes &= !(1u64 << i);
                return Some(i);
            }
        }

        // Wrap-around: search from 0 to next_node_to_use.
        if inner.next_node_to_use > 0 {
            let limit = inner.next_node_to_use;
            inner.next_node_to_use = 0;
            for i in 0..limit {
                if inner.available_pool_nodes & (1u64 << i) != 0 {
                    inner.next_node_to_use = i + 1;
                    inner.available_pool_nodes &= !(1u64 << i);
                    return Some(i);
                }
            }
        }

        None
    }

    /// Ensure image at `index` is created and set its layout.
    fn get_image_set_new_layout<F>(
        inner: &mut VulkanVideoImagePoolInner,
        image_index: u32,
        new_image_layout: vk::ImageLayout,
        image_factory: &mut F,
    ) -> Result<(), vk::Result>
    where
        F: FnMut(u32, &ImageCreateInfoSnapshot) -> Result<ImageResourceHandle, vk::Result>,
    {
        let idx = image_index as usize;

        if inner.image_resources[idx].recreate_image() {
            let snapshot = inner.image_create_info.clone();
            let handle = image_factory(image_index, &snapshot)?;
            inner.image_resources[idx].create_image(
                handle,
                snapshot.extent,
                image_index,
                inner.uses_image_array,
                inner.uses_image_view_array,
            );
        }

        let valid = inner.image_resources[idx].set_new_layout(new_image_layout);
        debug_assert!(valid, "SetNewLayout failed for image index {image_index}");
        if !valid {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
