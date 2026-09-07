// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The virtual camera's PipeWire door: a `Video/Source` node with
//! `media.role = Camera` that any portal-based application can select.
//!
//! The node exists from `open` to `close` whether or not anything ever watches
//! it — a camera in the picker with nobody looking — and needs no kernel
//! module and no root, which is what makes a fresh `pip install` have a camera
//! door on every machine.
//!
//! libpipewire is reached through the process's one `dlsym`'d table
//! ([`crate::linux::pipewire_runtime_library`]) and SPA's pod builders through
//! `pipewire_video_source_shim.c`, so nothing here links a video library.
//!
//! The consumer chooses how the pixels travel. Its first choice is the
//! engine's own DMA-BUF textures, imported with no copy; the shared-memory
//! sibling is there for a consumer that cannot import one, and costs a
//! read-back into host-cached memory. Either way the pictures are the same
//! textures, written by one GPU pass.

use std::ffi::{CStr, CString};
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Arc;

use crate::core::context::{GpuContextFullAccess, GpuContextLimitedAccess, TextureRegistration};
use crate::core::rhi::{Texture, TextureFormat, VulkanLayout};
use crate::core::{Error, Result};
use crate::host_rhi::HostTextureExt;
use crate::linux::pipewire_runtime_library::{PipeWireLibraryEntryPoints, ShimFailureText};
use crate::vulkan::rhi::{
    HostMappingWrittenByGpu, ImageCopyRegion, PresentScalingMode, RhiCommandRecorder, VulkanAccess,
    VulkanPresentCompositor, VulkanStage,
};

/// How many buffers a camera node offers.
///
/// Four is what the loopback door's readers ask for and what Chromium's
/// enumerator expects; PipeWire is told this exact count because every one of
/// them is allocated and exported before the stream can negotiate.
const CAMERA_BUFFER_COUNT: usize = 4;

/// The frame interval offered when a frame carries no rate of its own. It is a
/// hint on a source that publishes when it has a picture, not a promise.
const DEFAULT_FRAMES_PER_SECOND: u32 = 30;

/// The engine's textures are `Rgba8Unorm`, which the shim offers as SPA's
/// `RGBA` — the same byte order, `DRM_FORMAT_ABGR8888`.
const CAMERA_TEXTURE_FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;

mod video_shim {
    use std::ffi::{c_char, c_int, c_void};

    /// Mirrors `struct StreamLibPipeWireVideoSource`, which is opaque here.
    #[repr(C)]
    pub struct VideoSource {
        _opaque: [u8; 0],
    }

    /// Mirrors `struct StreamLibPipeWireVideoDmaBufPlane`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct DmaBufPlane {
        pub file_descriptor: i32,
        pub stride_bytes: u32,
        pub offset_bytes: u32,
        pub byte_size: u32,
    }

    /// `STREAMLIB_PIPEWIRE_VIDEO_BUFFER_KIND_DMA_BUF`.
    pub const BUFFER_KIND_DMA_BUF: u32 = 1;
    /// `STREAMLIB_PIPEWIRE_VIDEO_BUFFER_KIND_SHARED_MEMORY`.
    pub const BUFFER_KIND_SHARED_MEMORY: u32 = 2;

    unsafe extern "C" {
        pub fn streamlib_pipewire_video_source_open(
            entry_points: *const *mut c_void,
            camera_name: *const c_char,
            failure_text: *mut c_char,
            failure_text_capacity: usize,
        ) -> *mut VideoSource;
        pub fn streamlib_pipewire_video_source_set_extent(
            video_source: *mut VideoSource,
            width: u32,
            height: u32,
            framerate_numerator: u32,
            framerate_denominator: u32,
            drm_modifier: u64,
            planes: *const DmaBufPlane,
            plane_count: u32,
            failure_text: *mut c_char,
            failure_text_capacity: usize,
        ) -> c_int;
        pub fn streamlib_pipewire_video_source_negotiated_buffer_kind(
            video_source: *mut VideoSource,
        ) -> u32;
        pub fn streamlib_pipewire_video_source_dequeue_slot(
            video_source: *mut VideoSource,
            buffer_kind_out: *mut u32,
        ) -> i32;
        pub fn streamlib_pipewire_video_source_slot_shared_memory(
            video_source: *mut VideoSource,
            slot: i32,
            stride_bytes_out: *mut u32,
            byte_size_out: *mut u32,
        ) -> *mut u8;
        pub fn streamlib_pipewire_video_source_queue_slot(
            video_source: *mut VideoSource,
            slot: i32,
            timestamp_ns: i64,
            sequence: u64,
        ) -> c_int;
        pub fn streamlib_pipewire_video_source_failure(
            video_source: *mut VideoSource,
        ) -> *const c_char;
        pub fn streamlib_pipewire_video_source_close(video_source: *mut VideoSource);
    }

    /// Mirrors `struct StreamLibPipeWireVideoOfferedFormat`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    #[cfg(test)]
    pub struct OfferedFormat {
        pub width: u32,
        pub height: u32,
        pub framerate_numerator: u32,
        pub framerate_denominator: u32,
        pub drm_modifier: u64,
    }

    /// Mirrors `struct StreamLibPipeWireVideoOfferReport`.
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    #[cfg(test)]
    pub struct OfferReport {
        pub width: u32,
        pub height: u32,
        pub dma_buf_modifier_count: u32,
        pub dma_buf_modifier: i64,
        pub dma_buf_modifier_is_mandatory: bool,
        pub dma_buf_modifier_may_not_be_fixated: bool,
        pub shared_memory_format_carries_a_modifier: bool,
        pub both_formats_were_built: bool,
    }

    // The `node.name` a camera registers under, the property dict it announces
    // itself with, and what its offered formats actually say. The shim calls
    // the first two on every open; Rust only ever calls them to hold the
    // composition in a test, which is why they are declared there.
    #[cfg(test)]
    unsafe extern "C" {
        pub fn streamlib_pipewire_video_source_describe_offer(
            offered_format: *const OfferedFormat,
            fixated: bool,
        ) -> OfferReport;
        pub fn streamlib_pipewire_video_source_node_name(
            camera_name: *const c_char,
            node_name: *mut c_char,
            node_name_capacity: usize,
        ) -> usize;
        pub fn streamlib_pipewire_video_source_properties(
            items: *mut StreamProperty,
            item_capacity: u32,
            camera_name: *const c_char,
            node_name: *mut c_char,
            node_name_capacity: usize,
        ) -> u32;
    }

    /// Mirrors `struct StreamLibPipeWireStreamProperty`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    #[cfg(test)]
    pub struct StreamProperty {
        pub key: *const c_char,
        pub value: *const c_char,
    }
}

/// How a consumer settled on carrying this camera's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeWireCameraBufferKind {
    /// Nothing is watching, so nothing has been negotiated.
    NothingNegotiated,
    /// The consumer imports the engine's textures with no copy.
    DmaBufImportedByTheConsumer,
    /// The consumer took the shared-memory sibling, so each frame is read back
    /// into host-cached memory and copied.
    SharedMemoryCopy,
}

impl PipeWireCameraBufferKind {
    /// The word a log line should carry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NothingNegotiated => "nothing_negotiated",
            Self::DmaBufImportedByTheConsumer => "dma_buf",
            Self::SharedMemoryCopy => "shared_memory",
        }
    }

    fn from_shim(kind: u32) -> Self {
        match kind {
            video_shim::BUFFER_KIND_DMA_BUF => Self::DmaBufImportedByTheConsumer,
            video_shim::BUFFER_KIND_SHARED_MEMORY => Self::SharedMemoryCopy,
            // `STREAMLIB_PIPEWIRE_VIDEO_BUFFER_KIND_NONE`, which is what an
            // unnegotiated stream reports.
            _ => Self::NothingNegotiated,
        }
    }
}

/// What became of one frame handed to the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeWireCameraFramePresentation {
    /// The frame is on its way to a consumer, which is carrying it this way.
    Published(PipeWireCameraBufferKind),
    /// No application has the camera open, so the frame is dropped — which is
    /// what a camera nobody is watching does with its pictures.
    NoConsumerIsWatching,
    /// Every buffer is still with the consumer, so this frame is dropped
    /// rather than waited on.
    EveryBufferIsHeldByTheConsumer,
}

/// One offered extent: the DMA-BUF textures the consumer imports, and — on the
/// shared-memory sibling — the RHI's view of the shim's own mappings.
struct OfferedCameraExtent {
    width: u32,
    height: u32,
    drm_modifier: u64,
    /// One per buffer slot, in the order the shim hands them out.
    slot_textures: Vec<Texture>,
    /// The exported descriptors, closed when this extent is replaced.
    /// PipeWire and its consumers `dup` on receipt, so closing here is safe
    /// while a consumer still holds the buffer.
    _exported_plane_descriptors: Vec<OwnedFd>,
    /// Filled lazily on the shared-memory sibling's first frame, one per slot.
    shared_memory_written_by_gpu: Vec<Option<SharedMemorySlotWrittenByGpu>>,
    /// The descriptors handed to the shim, kept beside the fds they name.
    planes: Vec<video_shim::DmaBufPlane>,
}

/// The RHI's view of one shared-memory slot, beside the range it names.
///
/// The range is recorded so a frame can check it against what the shim reports
/// rather than trusting a cache: the import is only sound while the mapping it
/// was taken from is still there, and the shim's contract — mappings live
/// exactly as long as the extent that allocated them — is what makes holding
/// one across frames safe. A mismatch means that contract broke, and the frame
/// is refused by name rather than writing through an address this process may
/// no longer own.
struct SharedMemorySlotWrittenByGpu {
    /// Held as an address rather than a pointer: it is only ever compared for
    /// identity, never read through, and a pointer here would be a `Send`
    /// obligation this type has no business making.
    host_range_address: usize,
    host_range_byte_len: usize,
    written_by_gpu: HostMappingWrittenByGpu,
}

/// The shim-owned source, held as its own type so that `Send` is promised for
/// the pointer alone — a field added to [`PipeWireCameraNode`] later must earn
/// it structurally rather than inherit an assertion made about something else.
struct ShimOwnedVideoSourcePointer(*mut video_shim::VideoSource);

// The source is owned by the value that holds this pointer, and every shim
// entry point takes PipeWire's thread-loop lock, so it is only ever used from
// whichever thread holds that value.
unsafe impl Send for ShimOwnedVideoSourcePointer {}

/// A `Video/Source` node, alive for as long as this value is.
pub struct PipeWireCameraNode {
    /// Kept solely so the resolved entry points outlive the shim's use of them.
    _entry_points: Arc<PipeWireLibraryEntryPoints>,
    video_source: ShimOwnedVideoSourcePointer,
    camera_name: String,
    gpu: GpuContextLimitedAccess,
    compositor: VulkanPresentCompositor,
    recorder: RhiCommandRecorder,
    offered: Option<OfferedCameraExtent>,
    published_frame_count: u64,
}

impl PipeWireCameraNode {
    /// Register a camera node named `camera_name`.
    ///
    /// Refuses by name when no libpipewire loads or no session daemon answers —
    /// the same probe-by-opening the audio chain uses, because a library
    /// present with no daemon behind it is the ordinary container case.
    pub fn open(gpu_full: &GpuContextFullAccess, camera_name: &str) -> Result<Self> {
        let entry_points = PipeWireLibraryEntryPoints::loaded_once_per_process()
            .map_err(|reason| Self::no_door(camera_name, &reason.to_string()))?;
        entry_points
            .daemon_answers()
            .map_err(|reason| Self::no_door(camera_name, &reason))?;

        let camera_name_c = CString::new(camera_name).map_err(|_| {
            Error::Runtime(format!(
                "VirtualCameraSink \"{camera_name}\": a camera name cannot contain a NUL byte"
            ))
        })?;
        let mut failure_text = ShimFailureText::new();
        let (failure_text_ptr, failure_text_capacity) = failure_text.as_shim_out_parameters();
        // SAFETY: the table is fully resolved, the name is NUL-terminated and
        // outlives the call, and the out-buffer's pointer and capacity come
        // from the buffer itself.
        let video_source = unsafe {
            video_shim::streamlib_pipewire_video_source_open(
                entry_points.as_ptr(),
                camera_name_c.as_ptr(),
                failure_text_ptr,
                failure_text_capacity,
            )
        };
        if video_source.is_null() {
            return Err(Self::no_door(camera_name, &failure_text.read()));
        }

        let node = Self {
            _entry_points: Arc::clone(entry_points),
            video_source: ShimOwnedVideoSourcePointer(video_source),
            camera_name: camera_name.to_string(),
            gpu: gpu_full.host_inner().limited_access(),
            // This camera's own compositor and recorder: two sinks in one graph
            // must not share a descriptor ring across their threads.
            compositor: gpu_full.create_present_compositor(CAMERA_TEXTURE_FORMAT)?,
            recorder: gpu_full.create_command_recorder("virtual_camera_pipewire_door")?,
            offered: None,
            published_frame_count: 0,
        };
        Ok(node)
    }

    fn no_door(camera_name: &str, reason: &str) -> Error {
        Error::Runtime(format!(
            "VirtualCameraSink \"{camera_name}\": the PipeWire camera door is not available: \
             {reason}"
        ))
    }

    /// Offer one extent, allocating and exporting the buffers a consumer will
    /// import. Replaces any extent offered earlier; consumers re-negotiate.
    pub fn offer_extent(
        &mut self,
        width: u32,
        height: u32,
        frames_per_second: Option<u32>,
    ) -> Result<()> {
        // One escalation per extent, not per frame: allocating the buffers a
        // consumer imports is the only thing on this path that needs it.
        let gpu = self.gpu.clone();
        let offered = gpu.escalate(|gpu_full| {
            self.allocate_the_buffers_a_consumer_imports(gpu_full, width, height)
        })?;
        // The previous extent goes before the shim frees the mappings its
        // imports name — `set_extent`'s contract, and the reason this is not
        // simply an assignment at the end.
        self.offered = None;
        self.offer_to_pipewire(offered, width, height, frames_per_second)
    }

    fn allocate_the_buffers_a_consumer_imports(
        &self,
        gpu_full: &GpuContextFullAccess,
        width: u32,
        height: u32,
    ) -> Result<OfferedCameraExtent> {
        let mut slot_textures = Vec::with_capacity(CAMERA_BUFFER_COUNT);
        let mut exported_plane_descriptors = Vec::with_capacity(CAMERA_BUFFER_COUNT);
        let mut planes = Vec::with_capacity(CAMERA_BUFFER_COUNT);
        let mut offered_modifier: Option<u64> = None;

        for slot_index in 0..CAMERA_BUFFER_COUNT {
            let texture = gpu_full.acquire_render_target_dma_buf_image(
                width,
                height,
                CAMERA_TEXTURE_FORMAT,
            )?;
            let vulkan_texture = texture.vulkan_inner();
            let modifier = vulkan_texture.chosen_drm_format_modifier();
            // One modifier is offered, so every slot has to carry it: a
            // consumer that fixated on the first slot's modifier would import
            // the rest as a tiling they are not in.
            if *offered_modifier.get_or_insert(modifier) != modifier {
                return Err(Error::Runtime(format!(
                    "VirtualCameraSink \"{}\": the driver gave buffer {slot_index} DRM modifier \
                     {modifier:#x} rather than {:#x}, so one modifier cannot describe them all",
                    self.camera_name,
                    offered_modifier.unwrap_or(modifier)
                )));
            }
            let (offset_bytes, stride_bytes) = vulkan_texture
                .dma_buf_plane_layout()?
                .first()
                .copied()
                .ok_or_else(|| {
                    Error::Runtime(format!(
                        "VirtualCameraSink \"{}\": buffer {slot_index} reported no DMA-BUF plane",
                        self.camera_name
                    ))
                })?;
            let file_descriptor = vulkan_texture.export_dma_buf_fd()?;
            // SAFETY: `export_dma_buf_fd` hands back a fresh kernel descriptor
            // the caller owns; wrapping it here is what closes it exactly once.
            exported_plane_descriptors.push(unsafe { OwnedFd::from_raw_fd(file_descriptor) });
            planes.push(video_shim::DmaBufPlane {
                file_descriptor,
                stride_bytes: self.plane_field_of(stride_bytes, "stride", slot_index)?,
                offset_bytes: self.plane_field_of(offset_bytes, "offset", slot_index)?,
                byte_size: self.plane_field_of(
                    vulkan_texture.vma_allocation_size(),
                    "allocation size",
                    slot_index,
                )?,
            });
            slot_textures.push(texture);
        }

        Ok(OfferedCameraExtent {
            width,
            height,
            drm_modifier: offered_modifier.unwrap_or_default(),
            slot_textures,
            _exported_plane_descriptors: exported_plane_descriptors,
            shared_memory_written_by_gpu: (0..CAMERA_BUFFER_COUNT).map(|_| None).collect(),
            planes,
        })
    }

    /// `spa_data` sizes every field as a `u32`, so a driver-reported value that
    /// will not fit is named rather than narrowed — a truncated size would tell
    /// PipeWire a buffer is smaller than the memory it hands over.
    fn plane_field_of(&self, value: u64, field: &str, slot_index: usize) -> Result<u32> {
        u32::try_from(value).map_err(|_| {
            Error::Runtime(format!(
                "VirtualCameraSink \"{}\": buffer {slot_index} reported a {field} of {value}, \
                 which does not fit the 32 bits PipeWire gives it",
                self.camera_name
            ))
        })
    }

    fn offer_to_pipewire(
        &mut self,
        offered: OfferedCameraExtent,
        width: u32,
        height: u32,
        frames_per_second: Option<u32>,
    ) -> Result<()> {
        let drm_modifier = offered.drm_modifier;
        let mut failure_text = ShimFailureText::new();
        let (failure_text_ptr, failure_text_capacity) = failure_text.as_shim_out_parameters();
        // SAFETY: the source is live, `planes` holds exactly `plane_count`
        // entries and outlives the call, which copies them.
        let taken = unsafe {
            video_shim::streamlib_pipewire_video_source_set_extent(
                self.video_source.0,
                width,
                height,
                frames_per_second
                    .filter(|&rate| rate > 0)
                    .unwrap_or(DEFAULT_FRAMES_PER_SECOND),
                1,
                drm_modifier,
                offered.planes.as_ptr(),
                offered.planes.len() as u32,
                failure_text_ptr,
                failure_text_capacity,
            )
        };
        if taken != 0 {
            return Err(Error::Runtime(format!(
                "VirtualCameraSink \"{}\": PipeWire would not take a {width}x{height} camera \
                 format: {}",
                self.camera_name,
                failure_text.read()
            )));
        }

        self.offered = Some(offered);
        tracing::info!(
            camera = %self.camera_name,
            width,
            height,
            buffers = CAMERA_BUFFER_COUNT,
            drm_modifier = format!("{drm_modifier:#x}"),
            "VirtualCameraSink: the PipeWire camera offers this extent"
        );
        Ok(())
    }

    /// The extent currently offered, if one is.
    pub fn offered_extent(&self) -> Option<(u32, u32)> {
        self.offered
            .as_ref()
            .map(|offered| (offered.width, offered.height))
    }

    /// The DRM modifier the offered buffers carry.
    pub fn offered_drm_modifier(&self) -> Option<u64> {
        self.offered.as_ref().map(|offered| offered.drm_modifier)
    }

    /// How a consumer settled on carrying the pixels.
    pub fn negotiated_buffer_kind(&self) -> PipeWireCameraBufferKind {
        // SAFETY: the source is live for as long as this value is.
        PipeWireCameraBufferKind::from_shim(unsafe {
            video_shim::streamlib_pipewire_video_source_negotiated_buffer_kind(self.video_source.0)
        })
    }

    /// The reason the node failed after it was registered, if it has.
    pub fn failure(&self) -> Option<String> {
        // SAFETY: the source is live. The text is written once and never
        // rewritten — `record_stream_failure` returns early on an already
        // failed source — so a pointer handed out after `stream_failed` was
        // observed true under the loop's lock names an immutable buffer.
        unsafe {
            let failure = video_shim::streamlib_pipewire_video_source_failure(self.video_source.0);
            if failure.is_null() {
                None
            } else {
                Some(CStr::from_ptr(failure).to_string_lossy().into_owned())
            }
        }
    }

    /// Write `source` into the next free buffer and publish it, stamped
    /// `timestamp_ns`.
    ///
    /// `source` is another processor's surface, sampled onto the camera's own
    /// texture. A frame that is composed comes back in
    /// `SHADER_READ_ONLY_OPTIMAL` — the layout the compositor's sampled
    /// descriptor declares — and its registration is republished there; a frame
    /// no consumer was waiting for is not touched at all.
    pub fn present_texture(
        &mut self,
        source: &TextureRegistration,
        timestamp_ns: i64,
    ) -> Result<PipeWireCameraFramePresentation> {
        if let Some(reason) = self.failure() {
            return Err(Error::Runtime(format!(
                "VirtualCameraSink \"{}\": {reason}",
                self.camera_name
            )));
        }
        if self.offered.is_none() {
            return Ok(PipeWireCameraFramePresentation::NoConsumerIsWatching);
        }
        // The kind comes back with the slot, out of one locked call: reading it
        // separately would let a renegotiation land between the two and leave
        // this frame filling the slot the wrong way.
        let mut negotiated_kind = 0_u32;
        // SAFETY: the source is live and the out-parameter is this frame's own
        // local; a negative answer means no buffer, and a non-negative one is a
        // slot the shim now holds for this caller.
        let slot = unsafe {
            video_shim::streamlib_pipewire_video_source_dequeue_slot(
                self.video_source.0,
                &mut negotiated_kind,
            )
        };
        let buffer_kind = PipeWireCameraBufferKind::from_shim(negotiated_kind);
        if slot < 0 {
            return Ok(
                if buffer_kind == PipeWireCameraBufferKind::NothingNegotiated {
                    PipeWireCameraFramePresentation::NoConsumerIsWatching
                } else {
                    PipeWireCameraFramePresentation::EveryBufferIsHeldByTheConsumer
                },
            );
        }

        match self.write_slot_and_publish(slot, buffer_kind, source, timestamp_ns) {
            Ok(()) => {
                self.published_frame_count += 1;
                Ok(PipeWireCameraFramePresentation::Published(buffer_kind))
            }
            // A slot this call could not fill stays held by the shim and is
            // handed back on the next frame, because queueing an output buffer
            // publishes it — there is no way to give one back unpublished.
            Err(failure) => Err(failure),
        }
    }

    fn write_slot_and_publish(
        &mut self,
        slot: i32,
        buffer_kind: PipeWireCameraBufferKind,
        source: &TextureRegistration,
        timestamp_ns: i64,
    ) -> Result<()> {
        let slot_index = slot as usize;
        let offered = self.offered.as_mut().ok_or_else(|| {
            Error::Runtime("the camera's extent was withdrawn mid-frame".to_string())
        })?;
        let destination = offered.slot_textures.get(slot_index).ok_or_else(|| {
            Error::Runtime(format!(
                "PipeWire named buffer slot {slot_index}, which was never offered"
            ))
        })?;

        // One descriptor-ring slot, always: `offscreen_render` submits and
        // waits, so the staged bindings never outlive the frame that staged
        // them.
        const ONLY_DESCRIPTOR_RING_SLOT: u32 = 0;
        self.compositor.compose_to_offscreen_texture(
            ONLY_DESCRIPTOR_RING_SLOT,
            destination,
            source.texture(),
            source.current_layout(),
            // The camera's buffers are allocated at the frame's own extent, so
            // every mode is the identity; naming one keeps a later extent race
            // from silently cropping.
            PresentScalingMode::Fit,
        )?;
        // The compose leaves the source sampled-ready whatever it arrived in,
        // and the registration is the cell every other consumer of this surface
        // barriers out of, so it has to learn where the compose left it.
        source.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);

        match buffer_kind {
            PipeWireCameraBufferKind::DmaBufImportedByTheConsumer => {
                self.settle_slot_for_an_importing_consumer(slot_index)?;
            }
            PipeWireCameraBufferKind::SharedMemoryCopy => {
                self.copy_slot_into_shared_memory(slot, slot_index)?;
            }
            PipeWireCameraBufferKind::NothingNegotiated => {}
        }

        // SAFETY: the source is live and `slot` is the one this call dequeued.
        let queued = unsafe {
            video_shim::streamlib_pipewire_video_source_queue_slot(
                self.video_source.0,
                slot,
                timestamp_ns,
                self.published_frame_count,
            )
        };
        if queued != 0 {
            return Err(Error::Runtime(format!(
                "PipeWire refused buffer slot {slot_index}"
            )));
        }
        Ok(())
    }

    /// Bring a composed slot to rest in `GENERAL`, which is where the engine
    /// leaves every image another API reads through its DMA-BUF.
    fn settle_slot_for_an_importing_consumer(&mut self, slot_index: usize) -> Result<()> {
        let Self {
            offered, recorder, ..
        } = self;
        let destination = &offered
            .as_ref()
            .ok_or_else(withdrawn_mid_frame)?
            .slot_textures[slot_index];
        record_one_submission(recorder, |recorder| {
            recorder.record_image_barrier(
                destination,
                VulkanLayout::COLOR_ATTACHMENT_OPTIMAL,
                VulkanLayout::GENERAL,
                VulkanStage::COLOR_ATTACHMENT_OUTPUT,
                VulkanStage::ALL_COMMANDS,
                VulkanAccess::COLOR_ATTACHMENT_WRITE,
                VulkanAccess::MEMORY_READ,
            )
        })
    }

    /// Where the shim's shared-memory sibling maps this slot.
    fn shim_shared_memory_range_of(
        &self,
        slot: i32,
        slot_index: usize,
    ) -> Result<(*mut u8, usize)> {
        let mut stride_bytes = 0_u32;
        let mut byte_size = 0_u32;
        // SAFETY: the source is live and the two out-parameters are this
        // frame's own locals.
        let mapping = unsafe {
            video_shim::streamlib_pipewire_video_source_slot_shared_memory(
                self.video_source.0,
                slot,
                &mut stride_bytes,
                &mut byte_size,
            )
        };
        if mapping.is_null() || byte_size == 0 {
            return Err(Error::Runtime(format!(
                "the shared-memory sibling mapped nothing for buffer slot {slot_index}"
            )));
        }
        Ok((mapping, byte_size as usize))
    }

    /// Bytes `ImageCopyRegion::tightly_packed` will write for this extent.
    ///
    /// Computed in 64 bits and compared against what the shim mapped, because
    /// the shim sizes its buffer in `uint32_t`: an extent whose packed size
    /// wrapped would map less than the copy writes, and the overrun would be a
    /// device-side write past the mapping rather than a refused frame.
    fn tightly_packed_byte_len(width: u32, height: u32) -> Option<usize> {
        u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .and_then(|bytes| usize::try_from(bytes).ok())
    }

    /// Read a composed slot back into the shim's shared-memory buffer, through
    /// host-cached memory the GPU writes — never the write-combined kind, off
    /// which a read costs tens of milliseconds a frame.
    fn copy_slot_into_shared_memory(&mut self, slot: i32, slot_index: usize) -> Result<()> {
        let (host_range_ptr, host_range_byte_len) =
            self.shim_shared_memory_range_of(slot, slot_index)?;

        let Self {
            offered,
            recorder,
            gpu,
            camera_name,
            ..
        } = self;
        let offered = offered.as_mut().ok_or_else(withdrawn_mid_frame)?;
        let (width, height) = (offered.width, offered.height);
        if Self::tightly_packed_byte_len(width, height)
            .is_none_or(|needed| needed > host_range_byte_len)
        {
            return Err(Error::Runtime(format!(
                "VirtualCameraSink \"{camera_name}\": the shared-memory sibling mapped \
                 {host_range_byte_len} bytes for buffer slot {slot_index}, less than a tightly \
                 packed {width}x{height} RGBA picture writes"
            )));
        }
        let slot_cache = &mut offered.shared_memory_written_by_gpu[slot_index];

        if slot_cache.is_none() {
            let written_by_gpu = gpu.escalate(|full| {
                // SAFETY: the shim's mappings live exactly as long as the
                // offered extent — `set_extent` is the only thing that frees
                // them, and this value is dropped with the extent it belongs
                // to, before that call. The check below is what makes the
                // claim observable rather than assumed.
                unsafe {
                    full.import_host_mapping_for_gpu_writes(host_range_ptr, host_range_byte_len)
                }
            })?;
            tracing::info!(
                camera = %camera_name,
                slot = slot_index,
                tier = written_by_gpu.tier().as_str(),
                reason = written_by_gpu
                    .fallback_reason()
                    .unwrap_or("the driver imported the shared-memory mapping"),
                "VirtualCameraSink: the shared-memory sibling writes through this tier"
            );
            *slot_cache = Some(SharedMemorySlotWrittenByGpu {
                host_range_address: host_range_ptr as usize,
                host_range_byte_len,
                written_by_gpu,
            });
        }
        let cached = slot_cache.as_mut().ok_or_else(withdrawn_mid_frame)?;
        if cached.host_range_address != host_range_ptr as usize
            || cached.host_range_byte_len != host_range_byte_len
        {
            // Refused rather than re-imported: freeing the old import would
            // hand the driver a range this process may already have unmapped.
            return Err(Error::Runtime(format!(
                "VirtualCameraSink \"{camera_name}\": the shared-memory sibling moved buffer \
                 slot {slot_index} while its extent stood, so the range the GPU was told to \
                 write is no longer the one PipeWire hands the consumer"
            )));
        }

        let destination = &offered.slot_textures[slot_index];
        let written_by_gpu = &mut cached.written_by_gpu;
        record_one_submission(recorder, |recorder| {
            recorder.record_image_barrier(
                destination,
                VulkanLayout::COLOR_ATTACHMENT_OPTIMAL,
                VulkanLayout::TRANSFER_SRC_OPTIMAL,
                VulkanStage::COLOR_ATTACHMENT_OUTPUT,
                VulkanStage::COPY,
                VulkanAccess::COLOR_ATTACHMENT_WRITE,
                VulkanAccess::TRANSFER_READ,
            )?;
            recorder.record_copy_image_to_buffer(
                destination,
                VulkanLayout::TRANSFER_SRC_OPTIMAL,
                written_by_gpu.storage_buffer(),
                ImageCopyRegion::tightly_packed(width, height),
            )?;
            written_by_gpu.record_release_to_host(recorder)
        })?;
        written_by_gpu.publish_to_host();
        Ok(())
    }
}

/// The refusal for a frame whose extent was withdrawn under it.
fn withdrawn_mid_frame() -> Error {
    Error::Runtime("the camera's extent was withdrawn mid-frame".to_string())
}

/// Record one submission and wait for it, abandoning a half-recorded buffer so
/// the next frame can begin again.
fn record_one_submission(
    recorder: &mut RhiCommandRecorder,
    record: impl FnOnce(&mut RhiCommandRecorder) -> Result<()>,
) -> Result<()> {
    recorder.begin()?;
    match record(recorder).and_then(|()| recorder.submit_and_wait()) {
        Ok(()) => Ok(()),
        Err(failure) => {
            recorder.abort_recording();
            Err(failure)
        }
    }
}

impl Drop for PipeWireCameraNode {
    fn drop(&mut self) {
        // SAFETY: the source was produced by `open` and is closed exactly once.
        unsafe { video_shim::streamlib_pipewire_video_source_close(self.video_source.0) };
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::c_char;

    use super::*;

    fn node_name_of(camera_name: &str) -> String {
        let camera_name = CString::new(camera_name).expect("no NUL in a test name");
        let mut node_name = [0_u8; 128];
        let written = unsafe {
            video_shim::streamlib_pipewire_video_source_node_name(
                camera_name.as_ptr(),
                node_name.as_mut_ptr().cast::<c_char>(),
                node_name.len(),
            )
        };
        String::from_utf8(node_name[..written].to_vec()).expect("an ASCII identifier")
    }

    /// `node.name` is an identifier a session manager and every `pw-dump`
    /// reader index by, so a camera called "Desk cam" has to reduce to one
    /// rather than carry a space into the graph.
    #[test]
    fn a_camera_name_reduces_to_a_pipewire_identifier() {
        assert_eq!(node_name_of("Desk cam"), "streamlib-camera-desk-cam");
        assert_eq!(
            node_name_of("StreamLib Camera 4f2a"),
            "streamlib-camera-streamlib-camera-4f2a"
        );
    }

    /// Runs of punctuation collapse and trailing ones are dropped, so two names
    /// that differ only in padding do not become two different node names.
    #[test]
    fn punctuation_collapses_rather_than_repeating() {
        assert_eq!(node_name_of("Desk  cam!!"), "streamlib-camera-desk-cam");
        assert_eq!(node_name_of("Desk-cam"), "streamlib-camera-desk-cam");
        assert_eq!(node_name_of("!!!"), "streamlib-camera");
    }

    /// The two properties WirePlumber's portal access rule keys on. Without
    /// both, the node is one no portal-based picker will ever list — a camera
    /// that exists in `pw-dump` and nowhere a user can see.
    ///
    /// Mental revert: drop `media.role` and this fails.
    #[test]
    fn a_camera_node_announces_the_class_and_role_a_portal_grants() {
        let camera_name = CString::new("Desk cam").expect("no NUL");
        let mut node_name = [0_u8; 128];
        let mut items = [video_shim::StreamProperty {
            key: std::ptr::null(),
            value: std::ptr::null(),
        }; 6];
        let count = unsafe {
            video_shim::streamlib_pipewire_video_source_properties(
                items.as_mut_ptr(),
                items.len() as u32,
                camera_name.as_ptr(),
                node_name.as_mut_ptr().cast::<c_char>(),
                node_name.len(),
            )
        };
        assert_eq!(count, 6, "every property the portal rule and a picker read");

        let announced: Vec<(String, String)> = items[..count as usize]
            .iter()
            .map(|item| unsafe {
                (
                    CStr::from_ptr(item.key).to_string_lossy().into_owned(),
                    CStr::from_ptr(item.value).to_string_lossy().into_owned(),
                )
            })
            .collect();
        assert!(announced.contains(&("media.class".to_string(), "Video/Source".to_string())));
        assert!(announced.contains(&("media.role".to_string(), "Camera".to_string())));
        assert!(
            announced.contains(&("node.description".to_string(), "Desk cam".to_string())),
            "the name a picker shows is the camera's own: {announced:?}"
        );
        assert!(
            announced.contains(&(
                "node.name".to_string(),
                "streamlib-camera-desk-cam".to_string()
            )),
            "the identifier is the reduced name: {announced:?}"
        );
    }

    fn describe_offer(fixated: bool) -> video_shim::OfferReport {
        let offered_format = video_shim::OfferedFormat {
            width: 1280,
            height: 720,
            framerate_numerator: 30,
            framerate_denominator: 1,
            drm_modifier: 0x0300_0000_0000_0013,
        };
        unsafe {
            video_shim::streamlib_pipewire_video_source_describe_offer(&offered_format, fixated)
        }
    }

    /// The offer is one tiled DMA-BUF format beside one shared-memory sibling:
    /// the modifier property is mandatory so a consumer that cannot take it
    /// rejects the pod rather than dropping the property and importing the
    /// engine's tiled memory as linear, and it is left unfixated so the
    /// consumer answers with what it can import.
    ///
    /// Mental revert: drop `DONT_FIXATE` and the consumer never gets to choose;
    /// drop `MANDATORY` and it can silently ignore the tiling.
    #[test]
    fn a_pipewire_camera_node_offers_a_modifier_and_a_shared_memory_sibling() {
        let report = describe_offer(false);
        assert!(
            report.both_formats_were_built,
            "both pods build and parse back: {report:?}"
        );
        assert_eq!((report.width, report.height), (1280, 720));
        assert_eq!(
            report.dma_buf_modifier_count, 1,
            "one modifier is offered, because one is what was allocated: {report:?}"
        );
        assert_eq!(report.dma_buf_modifier, 0x0300_0000_0000_0013);
        assert!(report.dma_buf_modifier_is_mandatory, "{report:?}");
        assert!(report.dma_buf_modifier_may_not_be_fixated, "{report:?}");
        assert!(
            !report.shared_memory_format_carries_a_modifier,
            "the sibling is the same picture with no modifier at all: {report:?}"
        );
    }

    /// The negotiation's second half: once the consumer has answered, the same
    /// modifier is re-offered as one fixed value rather than a choice, which is
    /// what lets PipeWire allocate.
    #[test]
    fn the_fixated_offer_carries_the_modifier_without_the_dont_fixate_flag() {
        let report = describe_offer(true);
        assert!(report.both_formats_were_built, "{report:?}");
        assert_eq!(report.dma_buf_modifier, 0x0300_0000_0000_0013);
        assert!(report.dma_buf_modifier_is_mandatory, "{report:?}");
        assert!(
            !report.dma_buf_modifier_may_not_be_fixated,
            "a fixated offer is the one the consumer no longer chooses from: {report:?}"
        );
    }
}
