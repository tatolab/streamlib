// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of `common/include/VkVideoCore/VulkanVideoCapabilities.h`
//!
//! Provides Vulkan Video capability query functions for decode and encode.
//! The C++ original is a class with all static methods; here we expose
//! them as free functions.
//!
//! Key divergence from C++: the C++ methods accept a `VulkanDeviceContext*`
//! which wraps a dispatch table. In Rust we accept the relevant ash extension
//! loader structs (`ash::khr::video_queue::Instance`, etc.) plus
//! `vk::PhysicalDevice` directly.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrVideoQueueExtensionInstanceCommands;
use vulkanalia::vk::KhrVideoEncodeQueueExtensionInstanceCommands;

// ---------------------------------------------------------------------------
// Codec-specific decode capability sType validation
// ---------------------------------------------------------------------------

/// Identifies the expected decode capability `sType` for a given codec.
///
/// Returns `Some(expected_stype)` for supported decode codecs, `None` otherwise.
fn decode_capability_stype(
    codec: vk::VideoCodecOperationFlagsKHR,
) -> Option<vk::StructureType> {
    if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
        Some(vk::StructureType::VIDEO_DECODE_H264_CAPABILITIES_KHR)
    } else if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H265 {
        Some(vk::StructureType::VIDEO_DECODE_H265_CAPABILITIES_KHR)
    } else if codec == vk::VideoCodecOperationFlagsKHR::DECODE_AV1 {
        Some(vk::StructureType::VIDEO_DECODE_AV1_CAPABILITIES_KHR)
    } else {
        // VP9 decode types are not yet present in ash 0.38.
        None
    }
}

/// Identifies the expected encode capability `sType` for a given codec.
///
/// Returns `Some(expected_stype)` for supported encode codecs, `None` otherwise.
fn encode_capability_stype(
    codec: vk::VideoCodecOperationFlagsKHR,
) -> Option<vk::StructureType> {
    if codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
        Some(vk::StructureType::VIDEO_ENCODE_H264_CAPABILITIES_KHR)
    } else if codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
        Some(vk::StructureType::VIDEO_ENCODE_H265_CAPABILITIES_KHR)
    } else {
        // AV1 encode types are not yet present in ash 0.38.
        None
    }
}

// ---------------------------------------------------------------------------
// GetVideoDecodeCapabilities
// ---------------------------------------------------------------------------

/// Result of a decode capability query, mirroring the C++ out-parameters.
pub struct VideoDecodeCapabilitiesResult {
    pub video_capabilities: vk::VideoCapabilitiesKHR,
    pub video_decode_capabilities: vk::VideoDecodeCapabilitiesKHR,
    /// Codec-specific capabilities, discriminated by the codec used.
    pub codec_capabilities: DecodeCodecCapabilities,
}

/// Codec-specific decode capabilities.
pub enum DecodeCodecCapabilities {
    H264(vk::VideoDecodeH264CapabilitiesKHR),
    H265(vk::VideoDecodeH265CapabilitiesKHR),
    Av1(vk::VideoDecodeAV1CapabilitiesKHR),
}

/// Query video decode capabilities for the given profile.
///
/// Mirrors `VulkanVideoCapabilities::GetVideoDecodeCapabilities`.
///
/// # Safety
///
/// The caller must ensure that `physical_device` is valid and that the
/// appropriate Vulkan Video extensions are enabled.
pub unsafe fn get_video_decode_capabilities(
    video_queue_instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    video_profile: &vk::VideoProfileInfoKHR,
) -> Result<VideoDecodeCapabilitiesResult, vk::ErrorCode> {
    let video_codec = video_profile.video_codec_operation;

    let mut h264_capabilities = vk::VideoDecodeH264CapabilitiesKHR::default();
    let mut h265_capabilities = vk::VideoDecodeH265CapabilitiesKHR::default();
    let mut av1_capabilities = vk::VideoDecodeAV1CapabilitiesKHR::default();

    let mut video_decode_capabilities = vk::VideoDecodeCapabilitiesKHR::default();

    if video_codec == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
        video_decode_capabilities.next =
            &mut h264_capabilities as *mut _ as *mut core::ffi::c_void;
    } else if video_codec == vk::VideoCodecOperationFlagsKHR::DECODE_H265 {
        video_decode_capabilities.next =
            &mut h265_capabilities as *mut _ as *mut core::ffi::c_void;
    } else if video_codec == vk::VideoCodecOperationFlagsKHR::DECODE_AV1 {
        video_decode_capabilities.next =
            &mut av1_capabilities as *mut _ as *mut core::ffi::c_void;
    } else {
        tracing::error!("Unsupported decode codec: {:?}", video_codec);
        return Err(vk::ErrorCode::VIDEO_PROFILE_CODEC_NOT_SUPPORTED_KHR);
    }

    let mut video_capabilities = vk::VideoCapabilitiesKHR {
        next: &mut video_decode_capabilities as *mut _ as *mut core::ffi::c_void,
        ..Default::default()
    };

    video_queue_instance
        .get_physical_device_video_capabilities_khr(
            physical_device,
            video_profile,
            &mut video_capabilities,
        )
        .map_err(|e| {
            tracing::error!("GetVideoDecodeCapabilities failed: {:?}", e);
            e
        })?;

    let codec_capabilities = if video_codec == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
        DecodeCodecCapabilities::H264(h264_capabilities)
    } else if video_codec == vk::VideoCodecOperationFlagsKHR::DECODE_H265 {
        DecodeCodecCapabilities::H265(h265_capabilities)
    } else {
        DecodeCodecCapabilities::Av1(av1_capabilities)
    };

    Ok(VideoDecodeCapabilitiesResult {
        video_capabilities,
        video_decode_capabilities,
        codec_capabilities,
    })
}

// ---------------------------------------------------------------------------
// GetVideoEncodeCapabilities
// ---------------------------------------------------------------------------

/// Result of an encode capability query.
pub struct VideoEncodeCapabilitiesResult {
    pub video_capabilities: vk::VideoCapabilitiesKHR,
    pub video_encode_capabilities: vk::VideoEncodeCapabilitiesKHR,
    pub codec_capabilities: EncodeCodecCapabilities,
}

/// Codec-specific encode capabilities.
pub enum EncodeCodecCapabilities {
    H264(vk::VideoEncodeH264CapabilitiesKHR),
    H265(vk::VideoEncodeH265CapabilitiesKHR),
}

/// Query video encode capabilities for the given profile.
///
/// Mirrors `VulkanVideoCapabilities::GetVideoEncodeCapabilities`.
/// The C++ version is templated over codec types for quantization map and
/// intra refresh capabilities. Those extensions are not yet in ash 0.38,
/// so we omit them here and will add them when available.
///
/// # Safety
///
/// The caller must ensure that `physical_device` is valid and that the
/// appropriate Vulkan Video extensions are enabled.
pub unsafe fn get_video_encode_capabilities(
    video_queue_instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    video_profile: &vk::VideoProfileInfoKHR,
) -> Result<VideoEncodeCapabilitiesResult, vk::ErrorCode> {
    let video_codec = video_profile.video_codec_operation;

    let mut h264_capabilities = vk::VideoEncodeH264CapabilitiesKHR::default();
    let mut h265_capabilities = vk::VideoEncodeH265CapabilitiesKHR::default();

    let mut video_encode_capabilities = vk::VideoEncodeCapabilitiesKHR::default();

    if video_codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
        video_encode_capabilities.next =
            &mut h264_capabilities as *mut _ as *mut core::ffi::c_void;
    } else if video_codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
        video_encode_capabilities.next =
            &mut h265_capabilities as *mut _ as *mut core::ffi::c_void;
    } else {
        tracing::error!("Unsupported encode codec: {:?}", video_codec);
        return Err(vk::ErrorCode::VIDEO_PROFILE_CODEC_NOT_SUPPORTED_KHR);
    }

    let mut video_capabilities = vk::VideoCapabilitiesKHR {
        next: &mut video_encode_capabilities as *mut _ as *mut core::ffi::c_void,
        ..Default::default()
    };

    video_queue_instance
        .get_physical_device_video_capabilities_khr(
            physical_device,
            video_profile,
            &mut video_capabilities,
        )
        .map_err(|e| {
            tracing::error!("GetVideoEncodeCapabilities failed: {:?}", e);
            e
        })?;

    let codec_capabilities = if video_codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
        EncodeCodecCapabilities::H264(h264_capabilities)
    } else {
        EncodeCodecCapabilities::H265(h265_capabilities)
    };

    Ok(VideoEncodeCapabilitiesResult {
        video_capabilities,
        video_encode_capabilities,
        codec_capabilities,
    })
}

// ---------------------------------------------------------------------------
// GetPhysicalDeviceVideoEncodeQualityLevelProperties
// ---------------------------------------------------------------------------

/// Result of a quality level query.
pub struct EncodeQualityLevelResult {
    pub quality_level_properties: vk::VideoEncodeQualityLevelPropertiesKHR,
}

/// Query encode quality level properties.
///
/// Mirrors `VulkanVideoCapabilities::GetPhysicalDeviceVideoEncodeQualityLevelProperties`.
/// The C++ version is templated over codec quality level types; in Rust we
/// accept a mutable `*mut c_void` for the codec-specific pNext extension.
///
/// # Safety
///
/// The caller must ensure that `physical_device` is valid and that
/// `codec_quality_level_properties_ptr` (if non-null) points to a valid
/// initialized structure with the correct `sType`.
pub unsafe fn get_physical_device_video_encode_quality_level_properties(
    video_encode_queue_instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    video_profile: &vk::VideoProfileInfoKHR,
    quality_level: u32,
    codec_quality_level_properties_ptr: *mut core::ffi::c_void,
) -> Result<EncodeQualityLevelResult, vk::ErrorCode> {
    let quality_level_info = vk::PhysicalDeviceVideoEncodeQualityLevelInfoKHR {
        s_type: vk::StructureType::PHYSICAL_DEVICE_VIDEO_ENCODE_QUALITY_LEVEL_INFO_KHR,
        next: core::ptr::null(),
        video_profile: video_profile as *const _,
        quality_level,
        ..Default::default()
    };

    let mut quality_level_properties = vk::VideoEncodeQualityLevelPropertiesKHR {
        next: codec_quality_level_properties_ptr,
        ..Default::default()
    };

    video_encode_queue_instance
        .get_physical_device_video_encode_quality_level_properties_khr(
            physical_device,
            &quality_level_info,
            &mut quality_level_properties,
        )
        .map_err(|e| {
            tracing::error!(
                "GetPhysicalDeviceVideoEncodeQualityLevelProperties failed: {:?}, quality_level: {}",
                e,
                quality_level
            );
            e
        })?;

    Ok(EncodeQualityLevelResult {
        quality_level_properties,
    })
}

// ---------------------------------------------------------------------------
// GetSupportedVideoFormats
// ---------------------------------------------------------------------------

/// Result of a video format query for decode.
pub struct SupportedVideoFormatsResult {
    pub picture_format: vk::Format,
    pub reference_pictures_format: vk::Format,
}

/// Query supported video formats for decode, choosing picture and DPB formats.
///
/// Mirrors `VulkanVideoCapabilities::GetSupportedVideoFormats`.
///
/// # Safety
///
/// The caller must ensure that `physical_device` is valid and the appropriate
/// extensions are enabled.
pub unsafe fn get_supported_video_formats(
    video_queue_instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    video_profile: &vk::VideoProfileInfoKHR,
    capability_flags: vk::VideoDecodeCapabilityFlagsKHR,
) -> Result<SupportedVideoFormatsResult, vk::ErrorCode> {
    if capability_flags.contains(vk::VideoDecodeCapabilityFlagsKHR::DPB_AND_OUTPUT_COINCIDE) {
        // NV, Intel: DPB and output share the same images.
        let formats = get_video_formats(
            video_queue_instance,
            physical_device,
            video_profile,
            vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR | vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR,
        )?;

        if formats.is_empty() {
            tracing::error!("No supported coincide DPB formats found");
            return Err(vk::ErrorCode::VIDEO_PROFILE_FORMAT_NOT_SUPPORTED_KHR);
        }

        let fmt = formats[0].format;
        validate_formats(fmt, fmt);

        Ok(SupportedVideoFormatsResult {
            picture_format: fmt,
            reference_pictures_format: fmt,
        })
    } else if capability_flags.contains(vk::VideoDecodeCapabilityFlagsKHR::DPB_AND_OUTPUT_DISTINCT)
    {
        // AMD: DPB and output are separate images.
        let dpb_formats = get_video_formats(
            video_queue_instance,
            physical_device,
            video_profile,
            vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR,
        )?;

        let out_formats = get_video_formats(
            video_queue_instance,
            physical_device,
            video_profile,
            vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR,
        )?;

        if dpb_formats.is_empty() || out_formats.is_empty() {
            tracing::error!("No supported distinct DPB/output formats found");
            return Err(vk::ErrorCode::VIDEO_PROFILE_FORMAT_NOT_SUPPORTED_KHR);
        }

        let reference_pictures_format = dpb_formats[0].format;
        let picture_format = out_formats[0].format;

        validate_formats(reference_pictures_format, picture_format);

        Ok(SupportedVideoFormatsResult {
            picture_format,
            reference_pictures_format,
        })
    } else {
        tracing::error!(
            "Unsupported decode capability flags: {:?}",
            capability_flags
        );
        Err(vk::ErrorCode::VIDEO_PROFILE_FORMAT_NOT_SUPPORTED_KHR)
    }
}

/// Log warnings when queried formats are undefined or mismatched.
fn validate_formats(reference_pictures_format: vk::Format, picture_format: vk::Format) {
    if reference_pictures_format == vk::Format::UNDEFINED
        || picture_format == vk::Format::UNDEFINED
    {
        tracing::error!(
            "Video format is undefined. reference_pictures_format: {:?}, picture_format: {:?}",
            reference_pictures_format,
            picture_format
        );
    }
    if reference_pictures_format != picture_format {
        tracing::warn!(
            "reference_pictures_format ({:?}) != picture_format ({:?})",
            reference_pictures_format,
            picture_format
        );
    }
}

// ---------------------------------------------------------------------------
// GetVideoCapabilities (validates pNext chain, then calls VK)
// ---------------------------------------------------------------------------

/// Validate the capabilities chain and issue the Vulkan call.
///
/// Mirrors `VulkanVideoCapabilities::GetVideoCapabilities`.
///
/// # Safety
///
/// `video_capabilities` must have a properly constructed pNext chain that
/// matches the codec indicated by `video_profile`.
pub unsafe fn get_video_capabilities(
    video_queue_instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    video_profile: &vk::VideoProfileInfoKHR,
    video_capabilities: &mut vk::VideoCapabilitiesKHR,
    dump_data: bool,
) -> vk::Result {
    assert_eq!(
        video_capabilities.s_type,
        vk::StructureType::VIDEO_CAPABILITIES_KHR
    );

    let codec = video_profile.video_codec_operation;

    // Validate the pNext chain sType for the codec.
    let next_ptr = video_capabilities.next;
    if !next_ptr.is_null() {
        let next_stype = (*(next_ptr as *const vk::BaseOutStructure)).s_type;

        let is_decode = next_stype == vk::StructureType::VIDEO_DECODE_CAPABILITIES_KHR;
        let is_encode = next_stype == vk::StructureType::VIDEO_ENCODE_CAPABILITIES_KHR;

        if !is_decode && !is_encode {
            tracing::error!(
                "Invalid pNext sType: {:?}, expected decode or encode capabilities",
                next_stype
            );
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }

        // Validate codec-specific capability structure further down the chain.
        if is_decode {
            let decode_caps = &*(next_ptr as *const vk::VideoDecodeCapabilitiesKHR);
            if !decode_caps.next.is_null() {
                let codec_stype =
                    (*(decode_caps.next as *const vk::BaseOutStructure)).s_type;
                if let Some(expected) = decode_capability_stype(codec) {
                    if codec_stype != expected {
                        tracing::error!(
                            "Codec capability sType mismatch: got {:?}, expected {:?}",
                            codec_stype,
                            expected
                        );
                        return vk::Result::ERROR_INITIALIZATION_FAILED;
                    }
                }
            }
        } else {
            // encode
            let encode_caps = &*(next_ptr as *const vk::VideoEncodeCapabilitiesKHR);
            if !encode_caps.next.is_null() {
                let codec_stype =
                    (*(encode_caps.next as *const vk::BaseOutStructure)).s_type;
                if let Some(expected) = encode_capability_stype(codec) {
                    if codec_stype != expected {
                        tracing::error!(
                            "Codec capability sType mismatch: got {:?}, expected {:?}",
                            codec_stype,
                            expected
                        );
                        return vk::Result::ERROR_INITIALIZATION_FAILED;
                    }
                }
            }
        }
    }

    let vk_result = video_queue_instance
        .get_physical_device_video_capabilities_khr(
            physical_device,
            video_profile,
            video_capabilities,
        );

    if let Err(e) = vk_result {
        tracing::error!(
            "GetPhysicalDeviceVideoCapabilitiesKHR failed: {:?}",
            e,
        );
        return vk::Result::from(e);
    }

    if dump_data {
        dump_video_capabilities(video_capabilities, codec);
    }

    vk::Result::SUCCESS
}

/// Dump capability data via tracing.
///
/// Mirrors the `dumpData` branch of the C++ `GetVideoCapabilities`.
fn dump_video_capabilities(
    caps: &vk::VideoCapabilitiesKHR,
    codec: vk::VideoCodecOperationFlagsKHR,
) {
    let codec_name = if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
        "h264"
    } else if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H265 {
        "h265"
    } else if codec == vk::VideoCodecOperationFlagsKHR::DECODE_AV1 {
        "av1"
    } else if codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
        "h264enc"
    } else if codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
        "h265enc"
    } else {
        "unknown"
    };

    tracing::debug!("{} capabilities:", codec_name);

    if caps
        .flags
        .contains(vk::VideoCapabilityFlagsKHR::SEPARATE_REFERENCE_IMAGES)
    {
        tracing::debug!("  Use separate reference images");
    }

    tracing::debug!(
        "  minBitstreamBufferOffsetAlignment: {}",
        caps.min_bitstream_buffer_offset_alignment
    );
    tracing::debug!(
        "  minBitstreamBufferSizeAlignment: {}",
        caps.min_bitstream_buffer_size_alignment
    );
    tracing::debug!(
        "  pictureAccessGranularity: {} x {}",
        caps.picture_access_granularity.width,
        caps.picture_access_granularity.height
    );
    tracing::debug!(
        "  minCodedExtent: {} x {}",
        caps.min_coded_extent.width,
        caps.min_coded_extent.height
    );
    tracing::debug!(
        "  maxCodedExtent: {} x {}",
        caps.max_coded_extent.width,
        caps.max_coded_extent.height
    );
    tracing::debug!("  maxDpbSlots: {}", caps.max_dpb_slots);
    tracing::debug!(
        "  maxActiveReferencePictures: {}",
        caps.max_active_reference_pictures
    );
}

// ---------------------------------------------------------------------------
// GetVideoFormats (internal helper)
// ---------------------------------------------------------------------------

/// Query video format properties for the given image usage.
///
/// Mirrors `VulkanVideoCapabilities::GetVideoFormats`.
///
/// # Safety
///
/// The caller must ensure that `physical_device` is valid.
pub unsafe fn get_video_formats(
    video_queue_instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    video_profile: &vk::VideoProfileInfoKHR,
    image_usage: vk::ImageUsageFlags,
) -> Result<Vec<vk::VideoFormatPropertiesKHR>, vk::ErrorCode> {
    let video_profiles = vk::VideoProfileListInfoKHR {
        s_type: vk::StructureType::VIDEO_PROFILE_LIST_INFO_KHR,
        next: core::ptr::null(),
        profile_count: 1,
        profiles: video_profile as *const _,
        ..Default::default()
    };

    let video_format_info = vk::PhysicalDeviceVideoFormatInfoKHR {
        s_type: vk::StructureType::PHYSICAL_DEVICE_VIDEO_FORMAT_INFO_KHR,
        next: &video_profiles as *const _ as *const core::ffi::c_void,
        image_usage,
        ..Default::default()
    };

    // vulkanalia's wrapper handles the two-call count+fill pattern.
    let supported_formats = video_queue_instance
        .get_physical_device_video_format_properties_khr(
            physical_device,
            &video_format_info,
        )?;

    if supported_formats.is_empty() {
        tracing::warn!(
            "No supported video formats found for usage {:?}",
            image_usage
        );
    }

    Ok(supported_formats)
}

// ---------------------------------------------------------------------------
// GetSupportedCodecs
// ---------------------------------------------------------------------------

/// Default set of all codec operation flags to query.
pub const ALL_CODEC_OPERATIONS: vk::VideoCodecOperationFlagsKHR =
    vk::VideoCodecOperationFlagsKHR::from_bits_truncate(
        vk::VideoCodecOperationFlagsKHR::DECODE_H264.bits()
            | vk::VideoCodecOperationFlagsKHR::DECODE_H265.bits()
            | vk::VideoCodecOperationFlagsKHR::DECODE_AV1.bits()
            | vk::VideoCodecOperationFlagsKHR::ENCODE_H264.bits()
            | vk::VideoCodecOperationFlagsKHR::ENCODE_H265.bits(),
    );

/// Default queue flags to look for when searching for a video queue.
pub const DEFAULT_VIDEO_QUEUE_FLAGS: vk::QueueFlags = vk::QueueFlags::from_bits_truncate(
    vk::QueueFlags::VIDEO_DECODE_KHR.bits() | vk::QueueFlags::VIDEO_ENCODE_KHR.bits(),
);

/// Query which video codec operations are supported on the given physical
/// device, optionally filtering to a specific queue family.
///
/// Mirrors `VulkanVideoCapabilities::GetSupportedCodecs`.
///
/// `video_queue_family`: if `Some(idx)`, only check that queue family.
/// If `None`, scan all families and return the first match (writing the
/// found index back through the return tuple).
///
/// Returns `(supported_codecs, queue_family_index)`.
///
/// # Safety
///
/// The caller must ensure that `instance` and `physical_device` are valid.
pub unsafe fn get_supported_codecs(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    video_queue_family: Option<u32>,
    queue_flags_required: vk::QueueFlags,
    video_codec_operations: vk::VideoCodecOperationFlagsKHR,
) -> (vk::VideoCodecOperationFlagsKHR, Option<u32>) {
    let (queues, video_queues, _query_result_status) =
        get_queue_family_video_properties(instance, physical_device);

    for (queue_index, (q, vq)) in queues.iter().zip(video_queues.iter()).enumerate() {
        let queue_index = queue_index as u32;

        if let Some(required_family) = video_queue_family {
            if required_family != queue_index {
                continue;
            }
        }

        let flags = q.queue_family_properties.queue_flags;
        if flags.contains(queue_flags_required)
            && vq.video_codec_operations.intersects(video_codec_operations)
        {
            return (vq.video_codec_operations, Some(queue_index));
        }
    }

    (vk::VideoCodecOperationFlagsKHR::NONE, None)
}

/// Convenience overload: check codecs supported on a known queue family.
///
/// Mirrors the second `GetSupportedCodecs` overload.
///
/// # Safety
///
/// The caller must ensure that `instance` and `physical_device` are valid.
pub unsafe fn get_supported_codecs_for_queue(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    video_queue_family: u32,
) -> vk::VideoCodecOperationFlagsKHR {
    let (codecs, _) = get_supported_codecs(
        instance,
        physical_device,
        Some(video_queue_family),
        DEFAULT_VIDEO_QUEUE_FLAGS,
        ALL_CODEC_OPERATIONS,
    );
    codecs
}

/// Check whether a specific codec is supported on the given queue family.
///
/// Mirrors `VulkanVideoCapabilities::IsCodecTypeSupported`.
///
/// # Safety
///
/// The caller must ensure that `instance` and `physical_device` are valid.
pub unsafe fn is_codec_type_supported(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    video_queue_family: u32,
    video_codec: vk::VideoCodecOperationFlagsKHR,
) -> bool {
    let codecs = get_supported_codecs_for_queue(instance, physical_device, video_queue_family);
    codecs.intersects(video_codec)
}

// ---------------------------------------------------------------------------
// GetDecodeH264Capabilities / GetDecodeH265Capabilities / etc.
// ---------------------------------------------------------------------------

/// Simple capability query for a specific decode profile.
///
/// Mirrors `VulkanVideoCapabilities::GetDecodeH264Capabilities` and siblings.
///
/// # Safety
///
/// The caller must ensure that `physical_device` is valid.
pub unsafe fn get_decode_capabilities_simple(
    video_queue_instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    video_profile: &vk::VideoProfileInfoKHR,
) -> Result<vk::VideoCapabilitiesKHR, vk::ErrorCode> {
    let mut video_capabilities = vk::VideoCapabilitiesKHR::default();

    video_queue_instance.get_physical_device_video_capabilities_khr(
        physical_device,
        video_profile,
        &mut video_capabilities,
    )?;

    Ok(video_capabilities)
}

/// Query encode H.264 capabilities with codec-specific output.
///
/// Mirrors `VulkanVideoCapabilities::GetEncodeH264Capabilities`.
///
/// # Safety
///
/// The caller must ensure that `physical_device` is valid.
pub unsafe fn get_encode_h264_capabilities(
    video_queue_instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    video_profile: &vk::VideoProfileInfoKHR,
) -> Result<
    (
        vk::VideoCapabilitiesKHR,
        vk::VideoEncodeH264CapabilitiesKHR,
    ),
    vk::ErrorCode,
> {
    let mut encode_264_capabilities = vk::VideoEncodeH264CapabilitiesKHR::default();
    let mut video_capabilities = vk::VideoCapabilitiesKHR {
        next: &mut encode_264_capabilities as *mut _ as *mut core::ffi::c_void,
        ..Default::default()
    };

    video_queue_instance.get_physical_device_video_capabilities_khr(
        physical_device,
        video_profile,
        &mut video_capabilities,
    )?;

    Ok((video_capabilities, encode_264_capabilities))
}

// ---------------------------------------------------------------------------
// GetVideoMaintenance1FeatureSupported
// ---------------------------------------------------------------------------

/// Check whether the `VK_KHR_video_maintenance1` feature is supported.
///
/// Mirrors `VulkanVideoCapabilities::GetVideoMaintenance1FeatureSupported`.
///
/// # Safety
///
/// The caller must ensure that `instance` and `physical_device` are valid.
pub unsafe fn get_video_maintenance1_feature_supported(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
) -> bool {
    let mut video_maintenance1_features =
        vk::PhysicalDeviceVideoMaintenance1FeaturesKHR::default();
    let mut device_features = vk::PhysicalDeviceFeatures2 {
        next: &mut video_maintenance1_features as *mut _ as *mut core::ffi::c_void,
        ..Default::default()
    };

    instance.get_physical_device_features2(physical_device, &mut device_features);

    video_maintenance1_features.video_maintenance1 == vk::TRUE
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Enumerate queue family properties including video codec operations.
///
/// Mirrors the `get()` helper from `Helpers.h`.
///
/// # Safety
///
/// The caller must ensure that `instance` and `physical_device` are valid.
unsafe fn get_queue_family_video_properties(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
) -> (
    Vec<vk::QueueFamilyProperties2>,
    Vec<vk::QueueFamilyVideoPropertiesKHR>,
    Vec<vk::QueueFamilyQueryResultStatusPropertiesKHR>,
) {
    // Use raw function pointer because vulkanalia's wrapper doesn't support
    // pNext chains on the output structures.
    let get_props_fn = instance.commands().get_physical_device_queue_family_properties2;

    // First call: get count.
    let mut count: u32 = 0;
    get_props_fn(physical_device, &mut count, core::ptr::null_mut());

    let mut query_result_status: Vec<vk::QueueFamilyQueryResultStatusPropertiesKHR> =
        vec![vk::QueueFamilyQueryResultStatusPropertiesKHR::default(); count as usize];
    let mut video_queues: Vec<vk::QueueFamilyVideoPropertiesKHR> =
        vec![vk::QueueFamilyVideoPropertiesKHR::default(); count as usize];
    let mut queues: Vec<vk::QueueFamilyProperties2> =
        vec![vk::QueueFamilyProperties2::default(); count as usize];

    // Wire up pNext chains: queue -> video_queue -> query_result_status
    for i in 0..count as usize {
        video_queues[i].next =
            &mut query_result_status[i] as *mut _ as *mut core::ffi::c_void;
        queues[i].next = &mut video_queues[i] as *mut _ as *mut core::ffi::c_void;
    }

    get_props_fn(physical_device, &mut count, queues.as_mut_ptr());

    queues.truncate(count as usize);
    video_queues.truncate(count as usize);
    query_result_status.truncate(count as usize);

    (queues, video_queues, query_result_status)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_capability_stype_h264() {
        assert_eq!(
            decode_capability_stype(vk::VideoCodecOperationFlagsKHR::DECODE_H264),
            Some(vk::StructureType::VIDEO_DECODE_H264_CAPABILITIES_KHR)
        );
    }

    #[test]
    fn test_decode_capability_stype_h265() {
        assert_eq!(
            decode_capability_stype(vk::VideoCodecOperationFlagsKHR::DECODE_H265),
            Some(vk::StructureType::VIDEO_DECODE_H265_CAPABILITIES_KHR)
        );
    }

    #[test]
    fn test_decode_capability_stype_av1() {
        assert_eq!(
            decode_capability_stype(vk::VideoCodecOperationFlagsKHR::DECODE_AV1),
            Some(vk::StructureType::VIDEO_DECODE_AV1_CAPABILITIES_KHR)
        );
    }

    #[test]
    fn test_decode_capability_stype_unsupported() {
        // Encode codec should return None for decode lookup.
        assert_eq!(
            decode_capability_stype(vk::VideoCodecOperationFlagsKHR::ENCODE_H264),
            None
        );
    }

    #[test]
    fn test_encode_capability_stype_h264() {
        assert_eq!(
            encode_capability_stype(vk::VideoCodecOperationFlagsKHR::ENCODE_H264),
            Some(vk::StructureType::VIDEO_ENCODE_H264_CAPABILITIES_KHR)
        );
    }

    #[test]
    fn test_encode_capability_stype_h265() {
        assert_eq!(
            encode_capability_stype(vk::VideoCodecOperationFlagsKHR::ENCODE_H265),
            Some(vk::StructureType::VIDEO_ENCODE_H265_CAPABILITIES_KHR)
        );
    }

    #[test]
    fn test_encode_capability_stype_unsupported() {
        assert_eq!(
            encode_capability_stype(vk::VideoCodecOperationFlagsKHR::DECODE_H264),
            None
        );
    }

    #[test]
    fn test_validate_formats_both_valid_same() {
        // Same valid format should succeed without error.
        validate_formats(
            vk::Format::G8_B8R8_2PLANE_420_UNORM,
            vk::Format::G8_B8R8_2PLANE_420_UNORM,
        );
    }

    #[test]
    fn test_validate_formats_different() {
        // Different but valid formats should still succeed (just warns).
        validate_formats(
            vk::Format::G8_B8R8_2PLANE_420_UNORM,
            vk::Format::R8G8B8A8_UNORM,
        );
    }

    #[test]
    fn test_all_codec_operations_contains_expected() {
        assert!(ALL_CODEC_OPERATIONS.contains(vk::VideoCodecOperationFlagsKHR::DECODE_H264));
        assert!(ALL_CODEC_OPERATIONS.contains(vk::VideoCodecOperationFlagsKHR::DECODE_H265));
        assert!(ALL_CODEC_OPERATIONS.contains(vk::VideoCodecOperationFlagsKHR::DECODE_AV1));
        assert!(ALL_CODEC_OPERATIONS.contains(vk::VideoCodecOperationFlagsKHR::ENCODE_H264));
        assert!(ALL_CODEC_OPERATIONS.contains(vk::VideoCodecOperationFlagsKHR::ENCODE_H265));
    }

    #[test]
    fn test_all_codec_operations_does_not_contain_none() {
        // NONE is the zero value; intersects should return false.
        assert!(!ALL_CODEC_OPERATIONS.intersects(vk::VideoCodecOperationFlagsKHR::NONE));
    }

    #[test]
    fn test_default_video_queue_flags() {
        assert!(DEFAULT_VIDEO_QUEUE_FLAGS.contains(vk::QueueFlags::VIDEO_DECODE_KHR));
        assert!(DEFAULT_VIDEO_QUEUE_FLAGS.contains(vk::QueueFlags::VIDEO_ENCODE_KHR));
    }

    #[test]
    fn test_video_capabilities_khr_default_stype() {
        let caps = vk::VideoCapabilitiesKHR::default();
        assert_eq!(caps.s_type, vk::StructureType::VIDEO_CAPABILITIES_KHR);
    }

    #[test]
    fn test_video_decode_capabilities_khr_default_stype() {
        let caps = vk::VideoDecodeCapabilitiesKHR::default();
        assert_eq!(
            caps.s_type,
            vk::StructureType::VIDEO_DECODE_CAPABILITIES_KHR
        );
    }

    #[test]
    fn test_video_encode_capabilities_khr_default_stype() {
        let caps = vk::VideoEncodeCapabilitiesKHR::default();
        assert_eq!(
            caps.s_type,
            vk::StructureType::VIDEO_ENCODE_CAPABILITIES_KHR
        );
    }
}
