// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `VkVideoSessionKHR` + `VkVideoSessionParametersKHR` lifecycle, with the
//! session's multi-binding device-memory bind. A codec holds
//! `Arc<VulkanVideoSession>` and `Arc<VulkanVideoSessionParameters>` and never
//! calls `vkCreateVideoSessionKHR` / `vkBindVideoSessionMemoryKHR` /
//! `vkCreateVideoSessionParametersKHR` itself.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrVideoQueueExtensionDeviceCommands as _;
use vulkanalia_vma::{self as vma, Alloc as _};

use crate::{VideoError, VideoResult};

use crate::VulkanVideoCodecDevice;

/// Upper bound on memory bindings per video session. Mirrors nvpro-
/// samples' `MAX_BOUND_MEMORY = 40`; observed driver caps stay well
/// below this on every Linux platform we exercise.
const MAX_BOUND_MEMORY: usize = 40;

/// What `vkCreateVideoSessionKHR` refusing with `refusal` means for the
/// session labelled `session_label`: `VK_ERROR_TOO_MANY_OBJECTS` is the
/// driver's limit on video sessions, which frees as other sessions end.
fn video_session_creation_refusal(session_label: &str, refusal: vk::ErrorCode) -> VideoError {
    if refusal == vk::ErrorCode::TOO_MANY_OBJECTS {
        return VideoError::VideoSessionLimitReached {
            session_label: session_label.to_owned(),
        };
    }
    VideoError::Rhi(format!(
        "video session '{session_label}': create_video_session_khr failed: {refusal}"
    ))
}

/// Every Vulkan Video decode codec operation the engine knows.
pub const VIDEO_DECODE_CODEC_OPERATIONS: vk::VideoCodecOperationFlagsKHR =
    vk::VideoCodecOperationFlagsKHR::from_bits_truncate(
        vk::VideoCodecOperationFlagsKHR::DECODE_H264.bits()
            | vk::VideoCodecOperationFlagsKHR::DECODE_H265.bits()
            | vk::VideoCodecOperationFlagsKHR::DECODE_AV1.bits()
            | vk::VideoCodecOperationFlagsKHR::DECODE_VP9.bits(),
    );

/// Every Vulkan Video encode codec operation the engine knows.
pub const VIDEO_ENCODE_CODEC_OPERATIONS: vk::VideoCodecOperationFlagsKHR =
    vk::VideoCodecOperationFlagsKHR::from_bits_truncate(
        vk::VideoCodecOperationFlagsKHR::ENCODE_H264.bits()
            | vk::VideoCodecOperationFlagsKHR::ENCODE_H265.bits()
            | vk::VideoCodecOperationFlagsKHR::ENCODE_AV1.bits(),
    );

// ----------------------------------------------------------------------------
// Codec-operation direction
// ----------------------------------------------------------------------------

/// Whether a Vulkan Video codec operation decodes or encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodecOperationDirection {
    /// A `VK_KHR_video_decode_queue` operation.
    Decode,
    /// A `VK_KHR_video_encode_queue` operation.
    Encode,
}

impl VideoCodecOperationDirection {
    /// Direction of `codec_operation`; `None` when it names no known
    /// operation or mixes decode and encode bits.
    pub fn of_codec_operation(codec_operation: vk::VideoCodecOperationFlagsKHR) -> Option<Self> {
        let decodes = codec_operation.intersects(VIDEO_DECODE_CODEC_OPERATIONS);
        let encodes = codec_operation.intersects(VIDEO_ENCODE_CODEC_OPERATIONS);
        match (decodes, encodes) {
            (true, false) => Some(Self::Decode),
            (false, true) => Some(Self::Encode),
            _ => None,
        }
    }

    /// Lower-case direction name, as error messages spell it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Decode => "decode",
            Self::Encode => "encode",
        }
    }
}

/// The Vulkan Video directions a device enabled at bring-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeviceVideoCodingSupport {
    decode: bool,
    encode: bool,
}

impl DeviceVideoCodingSupport {
    fn of_device(vulkan_device: &dyn VulkanVideoCodecDevice) -> Self {
        Self {
            decode: crate::vulkan_video_codec_device::supports_video_decode(vulkan_device),
            encode: crate::vulkan_video_codec_device::supports_video_encode(vulkan_device),
        }
    }

    fn supports(self, direction: VideoCodecOperationDirection) -> bool {
        match direction {
            VideoCodecOperationDirection::Decode => self.decode,
            VideoCodecOperationDirection::Encode => self.encode,
        }
    }
}

/// Refuses a codec operation the device did not enable. An unenabled
/// `VK_KHR_video_*` leaves its commands as vulkanalia's panicking
/// stubs, so this must run before any video entry point is called.
fn require_device_video_coding_support(
    codec_operation: vk::VideoCodecOperationFlagsKHR,
    device_support: DeviceVideoCodingSupport,
) -> VideoResult<VideoCodecOperationDirection> {
    let direction =
        VideoCodecOperationDirection::of_codec_operation(codec_operation).ok_or_else(|| {
            VideoError::Rhi(format!(
                "video session: codec_operation {codec_operation:?} is not a single-direction \
                 Vulkan Video operation",
            ))
        })?;
    if device_support.supports(direction) {
        Ok(direction)
    } else {
        Err(VideoError::Rhi(format!(
            "video session: device does not support Vulkan Video {} \
             (codec_operation {codec_operation:?})",
            direction.name(),
        )))
    }
}

// ----------------------------------------------------------------------------
// Descriptors
// ----------------------------------------------------------------------------

/// Inputs for [`VulkanVideoSession`] construction. The
/// `video_profile` is consumed as a fully-built
/// [`vk::VideoProfileInfoKHR`] — caller chains any codec-specific
/// extension structs (`VkVideoDecodeH264ProfileInfoKHR`,
/// `VkVideoEncodeH265ProfileInfoKHR`, etc.) onto its `pNext` and
/// owns their lifetime until [`VulkanVideoSession::new`] returns.
#[derive(Clone)]
pub struct VideoSessionDescriptor<'a> {
    pub label: &'a str,
    pub session_create_flags: vk::VideoSessionCreateFlagsKHR,
    pub video_queue_family: u32,
    pub video_profile: vk::VideoProfileInfoKHR,
    pub codec_operation: vk::VideoCodecOperationFlagsKHR,
    pub picture_format: vk::Format,
    pub max_coded_extent: vk::Extent2D,
    pub reference_pictures_format: vk::Format,
    pub max_dpb_slots: u32,
    pub max_active_reference_pictures: u32,
}

/// Codec-specific add-info for [`VulkanVideoSessionParameters`].
/// Each variant carries typed slices of the std-video parameter sets
/// the codec layer already builds at the call site. Adding AV1 / VP9
/// is a new variant — the typed enum is the deliberate choice over
/// erased `Box<dyn ExtendsVideoSessionParametersCreateInfoKHR>` so
/// the engine API stays ergonomic for known codecs and the
/// std-video struct lifetimes remain scoped to the descriptor.
///
/// Lifetimes: the slices reference caller-owned arrays of std-video
/// parameter sets. Those arrays (and any inner `pNext`-referenced
/// VUI / HRD structs the SPS reaches via raw pointer) must outlive
/// the call to [`VulkanVideoSessionParameters::new`].
#[derive(Clone)]
pub enum VideoSessionParametersAddInfo<'a> {
    DecodeH264 {
        sps: &'a [vk::video::StdVideoH264SequenceParameterSet],
        pps: &'a [vk::video::StdVideoH264PictureParameterSet],
        max_std_sps_count: u32,
        max_std_pps_count: u32,
    },
    DecodeH265 {
        vps: &'a [vk::video::StdVideoH265VideoParameterSet],
        sps: &'a [vk::video::StdVideoH265SequenceParameterSet],
        pps: &'a [vk::video::StdVideoH265PictureParameterSet],
        max_std_vps_count: u32,
        max_std_sps_count: u32,
        max_std_pps_count: u32,
    },
    EncodeH264 {
        sps: &'a [vk::video::StdVideoH264SequenceParameterSet],
        pps: &'a [vk::video::StdVideoH264PictureParameterSet],
        max_std_sps_count: u32,
        max_std_pps_count: u32,
    },
    EncodeH265 {
        vps: &'a [vk::video::StdVideoH265VideoParameterSet],
        sps: &'a [vk::video::StdVideoH265SequenceParameterSet],
        pps: &'a [vk::video::StdVideoH265PictureParameterSet],
        max_std_vps_count: u32,
        max_std_sps_count: u32,
        max_std_pps_count: u32,
    },
}

/// Inputs for [`VulkanVideoSessionParameters`] construction.
#[derive(Clone)]
pub struct VideoSessionParametersDescriptor<'a> {
    pub label: &'a str,
    pub add_info: VideoSessionParametersAddInfo<'a>,
    /// Quality level for encoder sessions. When `Some(level)` and
    /// `level > 0`, chains [`vk::VideoEncodeQualityLevelInfoKHR`] onto
    /// the create info — required by `VUID-vkCmdEncodeVideoKHR-None-08318`.
    /// Ignored for decode parameters.
    pub quality_level: Option<u32>,
    /// Optional template-parameters source. When set, the new
    /// parameters object inherits the parameter sets of the template,
    /// matching `VkVideoSessionParametersCreateInfoKHR::videoSessionParametersTemplate`.
    pub template: Option<&'a Arc<VulkanVideoSessionParameters>>,
}

// ----------------------------------------------------------------------------
// VulkanVideoSession
// ----------------------------------------------------------------------------

/// Snapshot of the create-info fields required by
/// [`VulkanVideoSession::is_compatible`]. Stored as scalars
/// because we cannot retain pointers into stack-allocated Vulkan
/// builder structs after construction returns.
#[derive(Clone)]
struct VideoSessionCreateInfoSnapshot {
    queue_family_index: u32,
    picture_format: vk::Format,
    max_coded_extent: vk::Extent2D,
    reference_picture_format: vk::Format,
    max_dpb_slots: u32,
    max_active_reference_pictures: u32,
}

/// Privileged RHI handle for a `VkVideoSessionKHR` plus its bound
/// device memory. Mirrors [`crate::VulkanVideoDpbImage`] / [`crate::VulkanVideoBitstreamBuffer`]
/// in shape: an `Arc<dyn VulkanVideoCodecDevice>` backref, the raw Vulkan
/// handle, the VMA allocations the driver demanded, and a `Drop`
/// impl that tears them down in the right order.
pub struct VulkanVideoSession {
    vulkan_device: Arc<dyn VulkanVideoCodecDevice>,
    handle: vk::VideoSessionKHR,
    allocations: Vec<vma::Allocation>,
    create_info_snapshot: VideoSessionCreateInfoSnapshot,
    flags: vk::VideoSessionCreateFlagsKHR,
}

// SAFETY: the inner Vulkan handle is only mutated on construction and
// `Drop`; all reads go through `&self`. The `Arc<dyn VulkanVideoCodecDevice>`
// owns the per-queue mutexes Vulkan needs for external sync.
unsafe impl Send for VulkanVideoSession {}
unsafe impl Sync for VulkanVideoSession {}

impl VulkanVideoSession {
    pub fn new(
        vulkan_device: &Arc<dyn VulkanVideoCodecDevice>,
        descriptor: &VideoSessionDescriptor<'_>,
    ) -> VideoResult<Arc<Self>> {
        tracing::debug!(
            rhi_op = "create_video_session",
            label = descriptor.label,
            codec_op = ?descriptor.codec_operation,
            picture_format = ?descriptor.picture_format,
            max_coded_extent_w = descriptor.max_coded_extent.width,
            max_coded_extent_h = descriptor.max_coded_extent.height,
            max_dpb_slots = descriptor.max_dpb_slots,
            max_active_reference_pictures = descriptor.max_active_reference_pictures,
            "VulkanVideoSession::new"
        );

        require_device_video_coding_support(
            descriptor.codec_operation,
            DeviceVideoCodingSupport::of_device(vulkan_device.as_ref()),
        )?;
        let std_header_version = std_header_version_for_codec(descriptor.codec_operation)?;
        let profile_info = descriptor.video_profile;

        let create_info = vk::VideoSessionCreateInfoKHR::builder()
            .flags(descriptor.session_create_flags)
            .video_profile(&profile_info)
            .queue_family_index(descriptor.video_queue_family)
            .picture_format(descriptor.picture_format)
            .max_coded_extent(descriptor.max_coded_extent)
            .max_dpb_slots(descriptor.max_dpb_slots)
            .max_active_reference_pictures(descriptor.max_active_reference_pictures)
            .reference_picture_format(descriptor.reference_pictures_format)
            .std_header_version(&std_header_version);

        let device = vulkan_device.device();
        let allocator = vulkan_device.allocator();

        let _device_lock = vulkan_device.lock_device();

        let raw_session = unsafe { device.create_video_session_khr(&create_info, None) }
            .map_err(|refusal| video_session_creation_refusal(descriptor.label, refusal))?;

        let bind_result =
            unsafe { Self::allocate_and_bind_memory(device, allocator, raw_session, descriptor) };
        let allocations = match bind_result {
            Ok(a) => a,
            Err(e) => {
                unsafe { device.destroy_video_session_khr(raw_session, None) };
                return Err(e);
            }
        };

        let snapshot = VideoSessionCreateInfoSnapshot {
            queue_family_index: descriptor.video_queue_family,
            picture_format: descriptor.picture_format,
            max_coded_extent: descriptor.max_coded_extent,
            reference_picture_format: descriptor.reference_pictures_format,
            max_dpb_slots: descriptor.max_dpb_slots,
            max_active_reference_pictures: descriptor.max_active_reference_pictures,
        };

        Ok(Arc::new(Self {
            vulkan_device: Arc::clone(vulkan_device),
            handle: raw_session,
            allocations,
            create_info_snapshot: snapshot,
            flags: descriptor.session_create_flags,
        }))
    }

    /// Raw `VkVideoSessionKHR` handle. Caller may pass it into codec
    /// extension commands (`vkCmdDecodeVideoKHR`,
    /// `vkCmdBeginVideoCodingKHR`, ...) but must NOT destroy it; the
    /// Arc's `Drop` impl owns destruction.
    #[inline]
    pub fn handle(&self) -> vk::VideoSessionKHR {
        self.handle
    }

    /// Host device backref. Used by
    /// [`VulkanVideoSessionParameters`] and by codec-side
    /// callers that need to reach `vulkanalia::Device` /
    /// `lock_device` without threading another field through.
    #[inline]
    pub fn device(&self) -> &Arc<dyn VulkanVideoCodecDevice> {
        &self.vulkan_device
    }

    /// Whether this session can be reused for a new picture with
    /// the given descriptor. Mirrors nvpro-samples'
    /// `VulkanVideoSession::IsCompatible` — the caller can skip
    /// session re-creation when this returns `true`.
    pub fn is_compatible(&self, descriptor: &VideoSessionDescriptor<'_>) -> bool {
        snapshot_is_compatible(&self.create_info_snapshot, self.flags, descriptor)
    }

    /// # Safety
    ///
    /// Caller must hold the device resource lock and have just
    /// successfully created `raw_session` against `device`.
    unsafe fn allocate_and_bind_memory(
        device: &vulkanalia::Device,
        allocator: &Arc<vma::Allocator>,
        raw_session: vk::VideoSessionKHR,
        descriptor: &VideoSessionDescriptor<'_>,
    ) -> VideoResult<Vec<vma::Allocation>> {
        let mem_requirements =
            unsafe { device.get_video_session_memory_requirements_khr(raw_session) }.map_err(
                |e| {
                    VideoError::Rhi(format!(
                        "video session '{}': get_video_session_memory_requirements_khr failed: {e}",
                        descriptor.label,
                    ))
                },
            )?;

        if mem_requirements.len() > MAX_BOUND_MEMORY {
            return Err(VideoError::Rhi(format!(
                "video session '{}': driver requested {} memory bindings, max is {}",
                descriptor.label,
                mem_requirements.len(),
                MAX_BOUND_MEMORY,
            )));
        }

        let mut allocations: Vec<vma::Allocation> = Vec::with_capacity(mem_requirements.len());
        let mut bind_infos: Vec<vk::BindVideoSessionMemoryInfoKHR> =
            Vec::with_capacity(mem_requirements.len());

        for req in mem_requirements.iter() {
            let memory_type_bits = req.memory_requirements.memory_type_bits;
            if memory_type_bits == 0 {
                Self::free_partial(allocator, &mut allocations);
                return Err(VideoError::Rhi(format!(
                    "video session '{}': zero memory_type_bits returned for binding {}",
                    descriptor.label, req.memory_bind_index,
                )));
            }

            let alloc_options = vma::AllocationOptions {
                usage: vma::MemoryUsage::Unknown,
                memory_type_bits,
                ..Default::default()
            };

            let allocation =
                unsafe { allocator.allocate_memory(req.memory_requirements, &alloc_options) }
                    .map_err(|e| {
                        Self::free_partial(allocator, &mut allocations);
                        VideoError::Rhi(format!(
                            "video session '{}': allocate_memory for binding {} failed: {e}",
                            descriptor.label, req.memory_bind_index,
                        ))
                    })?;

            let alloc_info = allocator.get_allocation_info(allocation);
            allocations.push(allocation);

            bind_infos.push(
                vk::BindVideoSessionMemoryInfoKHR::builder()
                    .memory_bind_index(req.memory_bind_index)
                    .memory(alloc_info.deviceMemory)
                    .memory_offset(alloc_info.offset)
                    .memory_size(req.memory_requirements.size)
                    .build(),
            );
        }

        unsafe { device.bind_video_session_memory_khr(raw_session, &bind_infos) }.map_err(|e| {
            Self::free_partial(allocator, &mut allocations);
            VideoError::Rhi(format!(
                "video session '{}': bind_video_session_memory_khr failed: {e}",
                descriptor.label,
            ))
        })?;

        Ok(allocations)
    }

    fn free_partial(allocator: &Arc<vma::Allocator>, allocations: &mut Vec<vma::Allocation>) {
        for allocation in allocations.drain(..) {
            unsafe { allocator.free_memory(allocation) };
        }
    }
}

impl Drop for VulkanVideoSession {
    fn drop(&mut self) {
        let device = self.vulkan_device.device();
        if self.handle != vk::VideoSessionKHR::null() {
            unsafe { device.destroy_video_session_khr(self.handle, None) };
            self.handle = vk::VideoSessionKHR::null();
        }
        let allocator = self.vulkan_device.allocator();
        for allocation in self.allocations.drain(..) {
            unsafe { allocator.free_memory(allocation) };
        }
    }
}

// ----------------------------------------------------------------------------
// VulkanVideoSessionParameters
// ----------------------------------------------------------------------------

/// Privileged RHI handle for a `VkVideoSessionParametersKHR`. Holds
/// a strong reference to the parent
/// [`VulkanVideoSession`]; Vulkan spec requires the session to
/// outlive any of its parameters objects, and the `Arc` makes that
/// invariant unbreakable from Rust.
pub struct VulkanVideoSessionParameters {
    session: Arc<VulkanVideoSession>,
    handle: vk::VideoSessionParametersKHR,
}

unsafe impl Send for VulkanVideoSessionParameters {}
unsafe impl Sync for VulkanVideoSessionParameters {}

impl VulkanVideoSessionParameters {
    pub fn new(
        session: &Arc<VulkanVideoSession>,
        descriptor: &VideoSessionParametersDescriptor<'_>,
    ) -> VideoResult<Arc<Self>> {
        tracing::debug!(
            rhi_op = "create_video_session_parameters",
            label = descriptor.label,
            quality_level = descriptor.quality_level.unwrap_or(0),
            has_template = descriptor.template.is_some(),
            "VulkanVideoSessionParameters::new"
        );

        let device = session.vulkan_device.device();
        let session_handle = session.handle;
        let template_handle = descriptor
            .template
            .map(|t| t.handle)
            .unwrap_or(vk::VideoSessionParametersKHR::null());

        let mut params_create =
            vk::VideoSessionParametersCreateInfoKHR::builder().video_session(session_handle);
        if template_handle != vk::VideoSessionParametersKHR::null() {
            params_create = params_create.video_session_parameters_template(template_handle);
        }

        // Codec-specific add-info structs must outlive the
        // `create_video_session_parameters_khr` call. Declared in
        // outer scope so raw-pointer chains inside the builder stay
        // valid. The `_params` ones must be `mut` because `push_next`
        // takes `&mut`; the `_add` ones are read by reference only.
        let h264_dec_add;
        let mut h264_dec_params;
        let h265_dec_add;
        let mut h265_dec_params;
        let h264_enc_add;
        let mut h264_enc_params;
        let h265_enc_add;
        let mut h265_enc_params;
        let mut quality_level_info;

        match &descriptor.add_info {
            VideoSessionParametersAddInfo::DecodeH264 {
                sps,
                pps,
                max_std_sps_count,
                max_std_pps_count,
            } => {
                h264_dec_add = vk::VideoDecodeH264SessionParametersAddInfoKHR::builder()
                    .std_sp_ss(sps)
                    .std_pp_ss(pps);
                h264_dec_params = vk::VideoDecodeH264SessionParametersCreateInfoKHR::builder()
                    .max_std_sps_count(*max_std_sps_count)
                    .max_std_pps_count(*max_std_pps_count)
                    .parameters_add_info(&h264_dec_add);
                params_create = params_create.push_next(&mut h264_dec_params);
            }
            VideoSessionParametersAddInfo::DecodeH265 {
                vps,
                sps,
                pps,
                max_std_vps_count,
                max_std_sps_count,
                max_std_pps_count,
            } => {
                h265_dec_add = vk::VideoDecodeH265SessionParametersAddInfoKHR::builder()
                    .std_vp_ss(vps)
                    .std_sp_ss(sps)
                    .std_pp_ss(pps);
                h265_dec_params = vk::VideoDecodeH265SessionParametersCreateInfoKHR::builder()
                    .max_std_vps_count(*max_std_vps_count)
                    .max_std_sps_count(*max_std_sps_count)
                    .max_std_pps_count(*max_std_pps_count)
                    .parameters_add_info(&h265_dec_add);
                params_create = params_create.push_next(&mut h265_dec_params);
            }
            VideoSessionParametersAddInfo::EncodeH264 {
                sps,
                pps,
                max_std_sps_count,
                max_std_pps_count,
            } => {
                h264_enc_add = vk::VideoEncodeH264SessionParametersAddInfoKHR::builder()
                    .std_sp_ss(sps)
                    .std_pp_ss(pps);
                h264_enc_params = vk::VideoEncodeH264SessionParametersCreateInfoKHR::builder()
                    .max_std_sps_count(*max_std_sps_count)
                    .max_std_pps_count(*max_std_pps_count)
                    .parameters_add_info(&h264_enc_add);
                params_create = params_create.push_next(&mut h264_enc_params);
            }
            VideoSessionParametersAddInfo::EncodeH265 {
                vps,
                sps,
                pps,
                max_std_vps_count,
                max_std_sps_count,
                max_std_pps_count,
            } => {
                h265_enc_add = vk::VideoEncodeH265SessionParametersAddInfoKHR::builder()
                    .std_vp_ss(vps)
                    .std_sp_ss(sps)
                    .std_pp_ss(pps);
                h265_enc_params = vk::VideoEncodeH265SessionParametersCreateInfoKHR::builder()
                    .max_std_vps_count(*max_std_vps_count)
                    .max_std_sps_count(*max_std_sps_count)
                    .max_std_pps_count(*max_std_pps_count)
                    .parameters_add_info(&h265_enc_add);
                params_create = params_create.push_next(&mut h265_enc_params);
            }
        }

        if let Some(level) = descriptor.quality_level
            && level > 0
        {
            quality_level_info = vk::VideoEncodeQualityLevelInfoKHR::builder().quality_level(level);
            params_create = params_create.push_next(&mut quality_level_info);
        }

        let handle = unsafe {
            device.create_video_session_parameters_khr(&params_create, None)
        }
        .map_err(|e| VideoError::Rhi(format!(
            "video session parameters '{}': create_video_session_parameters_khr failed: {e}",
            descriptor.label,
        )))?;

        Ok(Arc::new(Self {
            session: Arc::clone(session),
            handle,
        }))
    }

    /// Raw `VkVideoSessionParametersKHR` handle. Caller passes it
    /// into codec extension commands (`vkCmdBeginVideoCodingKHR`'s
    /// `videoSessionParameters` field, encode-feedback queries,
    /// etc.) but must NOT destroy it.
    #[inline]
    pub fn handle(&self) -> vk::VideoSessionParametersKHR {
        self.handle
    }

    /// The session this parameters object is parented to.
    #[inline]
    pub fn session(&self) -> &Arc<VulkanVideoSession> {
        &self.session
    }
}

impl Drop for VulkanVideoSessionParameters {
    fn drop(&mut self) {
        let device = self.session.vulkan_device.device();
        if self.handle != vk::VideoSessionParametersKHR::null() {
            unsafe {
                device.destroy_video_session_parameters_khr(self.handle, None);
            }
            self.handle = vk::VideoSessionParametersKHR::null();
        }
    }
}

/// Pure compatibility predicate over the cached snapshot + descriptor.
/// `VulkanVideoSession::is_compatible` delegates here so the rule
/// table is unit-testable without a real Vulkan device.
fn snapshot_is_compatible(
    snapshot: &VideoSessionCreateInfoSnapshot,
    flags: vk::VideoSessionCreateFlagsKHR,
    descriptor: &VideoSessionDescriptor<'_>,
) -> bool {
    if descriptor.session_create_flags != flags {
        return false;
    }
    if descriptor.max_coded_extent.width > snapshot.max_coded_extent.width {
        return false;
    }
    if descriptor.max_coded_extent.height > snapshot.max_coded_extent.height {
        return false;
    }
    if descriptor.max_dpb_slots > snapshot.max_dpb_slots {
        return false;
    }
    if descriptor.max_active_reference_pictures > snapshot.max_active_reference_pictures {
        return false;
    }
    if descriptor.reference_pictures_format != snapshot.reference_picture_format {
        return false;
    }
    if descriptor.picture_format != snapshot.picture_format {
        return false;
    }
    if descriptor.video_queue_family != snapshot.queue_family_index {
        return false;
    }
    true
}

// ----------------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------------

/// `VkExtensionProperties` (std header version) for a given codec
/// operation. Constructs the values at runtime because the
/// `VK_STD_VULKAN_VIDEO_CODEC_*` defines are not exposed through
/// `vulkanalia` 0.35.
fn std_header_version_for_codec(
    codec_op: vk::VideoCodecOperationFlagsKHR,
) -> VideoResult<vk::ExtensionProperties> {
    fn make_ext(name: &[u8], spec_version: u32) -> vk::ExtensionProperties {
        vk::ExtensionProperties {
            extension_name: vk::StringArray::from_bytes(name),
            spec_version,
        }
    }
    // VK_MAKE_VIDEO_STD_VERSION(1, 0, 0) = (1 << 22).
    const STD_VIDEO_VERSION_1_0_0: u32 = 1 << 22;

    if codec_op == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
        Ok(make_ext(
            b"VK_STD_vulkan_video_codec_h264_decode\0",
            STD_VIDEO_VERSION_1_0_0,
        ))
    } else if codec_op == vk::VideoCodecOperationFlagsKHR::DECODE_H265 {
        Ok(make_ext(
            b"VK_STD_vulkan_video_codec_h265_decode\0",
            STD_VIDEO_VERSION_1_0_0,
        ))
    } else if codec_op == vk::VideoCodecOperationFlagsKHR::DECODE_AV1 {
        Ok(make_ext(
            b"VK_STD_vulkan_video_codec_av1_decode\0",
            STD_VIDEO_VERSION_1_0_0,
        ))
    } else if codec_op == vk::VideoCodecOperationFlagsKHR::DECODE_VP9 {
        Ok(make_ext(
            b"VK_STD_vulkan_video_codec_vp9_decode\0",
            STD_VIDEO_VERSION_1_0_0,
        ))
    } else if codec_op == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
        Ok(make_ext(
            b"VK_STD_vulkan_video_codec_h264_encode\0",
            STD_VIDEO_VERSION_1_0_0,
        ))
    } else if codec_op == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
        Ok(make_ext(
            b"VK_STD_vulkan_video_codec_h265_encode\0",
            STD_VIDEO_VERSION_1_0_0,
        ))
    } else if codec_op == vk::VideoCodecOperationFlagsKHR::ENCODE_AV1 {
        Ok(make_ext(
            b"VK_STD_vulkan_video_codec_av1_encode\0",
            STD_VIDEO_VERSION_1_0_0,
        ))
    } else {
        Err(VideoError::Rhi(format!(
            "unsupported codec_operation {:?} for video session creation",
            codec_op,
        )))
    }
}

// ============================================================================
// Tests — pure logic (no GPU)
// ============================================================================

#[cfg(test)]
mod tests;
