// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Swapchain + window-surface orchestrator for the host RHI.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::ExtHdrMetadataExtensionDeviceCommands as _;
use vulkanalia::vk::KhrSurfaceExtensionInstanceCommands as _;
use vulkanalia::vk::KhrSwapchainExtensionDeviceCommands as _;

use crate::core::color::{ColorTraits, HdrStaticMetadata};
use crate::core::rhi::TextureFormat;
use crate::core::{Error, Result};

use super::HostVulkanDevice;
use super::vulkan_command_recorder::RhiCommandRecorder;
use super::vulkan_pipeline_flags::VulkanStage;
use super::vulkan_swapchain_colorspace::{
    SwapchainColorPick, build_hdr_metadata, pick_swapchain_format,
};
use super::vulkan_sync::HostVulkanTimelineSemaphore;

/// Maximum CPU/GPU frames in flight at once.
///
/// Per-frame resources (acquire semaphores, recorders, descriptor-set
/// slots) are sized to this constant — independent of swapchain image
/// count. See [`docs/learnings/vulkan-frames-in-flight.md`] for the
/// per-image-vs-per-frame distinction.
pub const MAX_FRAMES_IN_FLIGHT: usize = 2;

/// Vulkan presentation orchestrator: owns a `VkSurfaceKHR` +
/// `VkSwapchainKHR` bound to a windowing surface, per-swapchain-image
/// binary semaphores (present-wait), per-frame-in-flight acquire
/// semaphores + command recorders, and a timeline semaphore that
/// gates slot reuse.
pub struct VulkanPresentTarget {
    device: Arc<HostVulkanDevice>,
    surface: vk::SurfaceKHR,
    swapchain: vk::SwapchainKHR,
    swapchain_images: Vec<vk::Image>,
    swapchain_image_views: Vec<vk::ImageView>,
    swapchain_format: vk::Format,
    swapchain_extent: vk::Extent2D,
    color_format: TextureFormat,
    /// Negotiated `(format, color_space, is_hdr)` from the surface's
    /// exposed format list and the most recent `ColorInfo` hint. Read
    /// by [`Self::set_hdr_metadata`] to short-circuit when the current
    /// colorspace is not HDR-signaling.
    color_pick: SwapchainColorPick,
    /// Cached most-recent `vk::HdrMetadataEXT` payload pushed to the
    /// driver via `vkSetHdrMetadataEXT`. Carried so consecutive
    /// identical metadata pushes (every-frame call sites) compress to
    /// zero driver round-trips. `None` until the first successful
    /// push.
    last_hdr_metadata: Option<vk::HdrMetadataEXT>,
    vsync: bool,

    /// Per-swapchain-image binary semaphore signaled when rendering for
    /// that image completes; waited on by `vkQueuePresentKHR`. Must be
    /// keyed by `image_index` (not `frame_index`) to avoid
    /// `VUID-vkQueueSubmit2-semaphore-03868` — see
    /// [`docs/learnings/vulkan-frames-in-flight.md`].
    render_finished_semaphores: Vec<vk::Semaphore>,

    /// Per-frame-in-flight binary semaphore signaled by
    /// `vkAcquireNextImageKHR`; waited on at `COLOR_ATTACHMENT_OUTPUT`
    /// before the render submit writes to the swapchain image.
    image_available_semaphores: Vec<vk::Semaphore>,

    /// Per-frame-in-flight command recorder. Each frame slot owns its
    /// own command pool + buffer + completion fence.
    recorders: Vec<RhiCommandRecorder>,

    /// Timeline semaphore that the render submit signals at
    /// `frame_timeline_value`; the next call to `render_frame` waits
    /// for `value - MAX_FRAMES_IN_FLIGHT` before reusing slot N.
    frame_timeline: HostVulkanTimelineSemaphore,
    frame_timeline_value: u64,

    current_frame: usize,

    /// State stashed between [`Self::begin_frame`] and
    /// [`Self::end_frame`] for the frame currently being recorded. `None`
    /// between an `end_frame` and the next `begin_frame` — the state
    /// machine that lets the plugin-ABI `begin_frame`/`end_frame` split
    /// (which cannot pass a Rust closure across the boundary) detect a
    /// misordered call (`begin_frame` while a frame is already in flight,
    /// or `end_frame` with none) as a typed error instead of UB.
    in_flight: Option<InFlightFrame>,
}

/// The per-frame handles [`VulkanPresentTarget::end_frame`] needs, stashed
/// by [`VulkanPresentTarget::begin_frame`] after acquiring the swapchain
/// image. Kept host-side and opaque to any plugin-ABI caller — all
/// per-image render-finished-semaphore keying (VUID-vkQueueSubmit2-
/// semaphore-03868) stays inside the host across the begin/end split.
struct InFlightFrame {
    frame_index: usize,
    image_index: u32,
    swapchain_image: vk::Image,
    render_finished_semaphore: vk::Semaphore,
    image_available_semaphore: vk::Semaphore,
    timeline_signal_value: u64,
}

/// Outcome of [`VulkanPresentTarget::begin_frame`]: the acquired
/// swapchain image + frame-slot state the caller records draws against.
/// The recorder is left borrowed on the target (`self.recorders[frame_index]`,
/// already `begin()`'d and pre-barriered) — the caller drives it and then
/// calls [`VulkanPresentTarget::end_frame`].
///
/// Pure-POD (raw `VkImage` / `VkImageView` widened to `u64`) so the
/// plugin-ABI slot body can populate `PresentFrameBeginRepr` without
/// touching `vulkanalia` — the RHI boundary keeps every `vk::*` type
/// inside this crate.
pub struct AcquiredFrame {
    /// Frame-in-flight slot index ∈ `[0, MAX_FRAMES_IN_FLIGHT)`.
    pub frame_index: u32,
    /// Acquired swapchain image index (host-internal ordering).
    pub image_index: u32,
    /// Acquired swapchain `VkImage` (widened to `u64`).
    pub image_raw: u64,
    /// `VkImageView` feeding `cmd_begin_dynamic_rendering` (widened to `u64`).
    pub image_view_raw: u64,
    /// Current swapchain extent `(width, height)`.
    pub extent: (u32, u32),
    /// Swapchain image color format (kernel `attachment_formats` must match).
    pub color_format: TextureFormat,
}

/// Active-frame handle passed to the [`VulkanPresentTarget::render_frame`]
/// closure. Carries everything the caller needs to record draws into the
/// acquired swapchain image.
pub struct PresentFrame<'a> {
    /// Frame-in-flight slot index ∈ `[0, MAX_FRAMES_IN_FLIGHT)`. Use as
    /// the descriptor-ring slot for `VulkanGraphicsKernel::set_*` and
    /// `record_draw`.
    pub frame_index: u32,
    /// Acquired swapchain image index. Internal to the present target;
    /// callers don't typically need it.
    pub image_index: u32,
    /// Current swapchain extent.
    pub extent: (u32, u32),
    /// Color format of the swapchain images. The
    /// `VulkanGraphicsKernel`'s `attachment_formats` must match.
    pub color_format: TextureFormat,
    /// Command recorder for this frame slot. Already `begin()`'d. The
    /// closure records draws + barriers here.
    pub recorder: &'a mut RhiCommandRecorder,
    inner: PresentFrameInner,
}

struct PresentFrameInner {
    image_view: vk::ImageView,
    extra_waits: Vec<vk::SemaphoreSubmitInfo>,
    in_render_pass: bool,
}

impl VulkanPresentTarget {
    /// Build a present target bound to `window` at the requested initial
    /// extent + vsync preference. `color_traits` drives the
    /// `VkColorSpaceKHR` priority walk; `None` keeps the legacy SDR
    /// pick (`B8G8R8A8_UNORM` + `SRGB_NONLINEAR`). Consumers translate
    /// their schema `ColorInfo` via
    /// [`crate::core::color::color_traits_from_color_info`] at the call
    /// site. The window handle must outlive the present target;
    /// dropping the target destroys the surface + swapchain +
    /// per-frame resources.
    #[tracing::instrument(
        level = "trace",
        skip(device, window, color_traits),
        fields(width, height, vsync)
    )]
    pub fn new(
        device: &Arc<HostVulkanDevice>,
        window: &(impl HasWindowHandle + HasDisplayHandle),
        width: u32,
        height: u32,
        vsync: bool,
        color_traits: Option<&ColorTraits>,
    ) -> Result<Self> {
        let instance = device.instance();
        let surface = unsafe { vulkanalia::window::create_surface(instance, window, window) }
            .map_err(|e| {
                Error::DisplaySurfaceUnavailable(format!(
                    "VulkanPresentTarget: create_surface failed: {e}"
                ))
            })?;

        let physical_device = device.physical_device();
        let queue_family_index = device.queue_family_index();
        let surface_supported = unsafe {
            instance.get_physical_device_surface_support_khr(
                physical_device,
                queue_family_index,
                surface,
            )
        }
        .map_err(|e| {
            Error::GpuError(format!(
                "VulkanPresentTarget: get_physical_device_surface_support_khr: {e}"
            ))
        })?;
        if !surface_supported {
            unsafe { instance.destroy_surface_khr(surface, None) };
            return Err(Error::DisplaySurfaceUnavailable(
                "VulkanPresentTarget: graphics queue family does not support presentation".into(),
            ));
        }

        let (
            swapchain,
            swapchain_images,
            swapchain_image_views,
            swapchain_format,
            swapchain_extent,
            color_pick,
        ) = create_swapchain(
            device,
            surface,
            width,
            height,
            vsync,
            color_traits,
            vk::SwapchainKHR::null(),
        )?;

        let color_format = vk_format_to_texture_format(swapchain_format).ok_or_else(|| {
            Error::GpuError(format!(
                "VulkanPresentTarget: swapchain format {swapchain_format:?} not mapped to TextureFormat"
            ))
        })?;

        let semaphore_info = vk::SemaphoreCreateInfo::builder().build();
        let raw_device = device.device();

        let mut render_finished_semaphores = Vec::with_capacity(swapchain_images.len());
        for _ in 0..swapchain_images.len() {
            let sem =
                unsafe { raw_device.create_semaphore(&semaphore_info, None) }.map_err(|e| {
                    Error::GpuError(format!(
                        "VulkanPresentTarget: render-finished semaphore: {e}"
                    ))
                })?;
            render_finished_semaphores.push(sem);
        }

        let mut image_available_semaphores = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            let sem =
                unsafe { raw_device.create_semaphore(&semaphore_info, None) }.map_err(|e| {
                    Error::GpuError(format!(
                        "VulkanPresentTarget: image-available semaphore: {e}"
                    ))
                })?;
            image_available_semaphores.push(sem);
        }

        let frame_timeline = HostVulkanTimelineSemaphore::new(raw_device, 0).map_err(|e| {
            Error::GpuError(format!(
                "VulkanPresentTarget: frame timeline semaphore: {e}"
            ))
        })?;

        let mut recorders = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for i in 0..MAX_FRAMES_IN_FLIGHT {
            let label = format!("present-target-frame-{i}");
            let rec = RhiCommandRecorder::new(device, &label)?;
            recorders.push(rec);
        }

        Ok(Self {
            device: Arc::clone(device),
            surface,
            swapchain,
            swapchain_images,
            swapchain_image_views,
            swapchain_format,
            swapchain_extent,
            color_format,
            color_pick,
            last_hdr_metadata: None,
            vsync,
            render_finished_semaphores,
            image_available_semaphores,
            recorders,
            frame_timeline,
            frame_timeline_value: 0,
            current_frame: 0,
            in_flight: None,
        })
    }

    /// Recreate the swapchain for a new window extent (driven by the
    /// caller's resize handling or an `OUT_OF_DATE_KHR` return from
    /// [`Self::render_frame`]) or new color traits (driven by
    /// first-frame inspection or mid-stream color change). Passing
    /// `None` for `color_traits` keeps the legacy SDR pick.
    #[tracing::instrument(level = "trace", skip(self, color_traits), fields(width, height))]
    pub fn recreate(
        &mut self,
        width: u32,
        height: u32,
        color_traits: Option<&ColorTraits>,
    ) -> Result<()> {
        // Drain in-flight work before destroying the old swapchain.
        // `wait_idle()` takes the queue mutexes so it doesn't race with
        // an active submit on another thread.
        self.device.wait_idle().map_err(|e| {
            Error::GpuError(format!("VulkanPresentTarget::recreate: wait_idle: {e}"))
        })?;

        let raw_device = self.device.device();
        let (new_swapchain, new_images, new_views, new_format, new_extent, new_color_pick) =
            create_swapchain(
                &self.device,
                self.surface,
                width,
                height,
                self.vsync,
                color_traits,
                self.swapchain,
            )?;

        // Destroy old swapchain resources after successful recreate.
        unsafe {
            for &view in &self.swapchain_image_views {
                raw_device.destroy_image_view(view, None);
            }
            raw_device.destroy_swapchain_khr(self.swapchain, None);
        }

        // Resize render-finished semaphores to new image count.
        if new_images.len() != self.render_finished_semaphores.len() {
            unsafe {
                for &sem in &self.render_finished_semaphores {
                    raw_device.destroy_semaphore(sem, None);
                }
            }
            self.render_finished_semaphores.clear();
            let semaphore_info = vk::SemaphoreCreateInfo::builder().build();
            for _ in 0..new_images.len() {
                let sem =
                    unsafe { raw_device.create_semaphore(&semaphore_info, None) }.map_err(|e| {
                        Error::GpuError(format!(
                            "VulkanPresentTarget::recreate: render-finished semaphore: {e}"
                        ))
                    })?;
                self.render_finished_semaphores.push(sem);
            }
        }

        // Refresh the cached `TextureFormat` view alongside the raw
        // `vk::Format` — a recreate that lands on a new format
        // (e.g. SDR `BGRA8_UNORM` → HDR10 `R16G16B16A16_SFLOAT`)
        // must update both fields, otherwise downstream code
        // reading [`Self::color_format`] sees a stale value and
        // [`build_display_kernel`] (called by display.rs after a
        // colorspace transition) silently rebuilds against the old
        // attachment format.
        let new_color_format = vk_format_to_texture_format(new_format).ok_or_else(|| {
            Error::GpuError(format!(
                "VulkanPresentTarget::recreate: swapchain format {new_format:?} \
                 not mapped to TextureFormat"
            ))
        })?;

        self.swapchain = new_swapchain;
        self.swapchain_images = new_images;
        self.swapchain_image_views = new_views;
        self.swapchain_format = new_format;
        self.color_format = new_color_format;
        self.swapchain_extent = new_extent;
        // Update the cached colorspace pick. If the colorspace changed
        // (SDR → HDR or vice versa, or one HDR variant → another),
        // any previously-pushed `vkSetHdrMetadataEXT` payload is no
        // longer valid for the new swapchain — clear the cache so the
        // next `set_hdr_metadata` call re-pushes.
        if new_color_pick != self.color_pick {
            self.last_hdr_metadata = None;
        }
        self.color_pick = new_color_pick;
        // current_frame stays — slots are independent of swapchain count.
        Ok(())
    }

    /// Swapchain image color format.
    pub fn color_format(&self) -> TextureFormat {
        self.color_format
    }

    /// Current swapchain extent (width, height).
    pub fn current_extent(&self) -> (u32, u32) {
        (self.swapchain_extent.width, self.swapchain_extent.height)
    }

    /// Last-negotiated `(format, colorspace, is_hdr)` triple. The
    /// display processor uses this to decide whether to push HDR
    /// metadata via [`Self::set_hdr_metadata`].
    pub fn color_pick(&self) -> SwapchainColorPick {
        self.color_pick
    }

    /// Push HDR static metadata to the driver via
    /// `vkSetHdrMetadataEXT`. No-op when the current swapchain
    /// colorspace is not one of the HDR signaling variants
    /// (PQ / HLG), or when `VK_EXT_hdr_metadata` was not enabled at
    /// device construction. Subsequent calls with byte-identical
    /// metadata short-circuit to avoid redundant driver round-trips.
    pub fn set_hdr_metadata(&mut self, metadata: &HdrStaticMetadata) -> Result<()> {
        if !self.color_pick.is_hdr {
            return Ok(());
        }
        if !self.device.supports_hdr_metadata() {
            tracing::debug!(
                "VulkanPresentTarget::set_hdr_metadata: skipped — \
                 VK_EXT_hdr_metadata not enabled at device construction"
            );
            return Ok(());
        }
        let metadata = build_hdr_metadata(metadata);
        if hdr_metadata_eq(self.last_hdr_metadata.as_ref(), &metadata) {
            return Ok(());
        }
        let swapchains = [self.swapchain];
        let metadatas = [metadata];
        // Safety: device is alive (we hold an Arc); swapchain is the
        // currently-owned handle; metadata array is stack-allocated
        // and lives across the call. The driver is required to read
        // through the pointers before returning per spec.
        unsafe {
            self.device
                .device()
                .set_hdr_metadata_ext(&swapchains, &metadatas)
        };
        self.last_hdr_metadata = Some(metadata);
        Ok(())
    }

    /// Acquire the next swapchain image, wait for slot reuse, then
    /// `begin()` + pre-barrier (UNDEFINED → COLOR_ATTACHMENT_OPTIMAL) the
    /// frame slot's recorder and stash the per-frame state for the
    /// matching [`Self::end_frame`]. Returns `Ok(None)` if the swapchain
    /// returned `OUT_OF_DATE_KHR` during acquire (the timeline bump is
    /// rolled back so the next attempt's wait math stays consistent, and
    /// the caller must drive [`Self::recreate`] and NOT call `end_frame`).
    ///
    /// The recorder is left borrowed on the target — reach it via
    /// `self.recorders[frame_index]` (host-Rust) or the raw handle from
    /// [`Self::in_flight_recorder_handle`] (plugin ABI). This is the
    /// non-closure half of the former `render_frame` loop; splitting it
    /// lets the plugin ABI drive the frame without passing a Rust closure
    /// across the boundary. Blocks on the slot-reuse timeline wait and the
    /// `u64::MAX`-timeout acquire.
    #[tracing::instrument(level = "trace", skip(self), fields(frame_index = self.current_frame))]
    pub fn begin_frame(&mut self) -> Result<Option<AcquiredFrame>> {
        if self.in_flight.is_some() {
            return Err(Error::GpuError(
                "VulkanPresentTarget::begin_frame: a frame is already in flight — \
                 end_frame must be called before the next begin_frame"
                    .into(),
            ));
        }

        let frame_index = self.current_frame;
        let raw_device = self.device.device();

        // Slot reuse: wait until frame N-MAX_FRAMES_IN_FLIGHT signaled the timeline.
        self.frame_timeline_value += 1;
        let wait_value = self
            .frame_timeline_value
            .saturating_sub(MAX_FRAMES_IN_FLIGHT as u64);
        if wait_value > 0 {
            let semaphores = [self.frame_timeline.semaphore()];
            let values = [wait_value];
            let wait_info = vk::SemaphoreWaitInfo::builder()
                .semaphores(&semaphores)
                .values(&values)
                .build();
            unsafe {
                raw_device
                    .wait_semaphores(&wait_info, u64::MAX)
                    .map_err(|e| {
                        Error::GpuError(format!(
                            "VulkanPresentTarget::begin_frame: wait_semaphores (slot reuse): {e}"
                        ))
                    })?;
            }
        }

        let image_available_semaphore = self.image_available_semaphores[frame_index];
        let image_index = match unsafe {
            raw_device.acquire_next_image_khr(
                self.swapchain,
                u64::MAX,
                image_available_semaphore,
                vk::Fence::null(),
            )
        } {
            Ok((index, _)) => index,
            Err(vk::ErrorCode::OUT_OF_DATE_KHR) => {
                // Caller will drive recreate(). Roll back the timeline
                // bump so the next attempt's wait math stays consistent.
                // No recorder was begun, so nothing else to unwind.
                self.frame_timeline_value = self.frame_timeline_value.saturating_sub(1);
                return Ok(None);
            }
            Err(e) => {
                self.frame_timeline_value = self.frame_timeline_value.saturating_sub(1);
                return Err(Error::GpuError(format!(
                    "VulkanPresentTarget::begin_frame: acquire_next_image_khr: {e}"
                )));
            }
        };

        let swapchain_image = self.swapchain_images[image_index as usize];
        let image_view = self.swapchain_image_views[image_index as usize];
        let render_finished_semaphore = self.render_finished_semaphores[image_index as usize];
        let extent = self.swapchain_extent;
        let timeline_signal_value = self.frame_timeline_value;
        let color_format = self.color_format;

        let recorder = &mut self.recorders[frame_index];
        recorder.begin()?;

        // Pre-draw barrier: swapchain image UNDEFINED → COLOR_ATTACHMENT_OPTIMAL.
        // UNDEFINED is valid on every reuse because the render pass uses
        // CLEAR load op (set by `PresentFrame::begin_rendering` /
        // `cmd_begin_dynamic_rendering` with a clear color).
        recorder.record_swapchain_image_barrier(
            swapchain_image,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::PipelineStageFlags2::NONE,
            vk::AccessFlags2::NONE,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
        )?;

        self.in_flight = Some(InFlightFrame {
            frame_index,
            image_index,
            swapchain_image,
            render_finished_semaphore,
            image_available_semaphore,
            timeline_signal_value,
        });

        Ok(Some(AcquiredFrame {
            frame_index: frame_index as u32,
            image_index,
            image_raw: swapchain_image.as_raw(),
            image_view_raw: image_view.as_raw(),
            extent: (extent.width, extent.height),
            color_format,
        }))
    }

    /// Complete the frame acquired by [`Self::begin_frame`]: post-draw
    /// barrier (COLOR_ATTACHMENT_OPTIMAL → PRESENT_SRC_KHR), submit (wait
    /// image-available + `extra_waits`; signal render-finished +
    /// frame-timeline), and `vkQueuePresentKHR`. Returns `Ok(false)` when
    /// present returned `OUT_OF_DATE_KHR` (caller should drive
    /// [`Self::recreate`]).
    ///
    /// Always runs even when the caller's own draw calls failed —
    /// dropping the acquired image without presenting would leave it
    /// app-owned indefinitely and the next `u64::MAX`-timeout acquire
    /// would trip `VUID-vkAcquireNextImageKHR-surface-07783`
    /// (forward-progress). `extra_waits` fold any producer-finished
    /// timeline waits into the submit wait list. Returns a typed error
    /// (never UB) when no frame is in flight (misordered begin/end).
    #[tracing::instrument(level = "trace", skip(self, extra_waits))]
    pub fn end_frame(&mut self, extra_waits: &[vk::SemaphoreSubmitInfo]) -> Result<bool> {
        let in_flight = self.in_flight.take().ok_or_else(|| {
            Error::GpuError(
                "VulkanPresentTarget::end_frame: no frame in flight — \
                 begin_frame must return an acquired frame first"
                    .into(),
            )
        })?;

        let queue = self.device.queue();
        let frame_index = in_flight.frame_index;
        let swapchain_image = in_flight.swapchain_image;
        let image_available_semaphore = in_flight.image_available_semaphore;
        let render_finished_semaphore = in_flight.render_finished_semaphore;
        let timeline_semaphore = self.frame_timeline.semaphore();
        let timeline_signal_value = in_flight.timeline_signal_value;

        let recorder = &mut self.recorders[frame_index];
        // Post-draw barrier: swapchain image COLOR_ATTACHMENT_OPTIMAL → PRESENT_SRC_KHR.
        recorder.record_swapchain_image_barrier(
            swapchain_image,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::PRESENT_SRC_KHR,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags2::NONE,
            vk::AccessFlags2::NONE,
        )?;

        // Submit: wait on image_available (binary, COLOR_ATTACHMENT_OUTPUT)
        // + any caller-added timeline waits; signal render_finished (binary,
        // ALL_COMMANDS) + frame timeline.
        let mut wait_infos: Vec<vk::SemaphoreSubmitInfo> =
            Vec::with_capacity(1 + extra_waits.len());
        wait_infos.push(
            vk::SemaphoreSubmitInfo::builder()
                .semaphore(image_available_semaphore)
                .stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
                .build(),
        );
        wait_infos.extend_from_slice(extra_waits);

        let signal_infos = [
            vk::SemaphoreSubmitInfo::builder()
                .semaphore(render_finished_semaphore)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .build(),
            vk::SemaphoreSubmitInfo::builder()
                .semaphore(timeline_semaphore)
                .value(timeline_signal_value)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .build(),
        ];

        recorder.submit_with_semaphores(&wait_infos, &signal_infos)?;

        // Present.
        let present_wait_semaphores = [render_finished_semaphore];
        let swapchains = [self.swapchain];
        let image_indices = [in_flight.image_index];
        let present_info = vk::PresentInfoKHR::builder()
            .wait_semaphores(&present_wait_semaphores)
            .swapchains(&swapchains)
            .image_indices(&image_indices)
            .build();

        let present_result = unsafe { self.device.present_to_queue(queue, &present_info) };
        let out_of_date = match present_result {
            Ok(_) => false,
            Err(vk::ErrorCode::OUT_OF_DATE_KHR) => true,
            Err(e) => {
                self.current_frame = (frame_index + 1) % MAX_FRAMES_IN_FLIGHT;
                return Err(Error::GpuError(format!(
                    "VulkanPresentTarget::end_frame: queue_present: {e}"
                )));
            }
        };

        self.current_frame = (frame_index + 1) % MAX_FRAMES_IN_FLIGHT;
        Ok(!out_of_date)
    }

    /// Wire-format companion to [`Self::end_frame`]: materializes the
    /// `vk::SemaphoreSubmitInfo` extra-waits from their `#[repr(C)]`
    /// [`streamlib_plugin_abi::SemaphoreSubmitInfoRepr`] projections
    /// host-side (the RHI boundary keeps `vulkanalia` inside this crate),
    /// then dispatches [`Self::end_frame`]. The plugin-ABI `end_frame`
    /// slot body calls this so it never names a `vk::*` type.
    pub fn end_frame_from_wire(
        &mut self,
        extra_waits: &[streamlib_plugin_abi::SemaphoreSubmitInfoRepr],
    ) -> Result<bool> {
        let vk_waits: Vec<vk::SemaphoreSubmitInfo> = extra_waits
            .iter()
            .map(|r| {
                vk::SemaphoreSubmitInfo::builder()
                    .semaphore(vk::Semaphore::from_raw(r.semaphore))
                    .value(r.value)
                    .stage_mask(vk::PipelineStageFlags2::from_bits_truncate(r.stage_mask))
                    .device_index(r.device_index)
                    .build()
            })
            .collect();
        self.end_frame(&vk_waits)
    }

    /// Raw `Box<RhiCommandRecorderInner>` pointer of the in-flight frame's
    /// recorder, or null when no frame is in flight. Borrowed, NON-OWNING
    /// — the present target owns the recorder across the begin/end split;
    /// a caller must never release it. Used by the plugin-ABI `begin_frame`
    /// return (recorder handle) + `end_frame` recorder-identity check.
    pub fn in_flight_recorder_handle(&self) -> *const std::ffi::c_void {
        match &self.in_flight {
            Some(f) => self.recorders[f.frame_index].raw_handle(),
            None => std::ptr::null(),
        }
    }

    /// Acquire the next swapchain image, run the caller's `render`
    /// closure with the recorder in scope, then submit + present.
    /// Returns `Ok(false)` if the swapchain returned `OUT_OF_DATE_KHR`
    /// during acquire — callers should drive [`Self::recreate`] and
    /// retry next frame. Reimplemented atop the
    /// [`Self::begin_frame`]/[`Self::end_frame`] split so the host-Rust
    /// closure API and the plugin-ABI split share one frame-loop
    /// implementation.
    #[tracing::instrument(level = "trace", skip(self, render))]
    pub fn render_frame<F>(&mut self, render: F) -> Result<bool>
    where
        F: FnOnce(&mut PresentFrame<'_>) -> Result<()>,
    {
        let acquired = match self.begin_frame()? {
            Some(a) => a,
            None => return Ok(false),
        };
        let frame_index = acquired.frame_index as usize;

        let recorder = &mut self.recorders[frame_index];
        let mut frame = PresentFrame {
            frame_index: acquired.frame_index,
            image_index: acquired.image_index,
            extent: acquired.extent,
            color_format: acquired.color_format,
            recorder,
            inner: PresentFrameInner {
                image_view: vk::ImageView::from_raw(acquired.image_view_raw),
                extra_waits: Vec::new(),
                in_render_pass: false,
            },
        };

        let user_result = render(&mut frame);

        // If the user opened a render pass and didn't close it, close it now.
        if frame.inner.in_render_pass {
            frame.recorder.cmd_end_dynamic_rendering()?;
            frame.inner.in_render_pass = false;
        }

        // Move the caller-accumulated timeline waits out before releasing
        // the recorder borrow so `end_frame` can fold them into the submit.
        let extra_waits = std::mem::take(&mut frame.inner.extra_waits);
        drop(frame);

        // Always barrier + submit + present, even on user draw error (see
        // `end_frame`'s forward-progress note); propagate the user error
        // AFTER the swapchain image is presented back consistent.
        let present_ok = self.end_frame(&extra_waits)?;
        user_result?;
        Ok(present_ok)
    }
}

impl<'a> PresentFrame<'a> {
    /// Add an additional timeline-semaphore wait to this frame's submit
    /// (e.g. a producer-finished timeline). Call before [`Self::begin_rendering`]
    /// or any draw, since the wait happens at the listed pipeline stage
    /// during submission.
    pub fn add_timeline_wait(
        &mut self,
        timeline: &HostVulkanTimelineSemaphore,
        value: u64,
        stage: VulkanStage,
    ) {
        let info = vk::SemaphoreSubmitInfo::builder()
            .semaphore(timeline.semaphore())
            .value(value)
            .stage_mask(stage.as_vk())
            .build();
        self.inner.extra_waits.push(info);
    }

    /// Open a dynamic-rendering pass on the acquired swapchain image.
    /// Must be paired with [`Self::end_rendering`]. The render-pass
    /// uses `CLEAR` load-op when `clear_color` is `Some`, `LOAD`
    /// otherwise (which preserves whatever was in the swapchain image —
    /// rare; only meaningful for incremental redraws against the same
    /// swapchain slot, which is not how this engine drives display).
    pub fn begin_rendering(&mut self, clear_color: Option<[f32; 4]>) -> Result<()> {
        if self.inner.in_render_pass {
            return Err(Error::GpuError(
                "PresentFrame::begin_rendering: render pass already active".into(),
            ));
        }
        self.recorder.cmd_begin_dynamic_rendering(
            self.inner.image_view,
            self.extent,
            clear_color,
        )?;
        self.inner.in_render_pass = true;
        Ok(())
    }

    /// Close the dynamic-rendering pass opened by [`Self::begin_rendering`].
    pub fn end_rendering(&mut self) -> Result<()> {
        if !self.inner.in_render_pass {
            return Err(Error::GpuError(
                "PresentFrame::end_rendering: no active render pass".into(),
            ));
        }
        self.recorder.cmd_end_dynamic_rendering()?;
        self.inner.in_render_pass = false;
        Ok(())
    }
}

impl Drop for VulkanPresentTarget {
    fn drop(&mut self) {
        // Drain in-flight work before destroying handles.
        let raw_device = self.device.device();
        let instance = self.device.instance();
        // Queue-mutex-guarded wait, not raw device_wait_idle (see
        // HostVulkanDevice::wait_idle — concurrent setup races otherwise).
        let _ = self.device.wait_idle();
        unsafe {
            for &view in &self.swapchain_image_views {
                raw_device.destroy_image_view(view, None);
            }
            for &sem in &self.render_finished_semaphores {
                raw_device.destroy_semaphore(sem, None);
            }
            for &sem in &self.image_available_semaphores {
                raw_device.destroy_semaphore(sem, None);
            }
            raw_device.destroy_swapchain_khr(self.swapchain, None);
            instance.destroy_surface_khr(self.surface, None);
        }
        // Recorders + frame_timeline drop their own resources.
    }
}

unsafe impl Send for VulkanPresentTarget {}
unsafe impl Sync for VulkanPresentTarget {}

/// Boxed, `Mutex`-guarded [`VulkanPresentTarget`] backing a
/// [`PresentTarget`] PluginAbiObject handle. The `Mutex` matches
/// `RhiCommandRecorderInner`'s state-guard discipline: the plugin-ABI
/// method bodies `try_lock` it so a concurrent or misordered
/// `begin_frame`/`end_frame` returns a typed error rather than aliasing
/// `&mut VulkanPresentTarget` (UB). Single-owner — leaked with
/// [`Box::into_raw`] at mint, reclaimed with `Box::from_raw` at drop.
pub type PresentTargetInner = Mutex<VulkanPresentTarget>;

/// Layout-stable `#[repr(C)]` PluginAbiObject projecting the host
/// [`VulkanPresentTarget`] swapchain orchestrator across the plugin ABI.
///
/// **Box-shaped, drop-only (`!Clone`).** The opaque `handle` points at a
/// `Box<PresentTargetInner>` (`Box<Mutex<VulkanPresentTarget>>`); the
/// target is single-owner, `!Clone`, `Drop`-heavy (drains + destroys the
/// surface / swapchain / semaphores / recorders), so it maps onto the
/// [`RhiCommandRecorder`] Box precedent, not the Arc clone/drop pair.
/// Drop dispatches the parent [`GpuContextFullAccessVTable`]'s
/// `drop_present_target` slot (`Box::from_raw` + drop in host-compiled
/// code, keeping every `vkDestroy*` inside the host build). A raw
/// `repr(Rust)` `Arc<VulkanPresentTarget>` transiting a separately-built
/// `.slpkg` would be cross-build layout skew — see
/// `docs/learnings/slpkg-raw-device-rhi-construction.md`; the Box
/// PluginAbiObject is what keeps destruction host-side.
///
/// Per-frame method dispatch (`begin_frame` / `end_frame` / `recreate` /
/// `set_hdr_metadata`) routes through the per-type
/// [`streamlib_plugin_abi::PresentTargetMethodsVTable`]; the swapchain
/// color format is a cached POD field (`color_format_raw`) read with a
/// zero-hop `&self` getter and refreshed on `recreate`.
#[repr(C)]
pub struct PresentTarget {
    /// Opaque handle to the host's `Box<PresentTargetInner>`.
    pub(crate) handle: *const c_void,
    /// Parent vtable for plugin-ABI Drop dispatch (`drop_present_target`).
    pub(crate) vtable: *const streamlib_plugin_abi::GpuContextFullAccessVTable,
    /// Per-type vtable for plugin-ABI method dispatch. Null in a
    /// vtable-less host-mode construction; populated by
    /// [`Self::from_target`].
    pub(crate) methods_vtable: *const streamlib_plugin_abi::PresentTargetMethodsVTable,
    /// Cached swapchain-image `TextureFormat` `#[repr(u32)]` discriminant.
    /// Refreshed from the `recreate` slot's `out_color_format_raw` so a
    /// format flip (SDR BGRA8 → HDR10 FP16) never leaves a stale getter.
    pub(crate) color_format_raw: u32,
    /// Reserved padding (keeps the struct a multiple of 8; zero, never read).
    pub(crate) _padding: u32,
}

// SAFETY: `handle` points at a `Box<Mutex<VulkanPresentTarget>>`; the
// inner is Send + Sync (VulkanPresentTarget is Send + Sync and the Mutex
// serializes access). The vtable pointers are `'static` host statics.
unsafe impl Send for PresentTarget {}
unsafe impl Sync for PresentTarget {}

impl PresentTarget {
    /// Leak `target` as a `Box<PresentTargetInner>` opaque handle and
    /// resolve the host-mode parent + per-type vtables, caching the
    /// initial swapchain color format. Minting entry point for the
    /// FullAccess `create_present_target` slot body.
    pub fn from_target(target: VulkanPresentTarget) -> Self {
        let color_format_raw = target.color_format() as u32;
        let handle = Box::into_raw(Box::new(Mutex::new(target))) as *const c_void;
        let vtable = crate::core::plugin::host_services::host_gpu_context_full_access_vtable();
        let methods_vtable =
            crate::core::plugin::host_services::host_present_target_methods_vtable();
        Self {
            handle,
            vtable,
            methods_vtable,
            color_format_raw,
            _padding: 0,
        }
    }

    /// Swapchain image color format `#[repr(u32)]` discriminant — a
    /// zero-hop cached-POD read (no plugin-ABI hop). Refreshed on
    /// `recreate`.
    pub fn color_format_raw(&self) -> u32 {
        self.color_format_raw
    }

    /// Opaque `Box<PresentTargetInner>` handle. Borrowed — the plugin-ABI
    /// method slots (`begin_frame` / `end_frame` / `recreate` /
    /// `set_hdr_metadata`) take it as `present_handle`. A host-side test
    /// harness drives the surface by passing this to the methods vtable.
    pub fn handle(&self) -> *const c_void {
        self.handle
    }
}

impl Drop for PresentTarget {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with `Box::into_raw` in `from_target`; the
            // vtable's `drop_present_target` runs `Box::from_raw` + drop
            // host-side.
            unsafe {
                ((*self.vtable).drop_present_target)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for PresentTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PresentTarget")
            .field("color_format_raw", &self.color_format_raw)
            .finish()
    }
}

/// Engine-internal: surface + dimensions + ColorTraits hint → swapchain
/// handle chain. Returns `(swapchain, images, image_views, format,
/// extent, color_pick)`. Colorspace negotiation is delegated to
/// [`pick_swapchain_format`].
fn create_swapchain(
    device: &Arc<HostVulkanDevice>,
    surface: vk::SurfaceKHR,
    width: u32,
    height: u32,
    vsync: bool,
    color_traits: Option<&ColorTraits>,
    old_swapchain: vk::SwapchainKHR,
) -> Result<(
    vk::SwapchainKHR,
    Vec<vk::Image>,
    Vec<vk::ImageView>,
    vk::Format,
    vk::Extent2D,
    SwapchainColorPick,
)> {
    let instance = device.instance();
    let physical_device = device.physical_device();
    let raw_device = device.device();

    let capabilities =
        unsafe { instance.get_physical_device_surface_capabilities_khr(physical_device, surface) }
            .map_err(|e| {
                Error::GpuError(format!(
                    "VulkanPresentTarget: get_physical_device_surface_capabilities_khr: {e}"
                ))
            })?;

    let surface_formats =
        unsafe { instance.get_physical_device_surface_formats_khr(physical_device, surface) }
            .map_err(|e| {
                Error::GpuError(format!(
                    "VulkanPresentTarget: get_physical_device_surface_formats_khr: {e}"
                ))
            })?;

    let present_modes =
        unsafe { instance.get_physical_device_surface_present_modes_khr(physical_device, surface) }
            .map_err(|e| {
                Error::GpuError(format!(
                    "VulkanPresentTarget: get_physical_device_surface_present_modes_khr: {e}"
                ))
            })?;

    if surface_formats.is_empty() {
        return Err(Error::GpuError(
            "VulkanPresentTarget: surface advertised no formats — \
             vkGetPhysicalDeviceSurfaceFormatsKHR returned an empty list"
                .into(),
        ));
    }

    let representable_formats = filter_representable_surface_formats(&surface_formats);
    if representable_formats.is_empty() {
        return Err(Error::GpuError(format!(
            "VulkanPresentTarget: surface advertised {} formats but none are \
             representable as TextureFormat (extend `vk_format_to_texture_format` \
             when adding new HDR / wide-gamut backends)",
            surface_formats.len()
        )));
    }

    let color_pick = pick_swapchain_format(&representable_formats, color_traits);
    let chosen_format = vk::SurfaceFormatKHR {
        format: color_pick.format,
        color_space: color_pick.color_space,
    };
    tracing::info!(
        "VulkanPresentTarget: swapchain colorspace pick {:?} + {:?} (is_hdr={}, color_traits={:?})",
        chosen_format.format,
        chosen_format.color_space,
        color_pick.is_hdr,
        color_traits,
    );

    let present_mode = if vsync {
        vk::PresentModeKHR::FIFO
    } else if present_modes.contains(&vk::PresentModeKHR::MAILBOX) {
        vk::PresentModeKHR::MAILBOX
    } else {
        vk::PresentModeKHR::FIFO
    };

    let extent = if capabilities.current_extent.width != u32::MAX {
        capabilities.current_extent
    } else {
        vk::Extent2D {
            width: width.clamp(
                capabilities.min_image_extent.width,
                capabilities.max_image_extent.width,
            ),
            height: height.clamp(
                capabilities.min_image_extent.height,
                capabilities.max_image_extent.height,
            ),
        }
    };

    let mut image_count = capabilities.min_image_count + 1;
    if capabilities.max_image_count > 0 && image_count > capabilities.max_image_count {
        image_count = capabilities.max_image_count;
    }

    let swapchain_info = vk::SwapchainCreateInfoKHR::builder()
        .surface(surface)
        .min_image_count(image_count)
        .image_format(chosen_format.format)
        .image_color_space(chosen_format.color_space)
        .image_extent(extent)
        .image_array_layers(1)
        .image_usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::COLOR_ATTACHMENT)
        .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
        .pre_transform(capabilities.current_transform)
        .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
        .present_mode(present_mode)
        .clipped(true)
        .old_swapchain(old_swapchain)
        .build();

    let swapchain = unsafe { raw_device.create_swapchain_khr(&swapchain_info, None) }
        .map_err(|e| Error::GpuError(format!("VulkanPresentTarget: create_swapchain_khr: {e}")))?;

    let images = unsafe { raw_device.get_swapchain_images_khr(swapchain) }.map_err(|e| {
        Error::GpuError(format!(
            "VulkanPresentTarget: get_swapchain_images_khr: {e}"
        ))
    })?;

    let mut image_views = Vec::with_capacity(images.len());
    for &image in &images {
        let view_info = vk::ImageViewCreateInfo::builder()
            .image(image)
            .view_type(vk::ImageViewType::_2D)
            .format(chosen_format.format)
            .components(vk::ComponentMapping {
                r: vk::ComponentSwizzle::IDENTITY,
                g: vk::ComponentSwizzle::IDENTITY,
                b: vk::ComponentSwizzle::IDENTITY,
                a: vk::ComponentSwizzle::IDENTITY,
            })
            .subresource_range(
                vk::ImageSubresourceRange::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(1)
                    .build(),
            )
            .build();
        let view = unsafe { raw_device.create_image_view(&view_info, None) }
            .map_err(|e| Error::GpuError(format!("VulkanPresentTarget: create_image_view: {e}")))?;
        image_views.push(view);
    }

    Ok((
        swapchain,
        images,
        image_views,
        chosen_format.format,
        extent,
        color_pick,
    ))
}

/// Strip surface formats the engine's [`vk_format_to_texture_format`]
/// table can't project to [`TextureFormat`]. Called by
/// [`create_swapchain`] before the priority walk so the picker can
/// never produce a `vk::Format` that fails downstream conversion —
/// e.g. NVIDIA exposes `A2B10G10R10_UNORM_PACK32 + HDR10_ST2084_EXT`
/// for HDR10 PQ, but the engine's `TextureFormat` enum doesn't yet
/// carry a 10-bit packed variant. The filter degrades gracefully —
/// the picker walks representable formats only, falling through to
/// FP16 HDR or SDR.
fn filter_representable_surface_formats(
    surface_formats: &[vk::SurfaceFormatKHR],
) -> Vec<vk::SurfaceFormatKHR> {
    surface_formats
        .iter()
        .copied()
        .filter(|sf| vk_format_to_texture_format(sf.format).is_some())
        .collect()
}

/// Field-wise equality for `vk::HdrMetadataEXT`. The struct has no
/// `PartialEq` impl in vulkanalia (it carries `*const c_void` for the
/// `pNext` chain — never compared here since we always pass `null()`),
/// so we compare the spec-significant fields directly. Used to
/// short-circuit redundant `vkSetHdrMetadataEXT` round-trips.
fn hdr_metadata_eq(a: Option<&vk::HdrMetadataEXT>, b: &vk::HdrMetadataEXT) -> bool {
    let Some(a) = a else { return false };
    a.display_primary_red.x == b.display_primary_red.x
        && a.display_primary_red.y == b.display_primary_red.y
        && a.display_primary_green.x == b.display_primary_green.x
        && a.display_primary_green.y == b.display_primary_green.y
        && a.display_primary_blue.x == b.display_primary_blue.x
        && a.display_primary_blue.y == b.display_primary_blue.y
        && a.white_point.x == b.white_point.x
        && a.white_point.y == b.white_point.y
        && a.max_luminance == b.max_luminance
        && a.min_luminance == b.min_luminance
        && a.max_content_light_level == b.max_content_light_level
        && a.max_frame_average_light_level == b.max_frame_average_light_level
}

fn vk_format_to_texture_format(format: vk::Format) -> Option<TextureFormat> {
    match format {
        vk::Format::B8G8R8A8_UNORM => Some(TextureFormat::Bgra8Unorm),
        vk::Format::B8G8R8A8_SRGB => Some(TextureFormat::Bgra8UnormSrgb),
        vk::Format::R8G8B8A8_UNORM => Some(TextureFormat::Rgba8Unorm),
        vk::Format::R8G8B8A8_SRGB => Some(TextureFormat::Rgba8UnormSrgb),
        // FP16 scanout — the representable path for both `HDR10_ST2084_EXT`
        // and `EXTENDED_SRGB_LINEAR_EXT` colorspaces. The 10-bit packed
        // formats `A2B10G10R10_UNORM_PACK32` / `A2R10G10B10_UNORM_PACK32`
        // (the canonical HDR10 wire format) need a `Rgb10A2Unorm` variant
        // on `TextureFormat` to map; not added here as it's a multi-
        // backend / consumer-rhi change.
        vk::Format::R16G16B16A16_SFLOAT => Some(TextureFormat::Rgba16Float),
        _ => None,
    }
}

/// Test-only synthetic `PresentFrame` constructor. Locks the
/// `PresentFrameInner` state-machine (extra-waits accumulation +
/// `in_render_pass` flag) without requiring a real swapchain or
/// `vk::ImageView` — the `image_view` is left null because the
/// state-machine tests below never record `cmd_begin_rendering`
/// against it. The full acquire/submit/present cycle is exercised
/// by the camera+display E2E (see the `/verify-live` skill).
#[cfg(test)]
fn synthetic_present_frame<'a>(
    recorder: &'a mut RhiCommandRecorder,
    frame_index: u32,
    image_index: u32,
    extent: (u32, u32),
    color_format: TextureFormat,
) -> PresentFrame<'a> {
    PresentFrame {
        frame_index,
        image_index,
        extent,
        color_format,
        recorder,
        inner: PresentFrameInner {
            image_view: vk::ImageView::null(),
            extra_waits: Vec::new(),
            in_render_pass: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
        HostVulkanDevice::new().ok()
    }

    /// `vk_format_to_texture_format` is the format-mapping seam between
    /// swapchain surface negotiation and the kernel's
    /// `AttachmentFormats`. Mentally reverting any arm — say,
    /// `B8G8R8A8_UNORM → None` — would silently fail at run time on
    /// surfaces that picked that format; the test locks each arm.
    #[test]
    fn format_mapping_covers_canonical_surface_formats() {
        assert_eq!(
            vk_format_to_texture_format(vk::Format::B8G8R8A8_UNORM),
            Some(TextureFormat::Bgra8Unorm)
        );
        assert_eq!(
            vk_format_to_texture_format(vk::Format::B8G8R8A8_SRGB),
            Some(TextureFormat::Bgra8UnormSrgb)
        );
        assert_eq!(
            vk_format_to_texture_format(vk::Format::R8G8B8A8_UNORM),
            Some(TextureFormat::Rgba8Unorm)
        );
        assert_eq!(
            vk_format_to_texture_format(vk::Format::R8G8B8A8_SRGB),
            Some(TextureFormat::Rgba8UnormSrgb)
        );
        assert_eq!(
            vk_format_to_texture_format(vk::Format::R16G16B16A16_SFLOAT),
            Some(TextureFormat::Rgba16Float),
            "FP16 scanout drives both HDR10 (when 10-bit packed isn't \
             representable) and EXTENDED_SRGB_LINEAR — must round-trip"
        );
        assert_eq!(
            vk_format_to_texture_format(vk::Format::D32_SFLOAT),
            None,
            "non-color-attachment format must not map"
        );
    }

    /// Locks the contract `create_swapchain` relies on: when a surface
    /// exposes the picker-preferred HDR10 10-bit packed format that
    /// `vk_format_to_texture_format` doesn't yet support, the
    /// representability filter in `create_swapchain` strips it AND the
    /// picker then walks the remaining (representable) formats. Without
    /// the filter, an HDR10-capable surface that exposed only the
    /// 10-bit packed pair would have been picked and then errored out
    /// at the format-to-TextureFormat conversion step in
    /// `create_swapchain`. The construction-time path is hardware-only
    /// (needs a real surface), so this test simulates the
    /// filter+pick contract directly.
    #[test]
    fn representability_filter_drops_unsupported_picker_targets() {
        use crate::core::color::{ColorTraits, PrimariesId, TransferId};

        let surface_formats = vec![
            vk::SurfaceFormatKHR {
                format: vk::Format::B8G8R8A8_UNORM,
                color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
            },
            // HDR10 with the canonical 10-bit packed format — picker
            // would prefer this if unfiltered, but the engine has no
            // `TextureFormat` variant for it today.
            vk::SurfaceFormatKHR {
                format: vk::Format::A2B10G10R10_UNORM_PACK32,
                color_space: vk::ColorSpaceKHR::HDR10_ST2084_EXT,
            },
        ];
        let pq_bt2020 = ColorTraits {
            primaries: Some(PrimariesId::Bt2020),
            transfer: Some(TransferId::Pq),
        };

        // Without the filter: picker would pick A2B10G10R10_UNORM_PACK32
        // and `create_swapchain` would error at format conversion.
        let unfiltered_pick = pick_swapchain_format(&surface_formats, Some(&pq_bt2020));
        assert_eq!(unfiltered_pick.format, vk::Format::A2B10G10R10_UNORM_PACK32);
        assert!(
            vk_format_to_texture_format(unfiltered_pick.format).is_none(),
            "fixture assumption: picker-preferred HDR10 packed format must \
             remain unrepresentable, otherwise this test no longer locks the \
             filter contract"
        );

        // With the filter: picker walks representable formats only and
        // gracefully falls through to SDR (since FP16 isn't exposed
        // here). Call the same helper `create_swapchain` uses so a
        // future regression that bypasses the filter at the production
        // call site fails this test as soon as the helper's behavior
        // diverges from inline filtering.
        let representable = filter_representable_surface_formats(&surface_formats);
        let filtered_pick = pick_swapchain_format(&representable, Some(&pq_bt2020));
        assert_eq!(filtered_pick.format, vk::Format::B8G8R8A8_UNORM);
        assert_eq!(filtered_pick.color_space, vk::ColorSpaceKHR::SRGB_NONLINEAR);
        assert!(!filtered_pick.is_hdr);
        assert!(vk_format_to_texture_format(filtered_pick.format).is_some());
    }

    /// `MAX_FRAMES_IN_FLIGHT = 2` is load-bearing across the engine
    /// (see `docs/learnings/vulkan-frames-in-flight.md`). Locking the
    /// constant here catches a silent change that would over-allocate
    /// per-frame resources.
    #[test]
    fn max_frames_in_flight_is_two() {
        assert_eq!(MAX_FRAMES_IN_FLIGHT, 2);
    }

    /// The `PresentTarget` PluginAbiObject is `#[repr(C)]` and must stay
    /// byte-identical to the SDK twin
    /// (`sdk/streamlib-plugin-sdk/src/rhi/present_target.rs`). Both arms
    /// pin this layout; a drift on either side is a silent cross-build
    /// corruption. Mentally reverting a field reorder makes this fail.
    #[test]
    fn present_target_pluginabiobject_layout() {
        use core::mem::{align_of, offset_of, size_of};
        assert_eq!(size_of::<PresentTarget>(), 32);
        assert_eq!(align_of::<PresentTarget>(), 8);
        assert_eq!(offset_of!(PresentTarget, handle), 0);
        assert_eq!(offset_of!(PresentTarget, vtable), 8);
        assert_eq!(offset_of!(PresentTarget, methods_vtable), 16);
        assert_eq!(offset_of!(PresentTarget, color_format_raw), 24);
        assert_eq!(offset_of!(PresentTarget, _padding), 28);
    }

    /// `PresentTarget` is deliberately NOT `Clone` — the backing
    /// `VulkanPresentTarget` is single-owner `Drop`-heavy, so the parent
    /// vtable carries only `drop_present_target`, no clone slot.
    /// ```compile_fail
    /// fn assert_clone<T: Clone>() {}
    /// assert_clone::<streamlib_engine::vulkan::rhi::PresentTarget>();
    /// ```
    #[test]
    fn present_target_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PresentTarget>();
    }

    /// `PresentFrame::add_timeline_wait` must accumulate in insertion
    /// order — the submit copies the buffer verbatim into
    /// `wait_semaphore_infos`, and GPU sync correctness depends on the
    /// ordering the caller asked for. Mentally reverting `push` to
    /// `insert(0, info)` (or a `HashSet` re-ordering) would silently
    /// reshuffle producer-finished waits.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests"
    )]
    #[test]
    fn present_frame_extra_waits_accumulate_in_insertion_order() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => {
                println!("Skipping — no Vulkan device available");
                return;
            }
        };
        let timeline_a = HostVulkanTimelineSemaphore::new(device.device(), 0).expect("timeline a");
        let timeline_b = HostVulkanTimelineSemaphore::new(device.device(), 0).expect("timeline b");
        let mut recorder =
            RhiCommandRecorder::new(&device, "test-present-frame-waits").expect("recorder");

        {
            let mut frame = synthetic_present_frame(
                &mut recorder,
                0,
                0,
                (1920, 1080),
                TextureFormat::Bgra8Unorm,
            );
            frame.add_timeline_wait(&timeline_a, 7, VulkanStage::FRAGMENT_SHADER);
            frame.add_timeline_wait(&timeline_b, 13, VulkanStage::COLOR_ATTACHMENT_OUTPUT);
            assert_eq!(frame.inner.extra_waits.len(), 2);
            assert_eq!(frame.inner.extra_waits[0].semaphore, timeline_a.semaphore());
            assert_eq!(frame.inner.extra_waits[0].value, 7);
            assert_eq!(frame.inner.extra_waits[1].semaphore, timeline_b.semaphore());
            assert_eq!(frame.inner.extra_waits[1].value, 13);
        }
    }

    /// Double `begin_rendering` must error — the `in_render_pass` flag
    /// is what prevents the recorder from issuing a nested
    /// `vkCmdBeginRendering` on the same primary command buffer (which
    /// is a Vulkan validation error). Mentally reverting the flag
    /// check to `Ok(())` makes this test fail.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests"
    )]
    #[test]
    fn present_frame_begin_rendering_twice_is_typed_error() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => {
                println!("Skipping — no Vulkan device available");
                return;
            }
        };
        let mut recorder =
            RhiCommandRecorder::new(&device, "test-present-frame-double-begin").expect("recorder");
        recorder.begin().expect("begin recording");

        let mut frame =
            synthetic_present_frame(&mut recorder, 0, 0, (1920, 1080), TextureFormat::Bgra8Unorm);
        // First begin_rendering would normally succeed and issue a
        // real `cmd_begin_rendering` — but `image_view` is null in this
        // synthetic frame, so the underlying Vulkan call records a
        // begin against a null view (validation would flag this if it
        // ran on a real submit). The state-machine flag flip is what
        // we're locking, not the Vulkan-side correctness; the second
        // call must short-circuit BEFORE reaching the cmd_begin call.
        //
        // Force-set the flag to simulate "begin_rendering already
        // succeeded" without poking the null view:
        frame.inner.in_render_pass = true;

        let err = frame
            .begin_rendering(Some([0.0, 0.0, 0.0, 1.0]))
            .err()
            .expect("expected typed error");
        let msg = format!("{err}");
        assert!(msg.contains("render pass already active"), "got: {msg}");

        // Drain the recorder so Drop is clean.
        frame.inner.in_render_pass = false;
        drop(frame);
        let _ = recorder.submit_and_wait();
    }

    /// `end_rendering` without an active render pass must error — the
    /// `in_render_pass` flag is what prevents a stray
    /// `vkCmdEndRendering` outside a begun render pass (a Vulkan
    /// validation error). Mentally reverting the flag check to
    /// unconditionally record `cmd_end_rendering` makes this test
    /// fail.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests"
    )]
    #[test]
    fn present_frame_end_rendering_without_begin_is_typed_error() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => {
                println!("Skipping — no Vulkan device available");
                return;
            }
        };
        let mut recorder =
            RhiCommandRecorder::new(&device, "test-present-frame-stray-end").expect("recorder");
        recorder.begin().expect("begin recording");

        let mut frame =
            synthetic_present_frame(&mut recorder, 0, 0, (1920, 1080), TextureFormat::Bgra8Unorm);

        let err = frame.end_rendering().err().expect("expected typed error");
        let msg = format!("{err}");
        assert!(msg.contains("no active render pass"), "got: {msg}");

        drop(frame);
        let _ = recorder.submit_and_wait();
    }

    /// Smoke construction: requires a Vulkan device + winit window. We
    /// can build a Vulkan device in tests but not a winit window
    /// (event-loop-per-process). The render-frame loop is exercised
    /// end-to-end by the camera+display E2E (see the `/verify-live` skill),
    /// so this test focuses on the device-only init path.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests"
    )]
    #[test]
    fn construct_device_for_present_target_smoke() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => {
                println!("Skipping — no Vulkan device available");
                return;
            }
        };
        let _ = device.queue_family_index();
    }
}
