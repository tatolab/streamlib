// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VulkanVideoFrameBuffer.h + VulkanVideoFrameBuffer.cpp
//!
//! Frame buffer management for decoded video pictures. Allocates and releases
//! frames, manages display order, and handles synchronisation between the
//! decode and display pipelines.
//!
//! Key divergences from C++:
//! - `VkSharedBaseObj<T>` → `Arc<T>` or `Option<Arc<T>>`.
//! - `vkPicBuffBase` / `NvPerFrameDecodeResources` are merged into
//!   `PerFrameDecodeResources` with an `Arc`-based reference count.
//! - Bit-field structs are replaced by regular `bool` fields.
//! - `std::mutex` → `std::sync::Mutex`.
//! - `std::queue<uint8_t>` → `VecDeque<u8>`.
//! - `VulkanDeviceContext*` is represented as `vulkanalia::Device` (the Vulkan
//!   device dispatch table) which is the only thing actually used for
//!   Vulkan API calls. The higher-level context will be threaded through
//!   when the VulkanDeviceContext type is ported.
//! - Virtual interface methods become trait methods on `VulkanVideoFrameBuffer`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;


// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of images the frame buffer can manage (matches C++ `maxImages`).
pub const MAX_IMAGES: usize = 32;

/// Sentinel value for an invalid image type index.
pub const INVALID_IMAGE_TYPE_IDX: u8 = u8::MAX;

/// Maximum number of distinct per-frame image types (decode output, filter
/// output, linear output, etc.).  Mirrors `DecodeFrameBufferIf::MAX_PER_FRAME_IMAGE_TYPES`.
pub const MAX_PER_FRAME_IMAGE_TYPES: usize = 4;

/// Maximum number of external consumers that can register release semaphores.
pub const MAX_EXTERNAL_CONSUMERS: usize = 4;

// ---------------------------------------------------------------------------
// VkVideotimestamp — simple alias
// ---------------------------------------------------------------------------

/// Video timestamp type (mirrors C++ `VkVideotimestamp`).
pub type VkVideotimestamp = i64;

// ---------------------------------------------------------------------------
// DecodedFrameRelease
// ---------------------------------------------------------------------------

/// Information needed to release a decoded frame back to the pool.
/// Mirrors C++ `DecodedFrameRelease`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DecodedFrameRelease {
    pub picture_index: i32,
    pub timestamp: VkVideotimestamp,
    pub has_consumer_signal_fence: bool,
    pub has_consumer_signal_semaphore: bool,
    /// For debugging.
    pub display_order: u64,
    pub decode_order: u64,
}

// ---------------------------------------------------------------------------
// ImageSpecsIndex — which image type indices serve each role
// ---------------------------------------------------------------------------

/// Mirrors C++ `DecodeFrameBufferIf::ImageSpecsIndex`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ImageSpecsIndex {
    pub decode_out: u8,
    pub display_out: u8,
    pub linear_out: u8,
    pub filter_out: u8,
}

// ---------------------------------------------------------------------------
// SemSyncTypeIdx — semaphore sync type
// ---------------------------------------------------------------------------

/// Mirrors C++ `DecodeFrameBufferIf::SemSyncTypeIdx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum SemSyncTypeIdx {
    #[default]
    Decode = 0,
    Filter = 1,
    Display = 2,
    ExternalConsumer0 = 3,
    ExternalConsumer1 = 4,
    ExternalConsumer2 = 5,
    ExternalConsumer3 = 6,
}

/// Compute the timeline semaphore value for a given sync type and frame order.
/// Mirrors C++ `DecodeFrameBufferIf::GetSemaphoreValue`.
///
/// The formula packs the sync-type into the upper bits and the frame order
/// into the lower bits so that each (type, order) pair yields a unique,
/// monotonically increasing value per sync type.
pub fn get_semaphore_value(sync_type: SemSyncTypeIdx, frame_order: u64) -> u64 {
    // Upper 4 bits encode the sync type, lower 60 bits encode the order.
    ((sync_type as u64) << 60) | (frame_order & 0x0FFF_FFFF_FFFF_FFFF)
}

// ---------------------------------------------------------------------------
// InitType — image pool initialisation mode
// ---------------------------------------------------------------------------

/// Mirrors C++ `VulkanVideoFrameBuffer::InitType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InitType {
    /// Only invalidate image layouts; do not recreate or add images.
    InvalidateImagesLayout = 0,
    /// Recreate images because their formats or extent has increased.
    RecreateImages = 1 << 1,
    /// Increase the number of slots available.
    IncreaseNumSlots = 1 << 2,
}

// ---------------------------------------------------------------------------
// FrameSynchronizationInfo
// ---------------------------------------------------------------------------

/// Per-frame synchronisation primitives passed between the decoder and
/// the frame buffer.  Mirrors C++ `VulkanVideoFrameBuffer::FrameSynchronizationInfo`.
#[derive(Debug, Clone, Copy)]
pub struct FrameSynchronizationInfo {
    pub frame_complete_fence: vk::Fence,
    pub frame_complete_semaphore: vk::Semaphore,
    pub consumer_complete_semaphore: vk::Semaphore,
    pub frame_consumer_done_timeline_value: u64,
    pub decode_complete_timeline_value: u64,
    pub filter_complete_timeline_value: u64,
    pub query_pool: vk::QueryPool,
    pub start_query_id: u32,
    pub num_queries: u32,
    pub image_specs_index: ImageSpecsIndex,
    pub has_frame_complete_signal_fence: bool,
    pub has_frame_consumer_signal_semaphore: bool,
    pub has_frame_complete_signal_semaphore: bool,
    pub has_filter_signal_semaphore: bool,
    pub sync_on_frame_complete_fence: bool,
}

impl Default for FrameSynchronizationInfo {
    fn default() -> Self {
        Self {
            frame_complete_fence: vk::Fence::null(),
            frame_complete_semaphore: vk::Semaphore::null(),
            consumer_complete_semaphore: vk::Semaphore::null(),
            frame_consumer_done_timeline_value: 0,
            decode_complete_timeline_value: 0,
            filter_complete_timeline_value: 0,
            query_pool: vk::QueryPool::null(),
            start_query_id: 0,
            num_queries: 0,
            image_specs_index: ImageSpecsIndex::default(),
            has_frame_complete_signal_fence: false,
            has_frame_consumer_signal_semaphore: false,
            has_frame_complete_signal_semaphore: false,
            has_filter_signal_semaphore: false,
            sync_on_frame_complete_fence: false,
        }
    }
}

// ---------------------------------------------------------------------------
// PictureResourceInfo
// ---------------------------------------------------------------------------

/// Mirrors C++ `VulkanVideoFrameBuffer::PictureResourceInfo`.
#[derive(Debug, Clone, Copy)]
pub struct PictureResourceInfo {
    pub image: vk::Image,
    pub image_format: vk::Format,
    pub current_image_layout: vk::ImageLayout,
    pub base_array_layer: u32,
}

impl Default for PictureResourceInfo {
    fn default() -> Self {
        Self {
            image: vk::Image::null(),
            image_format: vk::Format::UNDEFINED,
            current_image_layout: vk::ImageLayout::UNDEFINED,
            base_array_layer: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// ReferencedObjectsInfo
// ---------------------------------------------------------------------------

/// Ref-counted objects that must stay alive while a frame is in the decode
/// pipeline.  Mirrors C++ `VulkanVideoFrameBuffer::ReferencedObjectsInfo`.
///
/// Each field is an `Option<Arc<dyn Any + Send + Sync>>` standing in for
/// the C++ `VkSharedBaseObj<VkVideoRefCountBase>`.  We use a concrete
/// placeholder type until the full ref-count-base hierarchy is ported.
#[derive(Debug, Clone, Default)]
pub struct ReferencedObjectsInfo {
    /// Opaque handle for the bitstream data buffer.
    pub bitstream_data: Option<Arc<dyn std::any::Any + Send + Sync>>,
    /// Opaque handle for the PPS parameter set.
    pub std_pps: Option<Arc<dyn std::any::Any + Send + Sync>>,
    /// Opaque handle for the SPS parameter set.
    pub std_sps: Option<Arc<dyn std::any::Any + Send + Sync>>,
    /// Opaque handle for the VPS parameter set (H.265 only).
    pub std_vps: Option<Arc<dyn std::any::Any + Send + Sync>>,
    /// Opaque handle for the filter pool node.
    pub filter_pool_node: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

// ---------------------------------------------------------------------------
// VulkanVideoDisplayPictureInfo
// ---------------------------------------------------------------------------

/// Information passed when queueing a decoded picture for display.
/// Mirrors C++ `VulkanVideoDisplayPictureInfo`.
#[derive(Debug, Clone, Copy, Default)]
pub struct VulkanVideoDisplayPictureInfo {
    pub timestamp: VkVideotimestamp,
}

// ---------------------------------------------------------------------------
// VkParserDecodePictureInfo — minimal subset used by frame buffer
// ---------------------------------------------------------------------------

/// Decode picture info stored per-frame.
/// Mirrors the fields of C++ `VkParserDecodePictureInfo` that the frame
/// buffer actually references.
#[derive(Debug, Clone, Copy, Default)]
pub struct VkParserDecodePictureInfo {
    pub picture_index: i32,
    pub display_width: u32,
    pub display_height: u32,
    pub image_layer_index: u32,
}

// ---------------------------------------------------------------------------
// VulkanDecodedFrame — output of DequeueDecodedPicture
// ---------------------------------------------------------------------------

/// Image-view pair for one display type.
#[derive(Debug, Clone, Default)]
pub struct DecodedFrameImageView {
    pub view: vk::ImageView,
    pub single_level_view: vk::ImageView,
    pub in_use: bool,
}

/// Maximum image view types per decoded frame — mirrors
/// `VulkanDisplayFrame::IMAGE_VIEW_TYPE_COUNT`.
pub const IMAGE_VIEW_TYPE_COUNT: usize = 2;
/// Index for optimal-tiling display output.
pub const IMAGE_VIEW_TYPE_OPTIMAL_DISPLAY: usize = 0;
/// Index for linear-tiling output.
pub const IMAGE_VIEW_TYPE_LINEAR: usize = 1;

/// A decoded frame ready for display.
/// Mirrors C++ `VulkanDecodedFrame`.
#[derive(Debug, Clone, Default)]
pub struct VulkanDecodedFrame {
    pub picture_index: i32,
    pub image_layer_index: u32,
    pub image_views: [DecodedFrameImageView; IMAGE_VIEW_TYPE_COUNT],
    pub output_image_layout: vk::ImageLayout,
    pub display_width: u32,
    pub display_height: u32,
    pub frame_complete_fence: vk::Fence,
    pub frame_complete_semaphore: vk::Semaphore,
    pub frame_complete_done_sem_value: u64,
    pub consumer_complete_semaphore: vk::Semaphore,
    pub frame_consumer_done_sem_value: u64,
    pub frame_consumer_done_fence: vk::Fence,
    pub num_external_consumers: u32,
    pub external_consumer_done_values: [u64; MAX_EXTERNAL_CONSUMERS],
    pub timestamp: VkVideotimestamp,
    pub decode_order: u64,
    pub display_order: u64,
    pub query_pool: vk::QueryPool,
    pub start_query_id: u32,
    pub num_queries: u32,
}

// ---------------------------------------------------------------------------
// ImageViewState — per-image-type per-frame state
// ---------------------------------------------------------------------------

/// Internal state for a single image-type slot within a frame.
/// Mirrors C++ `NvPerFrameDecodeResources::ImageViewState`.
#[derive(Debug, Clone)]
struct ImageViewState {
    current_layer_layout: vk::ImageLayout,
    view: vk::ImageView,
    single_level_view: vk::ImageView,
    recreate_image: bool,
    _layer_num: u32,
}

impl Default for ImageViewState {
    fn default() -> Self {
        Self {
            current_layer_layout: vk::ImageLayout::UNDEFINED,
            view: vk::ImageView::null(),
            single_level_view: vk::ImageView::null(),
            recreate_image: false,
            _layer_num: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// PerFrameDecodeResources (NvPerFrameDecodeResources)
// ---------------------------------------------------------------------------

/// Per-frame decode resources.  Mirrors C++ `NvPerFrameDecodeResources`
/// which itself inherits from `vkPicBuffBase`.
///
/// The C++ `vkPicBuffBase` is a ref-counted picture-buffer base; here
/// we track availability / decode / display state explicitly.
#[derive(Debug, Clone)]
pub struct PerFrameDecodeResources {
    // --- vkPicBuffBase fields ---
    pub pic_idx: i32,
    pub decode_order: u64,
    pub display_order: u64,
    pub timestamp: VkVideotimestamp,
    ref_count: i32,

    // --- Per-frame state ---
    pub pic_disp_info: VkParserDecodePictureInfo,
    pub frame_complete_fence: vk::Fence,
    pub frame_consumer_done_fence: vk::Fence,
    pub frame_complete_timeline_value: u64,
    pub frame_consumer_done_timeline_value: u64,
    pub external_consumer_done_values: [u64; MAX_EXTERNAL_CONSUMERS],
    pub image_specs_index: ImageSpecsIndex,
    pub has_frame_complete_signal_fence: bool,
    pub has_frame_complete_signal_semaphore: bool,
    pub has_consumer_signal_fence: bool,
    pub use_consumer_signal_semaphore: bool,
    pub in_decode_queue: bool,
    pub in_display_queue: bool,
    pub owned_by_consumer: bool,

    // Ref-counted parameter set / bitstream objects
    pub std_vps: Option<Arc<dyn std::any::Any + Send + Sync>>,
    pub std_sps: Option<Arc<dyn std::any::Any + Send + Sync>>,
    pub std_pps: Option<Arc<dyn std::any::Any + Send + Sync>>,
    pub bitstream_data: Option<Arc<dyn std::any::Any + Send + Sync>>,
    pub filter_pool_node: Option<Arc<dyn std::any::Any + Send + Sync>>,

    // Per-image-type views
    image_view_state: [ImageViewState; MAX_PER_FRAME_IMAGE_TYPES],
}

impl Default for PerFrameDecodeResources {
    fn default() -> Self {
        Self {
            pic_idx: -1,
            decode_order: 0,
            display_order: 0,
            timestamp: 0,
            ref_count: 0,
            pic_disp_info: VkParserDecodePictureInfo::default(),
            frame_complete_fence: vk::Fence::null(),
            frame_consumer_done_fence: vk::Fence::null(),
            frame_complete_timeline_value: 0,
            frame_consumer_done_timeline_value: 0,
            external_consumer_done_values: [0u64; MAX_EXTERNAL_CONSUMERS],
            image_specs_index: ImageSpecsIndex::default(),
            has_frame_complete_signal_fence: false,
            has_frame_complete_signal_semaphore: false,
            has_consumer_signal_fence: false,
            use_consumer_signal_semaphore: false,
            in_decode_queue: false,
            in_display_queue: false,
            owned_by_consumer: false,
            std_vps: None,
            std_sps: None,
            std_pps: None,
            bitstream_data: None,
            filter_pool_node: None,
            image_view_state: Default::default(),
        }
    }
}

impl PerFrameDecodeResources {
    /// Increment the reference count.  Mirrors C++ `AddRef`.
    pub fn add_ref(&mut self) {
        self.ref_count += 1;
    }

    /// Decrement the reference count.  Mirrors C++ `Release`.
    pub fn release(&mut self) {
        self.ref_count -= 1;
    }

    /// Returns `true` when the resource is not in use (ref_count <= 0 and not
    /// in any queue).  Mirrors C++ `IsAvailable`.
    pub fn is_available(&self) -> bool {
        self.ref_count <= 0 && !self.in_decode_queue && !self.in_display_queue && !self.owned_by_consumer
    }

    /// Reset transient state.  Mirrors C++ `Reset`.
    pub fn reset(&mut self) {
        self.pic_idx = -1;
        self.ref_count = 0;
        self.in_decode_queue = false;
        self.in_display_queue = false;
        self.owned_by_consumer = false;
        self.has_frame_complete_signal_fence = false;
        self.has_frame_complete_signal_semaphore = false;
        self.has_consumer_signal_fence = false;
        self.use_consumer_signal_semaphore = false;
    }

    /// Returns `true` when the given image type has a valid view.
    /// Mirrors C++ `ImageExist`.
    pub fn image_exist(&self, image_type_idx: u8) -> bool {
        if image_type_idx == INVALID_IMAGE_TYPE_IDX
            || (image_type_idx as usize) >= MAX_PER_FRAME_IMAGE_TYPES
        {
            return false;
        }
        self.image_view_state[image_type_idx as usize].view != vk::ImageView::null()
    }

    /// Invalidate the tracked image layout for the given type, forcing a
    /// layout transition on next use.  Mirrors C++ `InvalidateImageLayout`.
    pub fn invalidate_image_layout(&mut self, image_type_idx: u8) {
        if (image_type_idx as usize) < MAX_PER_FRAME_IMAGE_TYPES {
            self.image_view_state[image_type_idx as usize].current_layer_layout =
                vk::ImageLayout::UNDEFINED;
        }
    }

    /// Mark the image as needing recreation.  Mirrors C++ `SetRecreateImage`.
    pub fn set_recreate_image(&mut self, image_type_idx: u8) {
        if (image_type_idx as usize) < MAX_PER_FRAME_IMAGE_TYPES {
            self.image_view_state[image_type_idx as usize].recreate_image = true;
        }
    }

    /// Retrieve image view and optionally set a new tracked layout.
    /// Returns `false` if the image needs (re)creation.
    /// Mirrors C++ `GetImageSetNewLayout`.
    pub fn get_image_set_new_layout(
        &mut self,
        image_type_idx: u8,
        new_image_layout: vk::ImageLayout,
        picture_resource_info: Option<&mut PictureResourceInfo>,
    ) -> bool {
        let idx = image_type_idx as usize;
        if idx >= MAX_PER_FRAME_IMAGE_TYPES {
            return false;
        }
        if self.image_view_state[idx].recreate_image || !self.image_exist(image_type_idx) {
            return false;
        }

        if let Some(info) = picture_resource_info {
            // In the full port these would come from the image resource;
            // for now we fill in what we can.
            info.current_image_layout = self.image_view_state[idx].current_layer_layout;
        }

        // VK_IMAGE_LAYOUT_MAX_ENUM = 0x7FFFFFFF — sentinel meaning "don't update".
        if new_image_layout.as_raw() != 0x7FFF_FFFF {
            self.image_view_state[idx].current_layer_layout = new_image_layout;
        }

        true
    }

    /// Get the image view handle for the given type.
    pub fn get_image_view(&self, image_type_idx: u8) -> vk::ImageView {
        if self.image_exist(image_type_idx) {
            self.image_view_state[image_type_idx as usize].view
        } else {
            vk::ImageView::null()
        }
    }

    /// Get the single-level image view handle for the given type.
    pub fn get_single_level_image_view(&self, image_type_idx: u8) -> vk::ImageView {
        if self.image_exist(image_type_idx) {
            self.image_view_state[image_type_idx as usize].single_level_view
        } else {
            vk::ImageView::null()
        }
    }

    /// De-initialise, releasing Vulkan objects.
    /// Mirrors C++ `NvPerFrameDecodeResources::Deinit`.
    pub fn deinit(&mut self, device: Option<&vulkanalia::Device>) {
        self.bitstream_data = None;
        self.std_pps = None;
        self.std_sps = None;
        self.std_vps = None;
        self.filter_pool_node = None;

        if let Some(dev) = device {
            unsafe {
                if self.frame_complete_fence != vk::Fence::null() {
                    dev.destroy_fence(self.frame_complete_fence, None);
                    self.frame_complete_fence = vk::Fence::null();
                }
                if self.frame_consumer_done_fence != vk::Fence::null() {
                    dev.destroy_fence(self.frame_consumer_done_fence, None);
                    self.frame_consumer_done_fence = vk::Fence::null();
                }
            }
        }

        for ivs in &mut self.image_view_state {
            ivs.view = vk::ImageView::null();
            ivs.single_level_view = vk::ImageView::null();
        }

        self.reset();
    }

    /// Initialise fences for this frame.
    /// Mirrors C++ `NvPerFrameDecodeResources::init`.
    pub fn init(&mut self, device: &vulkanalia::Device) -> Result<(), vk::Result> {
        let fence_signaled_info = vk::FenceCreateInfo::builder()
            .flags(vk::FenceCreateFlags::SIGNALED);
        let fence_info = vk::FenceCreateInfo::default();

        unsafe {
            self.frame_complete_fence = device
                .create_fence(&fence_signaled_info, None)
                .map_err(|e| e)?;
            self.frame_consumer_done_fence = device
                .create_fence(&fence_info, None)
                .map_err(|e| e)?;
        }

        self.reset();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PerFrameDecodeImageSet (NvPerFrameDecodeImageSet)
// ---------------------------------------------------------------------------

/// Manages the set of per-frame images and associated synchronisation
/// primitives.  Mirrors C++ `NvPerFrameDecodeImageSet`.
pub struct PerFrameDecodeImageSet {
    queue_family_index: u32,
    pub frame_complete_semaphore: vk::Semaphore,
    pub consumer_complete_semaphore: vk::Semaphore,
    pub external_consumer_semaphores: [vk::Semaphore; MAX_EXTERNAL_CONSUMERS],
    pub external_consumer_types: [SemSyncTypeIdx; MAX_EXTERNAL_CONSUMERS],
    pub num_external_consumers: u32,
    num_images: u32,
    max_num_image_type_idx: u32,
    per_frame: Vec<PerFrameDecodeResources>,
}

impl Default for PerFrameDecodeImageSet {
    fn default() -> Self {
        let mut per_frame = Vec::with_capacity(MAX_IMAGES);
        for _ in 0..MAX_IMAGES {
            per_frame.push(PerFrameDecodeResources::default());
        }
        Self {
            queue_family_index: u32::MAX,
            frame_complete_semaphore: vk::Semaphore::null(),
            consumer_complete_semaphore: vk::Semaphore::null(),
            external_consumer_semaphores: [vk::Semaphore::null(); MAX_EXTERNAL_CONSUMERS],
            external_consumer_types: [SemSyncTypeIdx::Decode; MAX_EXTERNAL_CONSUMERS],
            num_external_consumers: 0,
            num_images: 0,
            max_num_image_type_idx: 0,
            per_frame,
        }
    }
}

impl PerFrameDecodeImageSet {
    pub fn size(&self) -> u32 {
        self.num_images
    }

    /// Access a per-frame resource by index.
    pub fn get(&self, index: u32) -> &PerFrameDecodeResources {
        debug_assert!((index as usize) < self.per_frame.len());
        &self.per_frame[index as usize]
    }

    /// Mutable access to a per-frame resource by index.
    pub fn get_mut(&mut self, index: u32) -> &mut PerFrameDecodeResources {
        debug_assert!((index as usize) < self.per_frame.len());
        &mut self.per_frame[index as usize]
    }

    /// Initialise (or reconfigure) the image set.
    /// Mirrors C++ `NvPerFrameDecodeImageSet::init`.
    pub fn init(
        &mut self,
        device: &vulkanalia::Device,
        num_images: u32,
        _max_num_image_type_idx: u32,
        queue_family_index: u32,
    ) -> i32 {
        if num_images as usize > self.per_frame.len() {
            tracing::error!("Number of requested images exceeds the max size of the image array");
            return -1;
        }

        // Init fences for new frames
        for image_index in self.num_images..num_images {
            if self.per_frame[image_index as usize].init(device).is_err() {
                return -1;
            }
        }

        // Create timeline semaphores if not already present.
        if self.frame_complete_semaphore == vk::Semaphore::null() {
            let timeline_info = vk::SemaphoreTypeCreateInfo::builder()
                .semaphore_type(vk::SemaphoreType::TIMELINE)
                .initial_value(0);
            let _sem_info = vk::SemaphoreCreateInfo::builder().push_next(
                // SAFETY: `timeline_info` lives on the stack and outlives the
                // `create_semaphore` call. We must use a mutable reference via
                // raw pointer workaround since ash builders require &mut.
                // This matches the C++ pattern of stack-allocated pNext chains.
                &mut { timeline_info },
            );
            // Note: the `push_next` pattern requires a mutable borrow which
            // is tricky with ash builders. In practice we build the chain manually:
            let mut timeline_ci = vk::SemaphoreTypeCreateInfo {
                s_type: vk::StructureType::SEMAPHORE_TYPE_CREATE_INFO,
                next: std::ptr::null(),
                semaphore_type: vk::SemaphoreType::TIMELINE,
                initial_value: 0,
            };
            let sem_ci = vk::SemaphoreCreateInfo {
                s_type: vk::StructureType::SEMAPHORE_CREATE_INFO,
                next: &mut timeline_ci as *mut _ as *const _,
                flags: vk::SemaphoreCreateFlags::empty(),
            };

            unsafe {
                match device.create_semaphore(&sem_ci, None) {
                    Ok(s) => self.frame_complete_semaphore = s,
                    Err(_) => return -1,
                }
                match device.create_semaphore(&sem_ci, None) {
                    Ok(s) => self.consumer_complete_semaphore = s,
                    Err(_) => return -1,
                }
            }
        }

        self.queue_family_index = queue_family_index;
        self.num_images = self.num_images.max(num_images);
        self.max_num_image_type_idx = _max_num_image_type_idx;

        num_images as i32
    }

    /// De-initialise the image set, destroying Vulkan objects.
    /// Mirrors C++ `NvPerFrameDecodeImageSet::Deinit`.
    pub fn deinit(&mut self, device: &vulkanalia::Device) {
        unsafe {
            if self.frame_complete_semaphore != vk::Semaphore::null() {
                device.destroy_semaphore(self.frame_complete_semaphore, None);
                self.frame_complete_semaphore = vk::Semaphore::null();
            }
            if self.consumer_complete_semaphore != vk::Semaphore::null() {
                device.destroy_semaphore(self.consumer_complete_semaphore, None);
                self.consumer_complete_semaphore = vk::Semaphore::null();
            }
        }

        for ndx in 0..self.num_images as usize {
            self.per_frame[ndx].deinit(Some(device));
        }

        self.num_images = 0;
    }
}

// ---------------------------------------------------------------------------
// VkVideoFrameBuffer — the concrete implementation
// ---------------------------------------------------------------------------

/// Concrete frame buffer implementation.
/// Mirrors C++ `VkVideoFrameBuffer` (the private impl of `VulkanVideoFrameBuffer`).
pub struct VkVideoFrameBuffer {
    device: vulkanalia::Device,
    display_queue_mutex: Mutex<DisplayQueueState>,
    image_set: PerFrameDecodeImageSet,
    query_pool: vk::QueryPool,
    frame_num_in_display_order: i32,
    number_parameter_updates: u32,
    max_num_image_type_idx: u32,
    debug: bool,
}

/// State protected by the display-queue mutex.
struct DisplayQueueState {
    display_frames: VecDeque<u8>,
    owned_by_display_mask: u32,
}

impl VkVideoFrameBuffer {
    /// Create a new frame buffer.
    /// Mirrors C++ `VulkanVideoFrameBuffer::Create`.
    pub fn new(device: vulkanalia::Device) -> Arc<Self> {
        Arc::new(Self {
            device,
            display_queue_mutex: Mutex::new(DisplayQueueState {
                display_frames: VecDeque::new(),
                owned_by_display_mask: 0,
            }),
            image_set: PerFrameDecodeImageSet::default(),
            query_pool: vk::QueryPool::null(),
            frame_num_in_display_order: 0,
            number_parameter_updates: 0,
            max_num_image_type_idx: 0,
            debug: false,
        })
    }

    /// Create a new frame buffer, returning `VK_SUCCESS` on success.
    /// Mirrors the C++ static `Create` factory method.
    pub fn create(device: vulkanalia::Device) -> Result<Arc<Self>, vk::Result> {
        Ok(Self::new(device))
    }

    // --- Query pool helpers ---

    fn create_video_queries(&mut self, _num_slots: u32) -> vk::Result {
        // Query pool creation requires the video profile; in this port we
        // defer to the caller or skip when query-result-status is unsupported.
        // The C++ code also guards on `GetVideoDecodeQueryResultStatusSupport`.
        vk::Result::SUCCESS
    }

    fn destroy_video_queries(&mut self) {
        if self.query_pool != vk::QueryPool::null() {
            unsafe {
                self.device.destroy_query_pool(self.query_pool, None);
            }
            self.query_pool = vk::QueryPool::null();
        }
    }

    /// Flush the display queue, releasing all pending frames.
    /// Mirrors C++ `FlushDisplayQueue`.
    pub fn flush_display_queue(&mut self) -> u32 {
        let mut state = self.display_queue_mutex.lock().unwrap();
        let mut flushed = 0u32;
        while let Some(pic_idx) = state.display_frames.pop_front() {
            let res = &mut self.image_set.per_frame[pic_idx as usize];
            if res.is_available() {
                res.release();
            }
            flushed += 1;
        }
        flushed
    }

    /// Initialise the image pool.
    /// Mirrors C++ `VkVideoFrameBuffer::InitImagePool`.
    pub fn init_image_pool(
        &mut self,
        num_images: u32,
        max_num_image_type_idx: u32,
        queue_family_index: u32,
        _num_images_to_preallocate: i32,
    ) -> i32 {
        assert!(num_images > 0 && (num_images as usize) <= MAX_IMAGES);

        let result = self.create_video_queries(num_images);
        if result != vk::Result::SUCCESS {
            return 0;
        }

        let image_set_result = self.image_set.init(
            &self.device,
            num_images,
            max_num_image_type_idx,
            queue_family_index,
        );

        if image_set_result >= 0 {
            self.max_num_image_type_idx = max_num_image_type_idx;
        }
        self.number_parameter_updates += 1;

        image_set_result
    }

    /// Queue a decoded picture for display.
    /// Mirrors C++ `QueueDecodedPictureForDisplay`.
    pub fn queue_decoded_picture_for_display(
        &mut self,
        pic_id: i8,
        disp_info: &VulkanVideoDisplayPictureInfo,
    ) -> i32 {
        debug_assert!((pic_id as u32) < self.image_set.size());

        let mut state = self.display_queue_mutex.lock().unwrap();
        let res = &mut self.image_set.per_frame[pic_id as usize];
        res.display_order = self.frame_num_in_display_order as u64;
        self.frame_num_in_display_order += 1;
        res.timestamp = disp_info.timestamp;
        res.in_display_queue = true;
        res.add_ref();

        state.display_frames.push_back(pic_id as u8);

        if self.debug {
            tracing::debug!(
                "Queue Display Picture picIdx: {} displayOrder: {} decodeOrder: {} timestamp: {}",
                pic_id,
                res.display_order,
                res.decode_order,
                res.timestamp,
            );
        }

        pic_id as i32
    }

    /// Queue a picture for decode (producer side).
    /// Mirrors C++ `VkVideoFrameBuffer::QueuePictureForDecode`.
    pub fn queue_picture_for_decode(
        &mut self,
        pic_id: i8,
        decode_picture_info: &VkParserDecodePictureInfo,
        referenced_objects: &ReferencedObjectsInfo,
        sync_info: &mut FrameSynchronizationInfo,
    ) -> i32 {
        debug_assert!((pic_id as u32) < self.image_set.size());
        let idx = pic_id as usize;

        // 1. Producer fence: wait for decode/filter to finish with this slot.
        if sync_info.sync_on_frame_complete_fence {
            let fence = self.image_set.per_frame[idx].frame_complete_fence;
            debug_assert!(fence != vk::Fence::null());
            unsafe {
                let _ = self
                    .device
                    .wait_for_fences(&[fence], true, u64::MAX);
                let _ = self.device.reset_fences(&[fence]);
            }
        }

        // 2. Consumer fence: wait for presentation to finish reading.
        if self.image_set.per_frame[idx].has_consumer_signal_fence {
            let fence = self.image_set.per_frame[idx].frame_consumer_done_fence;
            if fence != vk::Fence::null() {
                unsafe {
                    let _ = self.device.wait_for_fences(&[fence], true, u64::MAX);
                    let _ = self.device.reset_fences(&[fence]);
                }
                self.image_set.per_frame[idx].has_consumer_signal_fence = false;
            }
        }

        // 3. External consumer timeline semaphores.
        for c in 0..self.image_set.num_external_consumers as usize {
            let sem = self.image_set.external_consumer_semaphores[c];
            if sem != vk::Semaphore::null() {
                let wait_value = self.image_set.per_frame[idx].external_consumer_done_values[c];
                if wait_value > 0 {
                    let sems = [sem];
                    let vals = [wait_value];
                    let wait_info = vk::SemaphoreWaitInfo::builder()
                        .semaphores(&sems)
                        .values(&vals);
                    unsafe {
                        let _ = self.device.wait_semaphores(&wait_info, u64::MAX);
                    }
                }
            }
        }

        // Store per-frame data under the mutex.
        {
            let _lock = self.display_queue_mutex.lock().unwrap();
            let res = &mut self.image_set.per_frame[idx];
            res.pic_disp_info = *decode_picture_info;
            res.in_decode_queue = true;
            res.image_specs_index = sync_info.image_specs_index;
            res.std_pps = referenced_objects.std_pps.clone();
            res.std_sps = referenced_objects.std_sps.clone();
            res.std_vps = referenced_objects.std_vps.clone();
            res.bitstream_data = referenced_objects.bitstream_data.clone();
            res.filter_pool_node = referenced_objects.filter_pool_node.clone();

            if sync_info.has_frame_complete_signal_fence {
                sync_info.frame_complete_fence = res.frame_complete_fence;
                if sync_info.frame_complete_fence != vk::Fence::null() {
                    res.has_frame_complete_signal_fence = true;
                }
            }

            if sync_info.has_frame_complete_signal_semaphore {
                sync_info.frame_complete_semaphore = self.image_set.frame_complete_semaphore;
                if sync_info.frame_complete_semaphore != vk::Semaphore::null() {
                    sync_info.decode_complete_timeline_value = get_semaphore_value(
                        SemSyncTypeIdx::Decode,
                        res.decode_order,
                    );

                    if sync_info.has_filter_signal_semaphore {
                        sync_info.filter_complete_timeline_value = get_semaphore_value(
                            SemSyncTypeIdx::Filter,
                            res.decode_order,
                        );
                        res.frame_complete_timeline_value =
                            sync_info.filter_complete_timeline_value;
                    } else {
                        res.frame_complete_timeline_value =
                            sync_info.decode_complete_timeline_value;
                    }

                    res.has_frame_complete_signal_semaphore = true;
                }
            }

            if res.use_consumer_signal_semaphore {
                sync_info.has_frame_consumer_signal_semaphore = true;
                sync_info.consumer_complete_semaphore =
                    self.image_set.consumer_complete_semaphore;
                sync_info.frame_consumer_done_timeline_value =
                    res.frame_consumer_done_timeline_value;
                res.use_consumer_signal_semaphore = false;
            }

            sync_info.query_pool = self.query_pool;
            sync_info.start_query_id = pic_id as u32;
            sync_info.num_queries = 1;
        }

        pic_id as i32
    }

    /// Dequeue a decoded picture for display.
    /// Mirrors C++ `VkVideoFrameBuffer::DequeueDecodedPicture`.
    pub fn dequeue_decoded_picture(&mut self, decoded_frame: &mut VulkanDecodedFrame) -> i32 {
        let mut state = self.display_queue_mutex.lock().unwrap();
        let num_pending = state.display_frames.len() as i32;
        let mut picture_index: i32 = -1;

        if let Some(front) = state.display_frames.pop_front() {
            picture_index = front as i32;
            debug_assert!(
                (picture_index as u32) < self.image_set.size()
            );
            debug_assert!(state.owned_by_display_mask & (1 << picture_index) == 0);
            state.owned_by_display_mask |= 1 << picture_index;

            let res = &mut self.image_set.per_frame[picture_index as usize];
            res.in_display_queue = false;
            res.owned_by_consumer = true;

            decoded_frame.picture_index = picture_index;
            decoded_frame.image_layer_index = res.pic_disp_info.image_layer_index;
            decoded_frame.display_width = res.pic_disp_info.display_width;
            decoded_frame.display_height = res.pic_disp_info.display_height;

            // Frame-complete fence
            if res.has_frame_complete_signal_fence {
                decoded_frame.frame_complete_fence = res.frame_complete_fence;
                res.has_frame_complete_signal_fence = false;
            } else {
                decoded_frame.frame_complete_fence = vk::Fence::null();
            }

            // Frame-complete semaphore
            if res.has_frame_complete_signal_semaphore {
                decoded_frame.frame_complete_semaphore =
                    self.image_set.frame_complete_semaphore;
                decoded_frame.frame_complete_done_sem_value =
                    res.frame_complete_timeline_value;
                res.has_frame_complete_signal_semaphore = false;

                decoded_frame.consumer_complete_semaphore =
                    self.image_set.consumer_complete_semaphore;
                decoded_frame.frame_consumer_done_sem_value = get_semaphore_value(
                    SemSyncTypeIdx::Display,
                    res.display_order,
                );
            } else {
                decoded_frame.frame_complete_semaphore = vk::Semaphore::null();
            }

            decoded_frame.frame_consumer_done_fence = res.frame_consumer_done_fence;

            // External consumer done values
            decoded_frame.num_external_consumers = self.image_set.num_external_consumers;
            for c in 0..self.image_set.num_external_consumers as usize {
                let value = get_semaphore_value(
                    self.image_set.external_consumer_types[c],
                    res.display_order,
                );
                res.external_consumer_done_values[c] = value;
                decoded_frame.external_consumer_done_values[c] = value;
            }

            decoded_frame.timestamp = res.timestamp;
            decoded_frame.decode_order = res.decode_order;
            decoded_frame.display_order = res.display_order;

            decoded_frame.query_pool = self.query_pool;
            decoded_frame.start_query_id = picture_index as u32;
            decoded_frame.num_queries = 1;
        }

        if self.debug {
            tracing::debug!(
                "Dequeue from Display: {} out of {}",
                picture_index,
                num_pending
            );
        }

        num_pending
    }

    /// Release displayed pictures back to the pool.
    /// Mirrors C++ `VkVideoFrameBuffer::ReleaseDisplayedPicture`.
    pub fn release_displayed_picture(
        &mut self,
        releases: &[&DecodedFrameRelease],
    ) -> i32 {
        let mut state = self.display_queue_mutex.lock().unwrap();
        for release in releases {
            let pic_id = release.picture_index as usize;
            debug_assert!((pic_id as u32) < self.image_set.size());

            debug_assert!(state.owned_by_display_mask & (1 << pic_id) != 0);
            state.owned_by_display_mask &= !(1 << pic_id);
            let res = &mut self.image_set.per_frame[pic_id];
            res.in_decode_queue = false;
            res.owned_by_consumer = false;
            res.release();

            res.has_consumer_signal_fence = release.has_consumer_signal_fence;
            res.use_consumer_signal_semaphore = release.has_consumer_signal_semaphore;
            if release.has_consumer_signal_semaphore {
                res.frame_consumer_done_timeline_value = get_semaphore_value(
                    SemSyncTypeIdx::Display,
                    release.display_order,
                );
            }
        }
        0
    }

    /// Set the decode-order number for a picture.
    /// Mirrors C++ `VkVideoFrameBuffer::SetPicNumInDecodeOrder`.
    pub fn set_pic_num_in_decode_order(
        &mut self,
        pic_id: i32,
        pic_num_in_decode_order: u64,
    ) -> u64 {
        let _lock = self.display_queue_mutex.lock().unwrap();
        if (pic_id as u32) < self.image_set.size() {
            let old = self.image_set.per_frame[pic_id as usize].decode_order;
            self.image_set.per_frame[pic_id as usize].decode_order = pic_num_in_decode_order;
            return old;
        }
        debug_assert!(false);
        u64::MAX
    }

    /// Set the display-order number for a picture.
    /// Mirrors C++ `VkVideoFrameBuffer::SetPicNumInDisplayOrder`.
    pub fn set_pic_num_in_display_order(
        &mut self,
        pic_id: i32,
        pic_num_in_display_order: i32,
    ) -> i32 {
        let _lock = self.display_queue_mutex.lock().unwrap();
        if (pic_id as u32) < self.image_set.size() {
            let old = self.image_set.per_frame[pic_id as usize].display_order as i32;
            self.image_set.per_frame[pic_id as usize].display_order =
                pic_num_in_display_order as u64;
            return old;
        }
        debug_assert!(false);
        -1
    }

    /// Reserve a picture buffer (find the least-recently-used available frame).
    /// Mirrors C++ `VkVideoFrameBuffer::ReservePictureBuffer`.
    pub fn reserve_picture_buffer(&mut self) -> Option<usize> {
        let _lock = self.display_queue_mutex.lock().unwrap();
        let mut found_pic_id: i32 = -1;
        let mut min_decode_order: i64 = self.image_set.per_frame[0].decode_order as i64 + 1000;

        for pic_id in 0..self.image_set.size() {
            if self.image_set.per_frame[pic_id as usize].is_available() {
                let order = self.image_set.per_frame[pic_id as usize].decode_order as i64;
                if order < min_decode_order {
                    found_pic_id = pic_id as i32;
                    min_decode_order = order;
                }
            }
        }

        if found_pic_id >= 0 {
            let idx = found_pic_id as usize;
            self.image_set.per_frame[idx].reset();
            self.image_set.per_frame[idx].add_ref();
            self.image_set.per_frame[idx].pic_idx = found_pic_id;
            Some(idx)
        } else {
            debug_assert!(false, "No available picture buffer found");
            None
        }
    }

    /// Get the current number of queue slots.
    /// Mirrors C++ `VkVideoFrameBuffer::GetCurrentNumberQueueSlots`.
    pub fn get_current_number_queue_slots(&self) -> u32 {
        self.image_set.size()
    }

    /// Register an external consumer's release semaphore.
    /// Returns consumer index or -1 on failure.
    /// Mirrors C++ `VkVideoFrameBuffer::AddExternalConsumer`.
    pub fn add_external_consumer(
        &mut self,
        imported_release_semaphore: vk::Semaphore,
        consumer_type: SemSyncTypeIdx,
    ) -> i32 {
        let idx = self.image_set.num_external_consumers as usize;
        if idx >= MAX_EXTERNAL_CONSUMERS {
            return -1;
        }
        self.image_set.external_consumer_semaphores[idx] = imported_release_semaphore;
        self.image_set.external_consumer_types[idx] = consumer_type;
        self.image_set.num_external_consumers = (idx + 1) as u32;
        idx as i32
    }

    /// Get the consumer-complete semaphore.
    /// Mirrors C++ `VkVideoFrameBuffer::GetConsumerCompleteSemaphore`.
    pub fn get_consumer_complete_semaphore(&self) -> vk::Semaphore {
        self.image_set.consumer_complete_semaphore
    }

    /// De-initialise the entire frame buffer.
    /// Mirrors C++ `VkVideoFrameBuffer::Deinitialize`.
    pub fn deinitialize(&mut self) {
        self.flush_display_queue();
        self.destroy_video_queries();
        {
            let mut state = self.display_queue_mutex.lock().unwrap();
            state.owned_by_display_mask = 0;
        }
        self.frame_num_in_display_order = 0;
        self.image_set.deinit(&self.device);
    }
}

impl Drop for VkVideoFrameBuffer {
    fn drop(&mut self) {
        self.deinitialize();
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_semaphore_value_encoding() {
        // Verify that different sync types produce distinct upper nibbles.
        let v_decode = get_semaphore_value(SemSyncTypeIdx::Decode, 42);
        let v_filter = get_semaphore_value(SemSyncTypeIdx::Filter, 42);
        let v_display = get_semaphore_value(SemSyncTypeIdx::Display, 42);

        assert_ne!(v_decode, v_filter);
        assert_ne!(v_filter, v_display);

        // Lower 60 bits should encode the frame order.
        assert_eq!(v_decode & 0x0FFF_FFFF_FFFF_FFFF, 42);
        assert_eq!(v_filter & 0x0FFF_FFFF_FFFF_FFFF, 42);
        assert_eq!(v_display & 0x0FFF_FFFF_FFFF_FFFF, 42);

        // Same type + order should always produce the same value.
        assert_eq!(
            get_semaphore_value(SemSyncTypeIdx::Decode, 100),
            get_semaphore_value(SemSyncTypeIdx::Decode, 100),
        );
    }

    #[test]
    fn test_decoded_frame_release_default() {
        let r = DecodedFrameRelease::default();
        assert_eq!(r.picture_index, 0);
        assert!(!r.has_consumer_signal_fence);
        assert!(!r.has_consumer_signal_semaphore);
    }

    #[test]
    fn test_per_frame_decode_resources_lifecycle() {
        let mut res = PerFrameDecodeResources::default();
        assert!(res.is_available());

        res.add_ref();
        assert!(!res.is_available());

        res.release();
        assert!(res.is_available());
    }

    #[test]
    fn test_per_frame_decode_resources_reset() {
        let mut res = PerFrameDecodeResources::default();
        res.pic_idx = 5;
        res.in_decode_queue = true;
        res.add_ref();

        res.reset();
        assert_eq!(res.pic_idx, -1);
        assert!(!res.in_decode_queue);
        assert!(res.is_available());
    }

    #[test]
    fn test_image_exist_invalid() {
        let res = PerFrameDecodeResources::default();
        assert!(!res.image_exist(INVALID_IMAGE_TYPE_IDX));
        assert!(!res.image_exist(0));
        assert!(!res.image_exist(MAX_PER_FRAME_IMAGE_TYPES as u8));
    }

    #[test]
    fn test_frame_synchronization_info_default() {
        let info = FrameSynchronizationInfo::default();
        assert_eq!(info.frame_complete_fence, vk::Fence::null());
        assert!(!info.has_frame_complete_signal_fence);
        assert!(!info.sync_on_frame_complete_fence);
    }

    #[test]
    fn test_picture_resource_info_default() {
        let info = PictureResourceInfo::default();
        assert_eq!(info.image, vk::Image::null());
        assert_eq!(info.image_format, vk::Format::UNDEFINED);
        assert_eq!(info.current_image_layout, vk::ImageLayout::UNDEFINED);
    }

    #[test]
    fn test_init_type_values() {
        assert_eq!(InitType::InvalidateImagesLayout as u8, 0);
        assert_eq!(InitType::RecreateImages as u8, 2);
        assert_eq!(InitType::IncreaseNumSlots as u8, 4);
    }

    #[test]
    fn test_per_frame_image_set_default() {
        let set = PerFrameDecodeImageSet::default();
        assert_eq!(set.size(), 0);
        assert_eq!(set.frame_complete_semaphore, vk::Semaphore::null());
        assert_eq!(set.consumer_complete_semaphore, vk::Semaphore::null());
        assert_eq!(set.num_external_consumers, 0);
    }

    #[test]
    fn test_get_image_set_new_layout_needs_recreation() {
        let mut res = PerFrameDecodeResources::default();
        // No images exist, so should return false (needs creation).
        assert!(!res.get_image_set_new_layout(0, vk::ImageLayout::GENERAL, None));
    }

    #[test]
    fn test_invalidate_image_layout() {
        let mut res = PerFrameDecodeResources::default();
        res.invalidate_image_layout(0);
        // Verify we don't panic on valid index.
        res.invalidate_image_layout(MAX_PER_FRAME_IMAGE_TYPES as u8 - 1);
    }

    #[test]
    fn test_vulkan_decoded_frame_default() {
        let frame = VulkanDecodedFrame::default();
        assert_eq!(frame.picture_index, 0);
        assert_eq!(frame.frame_complete_fence, vk::Fence::null());
        assert_eq!(frame.num_external_consumers, 0);
    }

    #[test]
    fn test_sem_sync_type_idx_default() {
        let s = SemSyncTypeIdx::default();
        assert_eq!(s, SemSyncTypeIdx::Decode);
    }
}
