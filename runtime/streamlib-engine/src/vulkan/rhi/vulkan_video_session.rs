// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Privileged engine RHI primitive — `VkVideoSessionKHR` +
//! `VkVideoSessionParametersKHR` lifecycle.
//!
//! Owns the multi-binding device-memory dance every codec consumer
//! re-implemented inline pre-this-PR. Constructed only via
//! [`GpuContextFullAccess::create_video_session`](crate::core::context::GpuContextFullAccess::create_video_session)
//! / [`create_video_session_parameters`](crate::core::context::GpuContextFullAccess::create_video_session_parameters);
//! the codec layer holds `Arc<HostVulkanVideoSession>` and
//! `Arc<HostVulkanVideoSessionParameters>` and never reaches into raw
//! `vkCreateVideoSessionKHR` / `vkBindVideoSessionMemoryKHR`
//! / `vkCreateVideoSessionParametersKHR`.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrVideoQueueExtensionDeviceCommands as _;
use vulkanalia_vma::{self as vma, Alloc as _};

use crate::core::{Error, Result};

use super::HostVulkanDevice;

/// Upper bound on memory bindings per video session. Mirrors nvpro-
/// samples' `MAX_BOUND_MEMORY = 40`; observed driver caps stay well
/// below this on every Linux platform we exercise.
const MAX_BOUND_MEMORY: usize = 40;

// ----------------------------------------------------------------------------
// Descriptors
// ----------------------------------------------------------------------------

/// Inputs for [`HostVulkanVideoSession`] construction. The
/// `video_profile` is consumed as a fully-built
/// [`vk::VideoProfileInfoKHR`] — caller chains any codec-specific
/// extension structs (`VkVideoDecodeH264ProfileInfoKHR`,
/// `VkVideoEncodeH265ProfileInfoKHR`, etc.) onto its `pNext` and
/// owns their lifetime until [`HostVulkanVideoSession::new`] returns.
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

/// Codec-specific add-info for [`HostVulkanVideoSessionParameters`].
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
/// the call to
/// [`GpuContextFullAccess::create_video_session_parameters`](crate::core::context::GpuContextFullAccess::create_video_session_parameters).
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

/// Inputs for [`HostVulkanVideoSessionParameters`] construction.
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
    pub template: Option<&'a Arc<HostVulkanVideoSessionParameters>>,
}

// ----------------------------------------------------------------------------
// HostVulkanVideoSession
// ----------------------------------------------------------------------------

/// Snapshot of the create-info fields required by
/// [`HostVulkanVideoSession::is_compatible`]. Stored as scalars
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
/// device memory. Mirrors `HostVulkanTexture` / `HostVulkanBuffer`
/// in shape: an `Arc<HostVulkanDevice>` backref, the raw Vulkan
/// handle, the VMA allocations the driver demanded, and a `Drop`
/// impl that tears them down in the right order.
pub struct HostVulkanVideoSession {
    #[allow(dead_code)] // surfaced via tracing on construction; kept for future debug surfaces
    label: String,
    vulkan_device: Arc<HostVulkanDevice>,
    handle: vk::VideoSessionKHR,
    allocations: Vec<vma::Allocation>,
    create_info_snapshot: VideoSessionCreateInfoSnapshot,
    flags: vk::VideoSessionCreateFlagsKHR,
}

// SAFETY: the inner Vulkan handle is only mutated on construction and
// `Drop`; all reads go through `&self`. The `Arc<HostVulkanDevice>`
// owns the per-queue mutexes Vulkan needs for external sync.
unsafe impl Send for HostVulkanVideoSession {}
unsafe impl Sync for HostVulkanVideoSession {}

impl HostVulkanVideoSession {
    /// Build a new session. Runs `vkCreateVideoSessionKHR`,
    /// `vkGetVideoSessionMemoryRequirementsKHR`, VMA allocations for
    /// each requested binding, and `vkBindVideoSessionMemoryKHR` —
    /// all under the host's device-level resource lock so
    /// concurrent processor submissions on NVIDIA Linux cannot race.
    pub(crate) fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &VideoSessionDescriptor<'_>,
    ) -> Result<Arc<Self>> {
        tracing::debug!(
            rhi_op = "create_video_session",
            label = descriptor.label,
            codec_op = ?descriptor.codec_operation,
            picture_format = ?descriptor.picture_format,
            max_coded_extent_w = descriptor.max_coded_extent.width,
            max_coded_extent_h = descriptor.max_coded_extent.height,
            max_dpb_slots = descriptor.max_dpb_slots,
            max_active_reference_pictures = descriptor.max_active_reference_pictures,
            "HostVulkanVideoSession::new"
        );

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

        // Session creation + memory binding run under the device
        // resource lock so concurrent processor submissions on NVIDIA
        // Linux cannot race.
        let _device_lock = vulkan_device.lock_device();

        let raw_session =
            unsafe { device.create_video_session_khr(&create_info, None) }.map_err(|e| {
                Error::GpuError(format!(
                    "video session '{}': create_video_session_khr failed: {e}",
                    descriptor.label,
                ))
            })?;

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
            label: descriptor.label.to_string(),
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
    /// [`HostVulkanVideoSessionParameters`] and by codec-side
    /// callers that need to reach `vulkanalia::Device` /
    /// `lock_device` without threading another field through.
    #[inline]
    pub fn device(&self) -> &Arc<HostVulkanDevice> {
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
    ) -> Result<Vec<vma::Allocation>> {
        let mem_requirements =
            unsafe { device.get_video_session_memory_requirements_khr(raw_session) }.map_err(
                |e| {
                    Error::GpuError(format!(
                        "video session '{}': get_video_session_memory_requirements_khr failed: {e}",
                        descriptor.label,
                    ))
                },
            )?;

        if mem_requirements.len() > MAX_BOUND_MEMORY {
            return Err(Error::GpuError(format!(
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
                return Err(Error::GpuError(format!(
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
                        Error::GpuError(format!(
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
            Error::GpuError(format!(
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

impl Drop for HostVulkanVideoSession {
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
// HostVulkanVideoSessionParameters
// ----------------------------------------------------------------------------

/// Privileged RHI handle for a `VkVideoSessionParametersKHR`. Holds
/// a strong reference to the parent
/// [`HostVulkanVideoSession`]; Vulkan spec requires the session to
/// outlive any of its parameters objects, and the `Arc` makes that
/// invariant unbreakable from Rust.
pub struct HostVulkanVideoSessionParameters {
    #[allow(dead_code)]
    label: String,
    session: Arc<HostVulkanVideoSession>,
    handle: vk::VideoSessionParametersKHR,
}

unsafe impl Send for HostVulkanVideoSessionParameters {}
unsafe impl Sync for HostVulkanVideoSessionParameters {}

impl HostVulkanVideoSessionParameters {
    pub(crate) fn new(
        session: &Arc<HostVulkanVideoSession>,
        descriptor: &VideoSessionParametersDescriptor<'_>,
    ) -> Result<Arc<Self>> {
        tracing::debug!(
            rhi_op = "create_video_session_parameters",
            label = descriptor.label,
            quality_level = descriptor.quality_level.unwrap_or(0),
            has_template = descriptor.template.is_some(),
            "HostVulkanVideoSessionParameters::new"
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

        if let Some(level) = descriptor.quality_level {
            if level > 0 {
                quality_level_info =
                    vk::VideoEncodeQualityLevelInfoKHR::builder().quality_level(level);
                params_create = params_create.push_next(&mut quality_level_info);
            }
        }

        let handle = unsafe {
            device.create_video_session_parameters_khr(&params_create, None)
        }
        .map_err(|e| Error::GpuError(format!(
            "video session parameters '{}': create_video_session_parameters_khr failed: {e}",
            descriptor.label,
        )))?;

        Ok(Arc::new(Self {
            label: descriptor.label.to_string(),
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
    pub fn session(&self) -> &Arc<HostVulkanVideoSession> {
        &self.session
    }
}

impl Drop for HostVulkanVideoSessionParameters {
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
/// `HostVulkanVideoSession::is_compatible` delegates here so the rule
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
) -> Result<vk::ExtensionProperties> {
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
        Err(Error::GpuError(format!(
            "unsupported codec_operation {:?} for video session creation",
            codec_op,
        )))
    }
}

// ============================================================================
// Tests — pure logic (no GPU)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn ext_name_str(ext: &vk::ExtensionProperties) -> String {
        ext.extension_name
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8 as char)
            .collect()
    }

    #[test]
    fn std_header_version_h264_decode() {
        let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::DECODE_H264)
            .expect("H264 decode supported");
        assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_h264_decode");
        assert_eq!(ext.spec_version, 1 << 22);
    }

    #[test]
    fn std_header_version_h265_decode() {
        let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::DECODE_H265)
            .expect("H265 decode supported");
        assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_h265_decode");
    }

    #[test]
    fn std_header_version_h264_encode() {
        let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
            .expect("H264 encode supported");
        assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_h264_encode");
    }

    #[test]
    fn std_header_version_h265_encode() {
        let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::ENCODE_H265)
            .expect("H265 encode supported");
        assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_h265_encode");
    }

    #[test]
    fn std_header_version_av1_decode() {
        let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::DECODE_AV1)
            .expect("AV1 decode supported");
        assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_av1_decode");
    }

    #[test]
    fn std_header_version_vp9_decode() {
        let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::DECODE_VP9)
            .expect("VP9 decode supported");
        assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_vp9_decode");
    }

    #[test]
    fn std_header_version_av1_encode() {
        let ext = std_header_version_for_codec(vk::VideoCodecOperationFlagsKHR::ENCODE_AV1)
            .expect("AV1 encode supported");
        assert_eq!(ext_name_str(&ext), "VK_STD_vulkan_video_codec_av1_encode");
    }

    #[test]
    fn std_header_version_unknown_codec_errors() {
        let result = std_header_version_for_codec(
            vk::VideoCodecOperationFlagsKHR::from_bits_truncate(0xDEAD),
        );
        assert!(result.is_err());
    }

    #[test]
    fn max_bound_memory_matches_nvpro_cap() {
        // nvpro-samples `MAX_BOUND_MEMORY = 40`. Anchor here so a
        // future bump is visible.
        assert_eq!(MAX_BOUND_MEMORY, 40);
    }

    fn baseline_snapshot() -> VideoSessionCreateInfoSnapshot {
        VideoSessionCreateInfoSnapshot {
            queue_family_index: 0,
            picture_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
            max_coded_extent: vk::Extent2D {
                width: 1920,
                height: 1080,
            },
            reference_picture_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
            max_dpb_slots: 16,
            max_active_reference_pictures: 16,
        }
    }

    fn baseline_descriptor() -> VideoSessionDescriptor<'static> {
        VideoSessionDescriptor {
            label: "test/baseline",
            session_create_flags: vk::VideoSessionCreateFlagsKHR::empty(),
            video_queue_family: 0,
            video_profile: vk::VideoProfileInfoKHR::default(),
            codec_operation: vk::VideoCodecOperationFlagsKHR::DECODE_H264,
            picture_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
            max_coded_extent: vk::Extent2D {
                width: 1920,
                height: 1080,
            },
            reference_pictures_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
            max_dpb_slots: 16,
            max_active_reference_pictures: 16,
        }
    }

    #[test]
    fn snapshot_compatible_when_descriptor_matches_baseline() {
        let snap = baseline_snapshot();
        let desc = baseline_descriptor();
        assert!(snapshot_is_compatible(
            &snap,
            vk::VideoSessionCreateFlagsKHR::empty(),
            &desc
        ));
    }

    #[test]
    fn snapshot_compatible_when_descriptor_under_all_caps() {
        let snap = baseline_snapshot();
        let mut desc = baseline_descriptor();
        desc.max_coded_extent = vk::Extent2D {
            width: 1280,
            height: 720,
        };
        desc.max_dpb_slots = 8;
        desc.max_active_reference_pictures = 4;
        assert!(snapshot_is_compatible(
            &snap,
            vk::VideoSessionCreateFlagsKHR::empty(),
            &desc
        ));
    }

    #[test]
    fn snapshot_incompatible_when_width_exceeds_cap() {
        let snap = baseline_snapshot();
        let mut desc = baseline_descriptor();
        desc.max_coded_extent.width = 1921;
        assert!(!snapshot_is_compatible(
            &snap,
            vk::VideoSessionCreateFlagsKHR::empty(),
            &desc
        ));
    }

    #[test]
    fn snapshot_incompatible_when_height_exceeds_cap() {
        let snap = baseline_snapshot();
        let mut desc = baseline_descriptor();
        desc.max_coded_extent.height = 1081;
        assert!(!snapshot_is_compatible(
            &snap,
            vk::VideoSessionCreateFlagsKHR::empty(),
            &desc
        ));
    }

    #[test]
    fn snapshot_incompatible_when_dpb_slots_exceed_cap() {
        let snap = baseline_snapshot();
        let mut desc = baseline_descriptor();
        desc.max_dpb_slots = 17;
        assert!(!snapshot_is_compatible(
            &snap,
            vk::VideoSessionCreateFlagsKHR::empty(),
            &desc
        ));
    }

    #[test]
    fn snapshot_incompatible_when_active_refs_exceed_cap() {
        let snap = baseline_snapshot();
        let mut desc = baseline_descriptor();
        desc.max_active_reference_pictures = 17;
        assert!(!snapshot_is_compatible(
            &snap,
            vk::VideoSessionCreateFlagsKHR::empty(),
            &desc
        ));
    }

    #[test]
    fn snapshot_incompatible_when_picture_format_differs() {
        let snap = baseline_snapshot();
        let mut desc = baseline_descriptor();
        desc.picture_format = vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16;
        assert!(!snapshot_is_compatible(
            &snap,
            vk::VideoSessionCreateFlagsKHR::empty(),
            &desc
        ));
    }

    #[test]
    fn snapshot_incompatible_when_reference_format_differs() {
        let snap = baseline_snapshot();
        let mut desc = baseline_descriptor();
        desc.reference_pictures_format = vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16;
        assert!(!snapshot_is_compatible(
            &snap,
            vk::VideoSessionCreateFlagsKHR::empty(),
            &desc
        ));
    }

    #[test]
    fn snapshot_incompatible_when_queue_family_differs() {
        let snap = baseline_snapshot();
        let mut desc = baseline_descriptor();
        desc.video_queue_family = 1;
        assert!(!snapshot_is_compatible(
            &snap,
            vk::VideoSessionCreateFlagsKHR::empty(),
            &desc
        ));
    }

    #[test]
    fn snapshot_incompatible_when_session_flags_differ() {
        let snap = baseline_snapshot();
        let mut desc = baseline_descriptor();
        desc.session_create_flags = vk::VideoSessionCreateFlagsKHR::PROTECTED_CONTENT;
        assert!(!snapshot_is_compatible(
            &snap,
            vk::VideoSessionCreateFlagsKHR::empty(),
            &desc
        ));
    }

    // ----- Hardware-gated --------------------------------------------------
    //
    // Construct a real `HostVulkanVideoSession` against the host's
    // Vulkan device when the device exposes an H.264 decode queue
    // family. Gated by `--features streamlib/hardware-tests` so CI
    // (which has no GPU per `project_ci_strategy_no_gpu` memory)
    // doesn't try to run it.

    use crate::vulkan::rhi::HostVulkanDevice;

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn hardware_construct_h264_decode_session() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Skipping — no Vulkan device available: {e:?}");
                return;
            }
        };
        let queue_family = match device.video_decode_queue_family_index() {
            Some(idx) => idx,
            None => {
                eprintln!("Skipping — device exposes no video decode queue family");
                return;
            }
        };
        let mut h264_profile = vk::VideoDecodeH264ProfileInfoKHR::builder()
            .std_profile_idc(vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH)
            .picture_layout(vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE);
        let mut video_profile = vk::VideoProfileInfoKHR::default();
        video_profile.video_codec_operation = vk::VideoCodecOperationFlagsKHR::DECODE_H264;
        video_profile.chroma_subsampling = vk::VideoChromaSubsamplingFlagsKHR::_420;
        video_profile.luma_bit_depth = vk::VideoComponentBitDepthFlagsKHR::_8;
        video_profile.chroma_bit_depth = vk::VideoComponentBitDepthFlagsKHR::_8;
        video_profile.next = &mut *h264_profile as *mut _ as *mut std::ffi::c_void;

        let descriptor = VideoSessionDescriptor {
            label: "hardware/h264-decode-construct",
            session_create_flags: vk::VideoSessionCreateFlagsKHR::empty(),
            video_queue_family: queue_family,
            video_profile,
            codec_operation: vk::VideoCodecOperationFlagsKHR::DECODE_H264,
            picture_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
            max_coded_extent: vk::Extent2D {
                width: 1920,
                height: 1080,
            },
            reference_pictures_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
            max_dpb_slots: 17,
            max_active_reference_pictures: 16,
        };
        match HostVulkanVideoSession::new(&device, &descriptor) {
            Ok(session) => {
                assert_ne!(
                    session.handle(),
                    vk::VideoSessionKHR::null(),
                    "session handle must be non-null"
                );
                // is_compatible: same descriptor → compatible.
                assert!(session.is_compatible(&descriptor));
                // Shrunk DPB → still compatible.
                let mut smaller = descriptor.clone();
                smaller.max_dpb_slots = 5;
                assert!(session.is_compatible(&smaller));
                // Exceeded width → incompatible.
                let mut larger = descriptor.clone();
                larger.max_coded_extent.width = descriptor.max_coded_extent.width + 1;
                assert!(!session.is_compatible(&larger));
            }
            Err(e) => {
                eprintln!(
                    "Skipping — driver rejected H.264 decode session at construction (likely insufficient caps on this device): {e:?}"
                );
            }
        }
    }
}
