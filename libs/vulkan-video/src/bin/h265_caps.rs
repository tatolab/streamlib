// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // dev diagnostic binary: stdout is the output channel

//! Diagnostic tool: dump ALL Vulkan Video H.265 encode capabilities.
//!
//! Creates a Vulkan 1.3 instance, finds the first physical device with
//! video encode support, and queries every field of:
//!   - VkVideoCapabilitiesKHR
//!   - VkVideoEncodeCapabilitiesKHR
//!   - VkVideoEncodeH265CapabilitiesKHR
//!
//! Also queries the quality-level properties for each supported level.
//!
//! Usage:
//!   cargo run --bin h265-caps

use std::ffi::CStr;
use std::ptr;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrVideoQueueExtensionInstanceCommands;
use vulkanalia::vk::KhrVideoEncodeQueueExtensionInstanceCommands;

fn main() {
    println!("=======================================================");
    println!("  Vulkan Video H.265 Encode Capability Dump");
    println!("=======================================================\n");

    // ------------------------------------------------------------------
    // 1. Load Vulkan
    // ------------------------------------------------------------------
    let loader = match unsafe {
        vulkanalia::loader::LibloadingLoader::new(vulkanalia::loader::LIBRARY)
    } {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Cannot load Vulkan loader: {}", e);
            std::process::exit(1);
        }
    };
    let entry = match unsafe { vulkanalia::Entry::new(loader) } {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Cannot create Vulkan entry: {}", e);
            std::process::exit(1);
        }
    };

    // ------------------------------------------------------------------
    // 2. Create instance (Vulkan 1.3)
    // ------------------------------------------------------------------
    let app_info = vk::ApplicationInfo::builder()
        .application_name(b"h265-caps-dump\0")
        .application_version(vk::make_version(0, 1, 0))
        .engine_name(b"nvpro-vulkan-video\0")
        .engine_version(vk::make_version(0, 1, 0))
        .api_version(vk::make_version(1, 3, 0));

    let instance_info = vk::InstanceCreateInfo::builder().application_info(&app_info);

    let instance = match unsafe { entry.create_instance(&instance_info, None) } {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Failed to create Vulkan 1.3 instance: {:?}", e);
            std::process::exit(1);
        }
    };

    // ------------------------------------------------------------------
    // 3. Pick physical device
    // ------------------------------------------------------------------
    let physical_devices = unsafe { instance.enumerate_physical_devices() }.unwrap_or_default();
    if physical_devices.is_empty() {
        eprintln!("No Vulkan physical devices found.");
        unsafe { instance.destroy_instance(None) };
        std::process::exit(1);
    }

    let physical_device = physical_devices[0];
    let props = unsafe { instance.get_physical_device_properties(physical_device) };
    let device_name = unsafe { CStr::from_ptr(props.device_name.as_ptr()) }
        .to_str()
        .unwrap_or("unknown");
    println!(
        "Physical device: {} (Vulkan {}.{}.{})",
        device_name,
        vk::version_major(props.api_version),
        vk::version_minor(props.api_version),
        vk::version_patch(props.api_version),
    );
    println!(
        "Driver version: {}.{}.{}",
        vk::version_major(props.driver_version),
        vk::version_minor(props.driver_version),
        vk::version_patch(props.driver_version),
    );
    println!("Device type: {:?}", props.device_type);
    println!(
        "Vendor ID: 0x{:04X}, Device ID: 0x{:04X}",
        props.vendor_id, props.device_id
    );
    println!();

    // ------------------------------------------------------------------
    // 4. Check for video encode queue family
    // ------------------------------------------------------------------
    let queue_families =
        unsafe { instance.get_physical_device_queue_family_properties(physical_device) };

    let mut encode_qf: Option<u32> = None;
    println!("--- Queue Families ---");
    for (i, qf) in queue_families.iter().enumerate() {
        let has_encode = qf.queue_flags.contains(vk::QueueFlags::VIDEO_ENCODE_KHR);
        let has_decode = qf.queue_flags.contains(vk::QueueFlags::VIDEO_DECODE_KHR);
        println!(
            "  QF {}: flags={:?}, count={}, encode={}, decode={}",
            i, qf.queue_flags, qf.queue_count, has_encode, has_decode
        );
        if has_encode && encode_qf.is_none() {
            encode_qf = Some(i as u32);
        }
    }

    if encode_qf.is_none() {
        eprintln!("\nNo video encode queue family found. Cannot query H.265 encode caps.");
        unsafe { instance.destroy_instance(None) };
        std::process::exit(1);
    }
    println!("\nUsing encode queue family: {}\n", encode_qf.unwrap());

    // ------------------------------------------------------------------
    // 5. Build H.265 encode profile
    // ------------------------------------------------------------------
    let mut h265_profile = vk::VideoEncodeH265ProfileInfoKHR::builder()
        .std_profile_idc(vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN);

    let profile_info = vk::VideoProfileInfoKHR::builder()
        .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H265)
        .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::_420)
        .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
        .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
        .push_next(&mut h265_profile);

    // ------------------------------------------------------------------
    // 6. Query capabilities with full pNext chain
    // ------------------------------------------------------------------
    let mut h265_encode_caps = vk::VideoEncodeH265CapabilitiesKHR::default();
    let mut encode_caps = vk::VideoEncodeCapabilitiesKHR::default();
    let mut caps = vk::VideoCapabilitiesKHR::default();

    // Chain: caps -> encode_caps -> h265_encode_caps
    encode_caps.next = &mut h265_encode_caps as *mut _ as *mut std::ffi::c_void;
    caps.next = &mut encode_caps as *mut _ as *mut std::ffi::c_void;

    let result = unsafe {
        instance.get_physical_device_video_capabilities_khr(
            physical_device,
            &profile_info,
            &mut caps,
        )
    };

    if let Err(e) = result {
        eprintln!("get_physical_device_video_capabilities_khr FAILED: {:?}", e);
        eprintln!("H.265 encode may not be supported on this device.");
        unsafe { instance.destroy_instance(None) };
        std::process::exit(1);
    }

    // ------------------------------------------------------------------
    // 7. Print VkVideoCapabilitiesKHR
    // ------------------------------------------------------------------
    println!("=======================================================");
    println!("  VkVideoCapabilitiesKHR");
    println!("=======================================================");
    println!("  flags: {:?}", caps.flags);
    println!(
        "    SEPARATE_REFERENCE_IMAGES: {}",
        caps.flags
            .contains(vk::VideoCapabilityFlagsKHR::SEPARATE_REFERENCE_IMAGES)
    );
    println!(
        "    PROTECTED_CONTENT: {}",
        caps.flags
            .contains(vk::VideoCapabilityFlagsKHR::PROTECTED_CONTENT)
    );
    println!(
        "  minBitstreamBufferOffsetAlignment: {}",
        caps.min_bitstream_buffer_offset_alignment
    );
    println!(
        "  minBitstreamBufferSizeAlignment: {}",
        caps.min_bitstream_buffer_size_alignment
    );
    println!(
        "  pictureAccessGranularity: {}x{}",
        caps.picture_access_granularity.width, caps.picture_access_granularity.height
    );
    println!(
        "  minCodedExtent: {}x{}",
        caps.min_coded_extent.width, caps.min_coded_extent.height
    );
    println!(
        "  maxCodedExtent: {}x{}",
        caps.max_coded_extent.width, caps.max_coded_extent.height
    );
    println!("  maxDpbSlots: {}", caps.max_dpb_slots);
    println!(
        "  maxActiveReferencePictures: {}",
        caps.max_active_reference_pictures
    );
    println!(
        "  stdHeaderVersion: name={}, version={}.{}.{}, specVersion={}",
        unsafe { CStr::from_ptr(caps.std_header_version.extension_name.as_ptr()) }
            .to_str()
            .unwrap_or("?"),
        vk::version_major(caps.std_header_version.spec_version),
        vk::version_minor(caps.std_header_version.spec_version),
        vk::version_patch(caps.std_header_version.spec_version),
        caps.std_header_version.spec_version,
    );
    println!();

    // ------------------------------------------------------------------
    // 8. Print VkVideoEncodeCapabilitiesKHR
    // ------------------------------------------------------------------
    println!("=======================================================");
    println!("  VkVideoEncodeCapabilitiesKHR");
    println!("=======================================================");
    println!("  flags: {:?}", encode_caps.flags);
    println!(
        "    PRECEDING_EXTERNALLY_ENCODED_BYTES: {}",
        encode_caps
            .flags
            .contains(vk::VideoEncodeCapabilityFlagsKHR::PRECEDING_EXTERNALLY_ENCODED_BYTES)
    );
    println!(
        "    INSUFFICIENT_BITSTREAM_BUFFER_RANGE_DETECTION: {}",
        encode_caps
            .flags
            .contains(vk::VideoEncodeCapabilityFlagsKHR::INSUFFICIENT_BITSTREAM_BUFFER_RANGE_DETECTION)
    );
    println!(
        "  rateControlModes: {:?}",
        encode_caps.rate_control_modes
    );
    println!(
        "    DEFAULT: {}",
        encode_caps
            .rate_control_modes
            .contains(vk::VideoEncodeRateControlModeFlagsKHR::DEFAULT)
    );
    println!(
        "    DISABLED (CQP): {}",
        encode_caps
            .rate_control_modes
            .contains(vk::VideoEncodeRateControlModeFlagsKHR::DISABLED)
    );
    println!(
        "    CBR: {}",
        encode_caps
            .rate_control_modes
            .contains(vk::VideoEncodeRateControlModeFlagsKHR::CBR)
    );
    println!(
        "    VBR: {}",
        encode_caps
            .rate_control_modes
            .contains(vk::VideoEncodeRateControlModeFlagsKHR::VBR)
    );
    println!(
        "  maxRateControlLayers: {}",
        encode_caps.max_rate_control_layers
    );
    println!("  maxBitrate: {} bits/sec", encode_caps.max_bitrate);
    println!("  maxQualityLevels: {}", encode_caps.max_quality_levels);
    println!(
        "  encodeInputPictureGranularity: {}x{}",
        encode_caps.encode_input_picture_granularity.width,
        encode_caps.encode_input_picture_granularity.height
    );
    println!(
        "  supportedEncodeFeedbackFlags: {:?}",
        encode_caps.supported_encode_feedback_flags
    );
    println!(
        "    BITSTREAM_BUFFER_OFFSET: {}",
        encode_caps
            .supported_encode_feedback_flags
            .contains(vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BUFFER_OFFSET)
    );
    println!(
        "    BITSTREAM_BYTES_WRITTEN: {}",
        encode_caps
            .supported_encode_feedback_flags
            .contains(vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN)
    );
    println!(
        "    BITSTREAM_HAS_OVERRIDES: {}",
        encode_caps
            .supported_encode_feedback_flags
            .contains(vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_HAS_OVERRIDES)
    );
    println!();

    // ------------------------------------------------------------------
    // 9. Print VkVideoEncodeH265CapabilitiesKHR
    // ------------------------------------------------------------------
    println!("=======================================================");
    println!("  VkVideoEncodeH265CapabilitiesKHR");
    println!("=======================================================");

    // flags
    println!("  flags: {:?}", h265_encode_caps.flags);
    println!(
        "    HRD_COMPLIANCE: {}",
        h265_encode_caps
            .flags
            .contains(vk::VideoEncodeH265CapabilityFlagsKHR::HRD_COMPLIANCE)
    );
    println!(
        "    PREDICTION_WEIGHT_TABLE_GENERATED: {}",
        h265_encode_caps
            .flags
            .contains(vk::VideoEncodeH265CapabilityFlagsKHR::PREDICTION_WEIGHT_TABLE_GENERATED)
    );
    println!(
        "    ROW_UNALIGNED_SLICE_SEGMENT: {}",
        h265_encode_caps
            .flags
            .contains(vk::VideoEncodeH265CapabilityFlagsKHR::ROW_UNALIGNED_SLICE_SEGMENT)
    );
    println!(
        "    DIFFERENT_SLICE_SEGMENT_TYPE: {}",
        h265_encode_caps
            .flags
            .contains(vk::VideoEncodeH265CapabilityFlagsKHR::DIFFERENT_SLICE_SEGMENT_TYPE)
    );
    println!(
        "    B_FRAME_IN_L0_LIST: {}",
        h265_encode_caps
            .flags
            .contains(vk::VideoEncodeH265CapabilityFlagsKHR::B_FRAME_IN_L0_LIST)
    );
    println!(
        "    B_FRAME_IN_L1_LIST: {}",
        h265_encode_caps
            .flags
            .contains(vk::VideoEncodeH265CapabilityFlagsKHR::B_FRAME_IN_L1_LIST)
    );
    println!(
        "    PER_PICTURE_TYPE_MIN_MAX_QP: {}",
        h265_encode_caps
            .flags
            .contains(vk::VideoEncodeH265CapabilityFlagsKHR::PER_PICTURE_TYPE_MIN_MAX_QP)
    );
    println!(
        "    PER_SLICE_SEGMENT_CONSTANT_QP: {}",
        h265_encode_caps
            .flags
            .contains(vk::VideoEncodeH265CapabilityFlagsKHR::PER_SLICE_SEGMENT_CONSTANT_QP)
    );
    println!(
        "    MULTIPLE_TILES_PER_SLICE_SEGMENT: {}",
        h265_encode_caps
            .flags
            .contains(vk::VideoEncodeH265CapabilityFlagsKHR::MULTIPLE_TILES_PER_SLICE_SEGMENT)
    );
    println!(
        "    MULTIPLE_SLICE_SEGMENTS_PER_TILE: {}",
        h265_encode_caps
            .flags
            .contains(vk::VideoEncodeH265CapabilityFlagsKHR::MULTIPLE_SLICE_SEGMENTS_PER_TILE)
    );
    println!(
        "    CU_QP_DIFF_WRAPAROUND: {}",
        h265_encode_caps
            .flags
            .contains(vk::VideoEncodeH265CapabilityFlagsKHR::CU_QP_DIFF_WRAPAROUND)
    );
    println!(
        "    B_PICTURE_INTRA_REFRESH: {}",
        h265_encode_caps
            .flags
            .contains(vk::VideoEncodeH265CapabilityFlagsKHR::B_PICTURE_INTRA_REFRESH)
    );

    // maxLevelIdc
    let level_str = level_idc_to_string(h265_encode_caps.max_level_idc);
    println!(
        "  maxLevelIdc: {} (raw={})",
        level_str, h265_encode_caps.max_level_idc.0
    );

    // maxSliceSegmentCount
    println!(
        "  maxSliceSegmentCount: {}",
        h265_encode_caps.max_slice_segment_count
    );

    // maxTiles
    println!(
        "  maxTiles: {}x{}",
        h265_encode_caps.max_tiles.width, h265_encode_caps.max_tiles.height
    );

    // ctbSizes
    println!("  ctbSizes: {:?}", h265_encode_caps.ctb_sizes);
    println!(
        "    16x16: {}",
        h265_encode_caps
            .ctb_sizes
            .contains(vk::VideoEncodeH265CtbSizeFlagsKHR::_16)
    );
    println!(
        "    32x32: {}",
        h265_encode_caps
            .ctb_sizes
            .contains(vk::VideoEncodeH265CtbSizeFlagsKHR::_32)
    );
    println!(
        "    64x64: {}",
        h265_encode_caps
            .ctb_sizes
            .contains(vk::VideoEncodeH265CtbSizeFlagsKHR::_64)
    );

    // transformBlockSizes
    println!(
        "  transformBlockSizes: {:?}",
        h265_encode_caps.transform_block_sizes
    );
    println!(
        "    4x4: {}",
        h265_encode_caps
            .transform_block_sizes
            .contains(vk::VideoEncodeH265TransformBlockSizeFlagsKHR::_4)
    );
    println!(
        "    8x8: {}",
        h265_encode_caps
            .transform_block_sizes
            .contains(vk::VideoEncodeH265TransformBlockSizeFlagsKHR::_8)
    );
    println!(
        "    16x16: {}",
        h265_encode_caps
            .transform_block_sizes
            .contains(vk::VideoEncodeH265TransformBlockSizeFlagsKHR::_16)
    );
    println!(
        "    32x32: {}",
        h265_encode_caps
            .transform_block_sizes
            .contains(vk::VideoEncodeH265TransformBlockSizeFlagsKHR::_32)
    );

    // Reference counts
    println!(
        "  maxPPictureL0ReferenceCount: {}",
        h265_encode_caps.max_p_picture_l0_reference_count
    );
    println!(
        "  maxBPictureL0ReferenceCount: {}",
        h265_encode_caps.max_b_picture_l0_reference_count
    );
    println!(
        "  maxL1ReferenceCount: {}",
        h265_encode_caps.max_l1_reference_count
    );

    // Sub-layers
    println!(
        "  maxSubLayerCount: {}",
        h265_encode_caps.max_sub_layer_count
    );
    println!(
        "  expectDyadicTemporalSubLayerPattern: {} (raw={})",
        h265_encode_caps.expect_dyadic_temporal_sub_layer_pattern != 0,
        h265_encode_caps.expect_dyadic_temporal_sub_layer_pattern,
    );

    // QP range
    println!("  minQp: {}", h265_encode_caps.min_qp);
    println!("  maxQp: {}", h265_encode_caps.max_qp);

    // GOP
    println!(
        "  prefersGopRemainingFrames: {} (raw={})",
        h265_encode_caps.prefers_gop_remaining_frames != 0,
        h265_encode_caps.prefers_gop_remaining_frames,
    );
    println!(
        "  requiresGopRemainingFrames: {} (raw={})",
        h265_encode_caps.requires_gop_remaining_frames != 0,
        h265_encode_caps.requires_gop_remaining_frames,
    );

    // stdSyntaxFlags
    println!(
        "  stdSyntaxFlags: {:?}",
        h265_encode_caps.std_syntax_flags
    );
    println!(
        "    SEPARATE_COLOR_PLANE_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::SEPARATE_COLOR_PLANE_FLAG_SET)
    );
    println!(
        "    SAMPLE_ADAPTIVE_OFFSET_ENABLED_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::SAMPLE_ADAPTIVE_OFFSET_ENABLED_FLAG_SET)
    );
    println!(
        "    SCALING_LIST_DATA_PRESENT_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::SCALING_LIST_DATA_PRESENT_FLAG_SET)
    );
    println!(
        "    PCM_ENABLED_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::PCM_ENABLED_FLAG_SET)
    );
    println!(
        "    SPS_TEMPORAL_MVP_ENABLED_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::SPS_TEMPORAL_MVP_ENABLED_FLAG_SET)
    );
    println!(
        "    INIT_QP_MINUS26: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::INIT_QP_MINUS26)
    );
    println!(
        "    WEIGHTED_PRED_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::WEIGHTED_PRED_FLAG_SET)
    );
    println!(
        "    WEIGHTED_BIPRED_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::WEIGHTED_BIPRED_FLAG_SET)
    );
    println!(
        "    LOG2_PARALLEL_MERGE_LEVEL_MINUS2: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::LOG2_PARALLEL_MERGE_LEVEL_MINUS2)
    );
    println!(
        "    SIGN_DATA_HIDING_ENABLED_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::SIGN_DATA_HIDING_ENABLED_FLAG_SET)
    );
    println!(
        "    TRANSFORM_SKIP_ENABLED_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::TRANSFORM_SKIP_ENABLED_FLAG_SET)
    );
    println!(
        "    TRANSFORM_SKIP_ENABLED_FLAG_UNSET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::TRANSFORM_SKIP_ENABLED_FLAG_UNSET)
    );
    println!(
        "    PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT_FLAG_SET)
    );
    println!(
        "    TRANSQUANT_BYPASS_ENABLED_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::TRANSQUANT_BYPASS_ENABLED_FLAG_SET)
    );
    println!(
        "    CONSTRAINED_INTRA_PRED_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::CONSTRAINED_INTRA_PRED_FLAG_SET)
    );
    println!(
        "    ENTROPY_CODING_SYNC_ENABLED_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::ENTROPY_CODING_SYNC_ENABLED_FLAG_SET)
    );
    println!(
        "    DEBLOCKING_FILTER_OVERRIDE_ENABLED_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::DEBLOCKING_FILTER_OVERRIDE_ENABLED_FLAG_SET)
    );
    println!(
        "    DEPENDENT_SLICE_SEGMENTS_ENABLED_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::DEPENDENT_SLICE_SEGMENTS_ENABLED_FLAG_SET)
    );
    println!(
        "    DEPENDENT_SLICE_SEGMENT_FLAG_SET: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::DEPENDENT_SLICE_SEGMENT_FLAG_SET)
    );
    println!(
        "    SLICE_QP_DELTA: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::SLICE_QP_DELTA)
    );
    println!(
        "    DIFFERENT_SLICE_QP_DELTA: {}",
        h265_encode_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsKHR::DIFFERENT_SLICE_QP_DELTA)
    );
    println!();

    // ------------------------------------------------------------------
    // 10. Query quality level properties
    // ------------------------------------------------------------------
    println!("=======================================================");
    println!("  Quality Level Properties (per level)");
    println!("=======================================================");

    for ql in 0..encode_caps.max_quality_levels {
        let quality_level_info = vk::PhysicalDeviceVideoEncodeQualityLevelInfoKHR {
            s_type: vk::StructureType::PHYSICAL_DEVICE_VIDEO_ENCODE_QUALITY_LEVEL_INFO_KHR,
            next: ptr::null(),
            video_profile: &*profile_info as *const vk::VideoProfileInfoKHR,
            quality_level: ql,
            ..Default::default()
        };

        let mut h265_ql_props = vk::VideoEncodeH265QualityLevelPropertiesKHR::default();
        let mut ql_props = vk::VideoEncodeQualityLevelPropertiesKHR {
            next: &mut h265_ql_props as *mut _ as *mut std::ffi::c_void,
            ..Default::default()
        };

        let result = unsafe {
            instance.get_physical_device_video_encode_quality_level_properties_khr(
                physical_device,
                &quality_level_info,
                &mut ql_props,
            )
        };

        match result {
            Ok(()) => {
                println!("  --- Quality Level {} ---", ql);
                println!(
                    "    preferredRateControlMode: {:?}",
                    ql_props.preferred_rate_control_mode
                );
                println!(
                    "    preferredRateControlLayerCount: {}",
                    ql_props.preferred_rate_control_layer_count
                );
                println!(
                    "    H265 preferredConstantQp I={}, P={}, B={}",
                    h265_ql_props.preferred_constant_qp.qp_i,
                    h265_ql_props.preferred_constant_qp.qp_p,
                    h265_ql_props.preferred_constant_qp.qp_b,
                );
                println!(
                    "    H265 preferredRateControlFlags: {:?}",
                    h265_ql_props.preferred_rate_control_flags
                );
                println!(
                    "    H265 preferredGopFrameCount: {}",
                    h265_ql_props.preferred_gop_frame_count
                );
                println!(
                    "    H265 preferredIdrPeriod: {}",
                    h265_ql_props.preferred_idr_period
                );
                println!(
                    "    H265 preferredConsecutiveBFrameCount: {}",
                    h265_ql_props.preferred_consecutive_b_frame_count
                );
                println!(
                    "    H265 preferredSubLayerCount: {}",
                    h265_ql_props.preferred_sub_layer_count
                );
                println!(
                    "    H265 preferredMaxL0ReferenceCount: {}",
                    h265_ql_props.preferred_max_l0_reference_count
                );
                println!(
                    "    H265 preferredMaxL1ReferenceCount: {}",
                    h265_ql_props.preferred_max_l1_reference_count
                );
            }
            Err(e) => {
                println!("  --- Quality Level {} --- FAILED: {:?}", ql, e);
            }
        }
    }
    println!();

    // ------------------------------------------------------------------
    // 11. Query supported video formats
    // ------------------------------------------------------------------
    println!("=======================================================");
    println!("  Supported Video Formats");
    println!("=======================================================");

    let profile_list_info = vk::VideoProfileListInfoKHR {
        s_type: vk::StructureType::VIDEO_PROFILE_LIST_INFO_KHR,
        next: ptr::null(),
        profile_count: 1,
        profiles: &*profile_info as *const vk::VideoProfileInfoKHR,
        ..Default::default()
    };

    // Query encode input (SRC) formats
    {
        let format_info = vk::PhysicalDeviceVideoFormatInfoKHR {
            s_type: vk::StructureType::PHYSICAL_DEVICE_VIDEO_FORMAT_INFO_KHR,
            next: &profile_list_info as *const _ as *const std::ffi::c_void,
            image_usage: vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR,
            ..Default::default()
        };

        match unsafe {
            instance.get_physical_device_video_format_properties_khr(physical_device, &format_info)
        } {
            Ok(formats) => {
                println!("  Encode SRC formats ({} total):", formats.len());
                for (i, f) in formats.iter().enumerate() {
                    println!(
                        "    [{}] format={:?}, componentMapping=({:?},{:?},{:?},{:?})",
                        i, f.format, f.component_mapping.r, f.component_mapping.g,
                        f.component_mapping.b, f.component_mapping.a
                    );
                }
            }
            Err(e) => {
                println!("  Encode SRC format query FAILED: {:?}", e);
            }
        }
    }

    // Query encode DPB formats
    {
        let format_info = vk::PhysicalDeviceVideoFormatInfoKHR {
            s_type: vk::StructureType::PHYSICAL_DEVICE_VIDEO_FORMAT_INFO_KHR,
            next: &profile_list_info as *const _ as *const std::ffi::c_void,
            image_usage: vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR,
            ..Default::default()
        };

        match unsafe {
            instance.get_physical_device_video_format_properties_khr(physical_device, &format_info)
        } {
            Ok(formats) => {
                println!("  Encode DPB formats ({} total):", formats.len());
                for (i, f) in formats.iter().enumerate() {
                    println!(
                        "    [{}] format={:?}, componentMapping=({:?},{:?},{:?},{:?})",
                        i, f.format, f.component_mapping.r, f.component_mapping.g,
                        f.component_mapping.b, f.component_mapping.a
                    );
                }
            }
            Err(e) => {
                println!("  Encode DPB format query FAILED: {:?}", e);
            }
        }
    }
    println!();

    // ------------------------------------------------------------------
    // 12. Also dump H.264 encode caps for comparison
    // ------------------------------------------------------------------
    println!("=======================================================");
    println!("  H.264 Encode Caps (for comparison)");
    println!("=======================================================");

    let mut h264_profile_info_ext = vk::VideoEncodeH264ProfileInfoKHR::builder()
        .std_profile_idc(vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH);

    let h264_profile_info = vk::VideoProfileInfoKHR::builder()
        .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
        .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::_420)
        .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
        .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
        .push_next(&mut h264_profile_info_ext);

    let mut h264_encode_caps = vk::VideoEncodeH264CapabilitiesKHR::default();
    let mut h264_gen_encode_caps = vk::VideoEncodeCapabilitiesKHR::default();
    let mut h264_caps = vk::VideoCapabilitiesKHR::default();
    h264_gen_encode_caps.next = &mut h264_encode_caps as *mut _ as *mut std::ffi::c_void;
    h264_caps.next = &mut h264_gen_encode_caps as *mut _ as *mut std::ffi::c_void;

    let h264_result = unsafe {
        instance.get_physical_device_video_capabilities_khr(
            physical_device,
            &h264_profile_info,
            &mut h264_caps,
        )
    };

    match h264_result {
        Ok(()) => {
            println!("  maxDpbSlots: {}", h264_caps.max_dpb_slots);
            println!(
                "  maxActiveReferencePictures: {}",
                h264_caps.max_active_reference_pictures
            );
            println!(
                "  pictureAccessGranularity: {}x{}",
                h264_caps.picture_access_granularity.width,
                h264_caps.picture_access_granularity.height
            );
            println!(
                "  encodeInputPictureGranularity: {}x{}",
                h264_gen_encode_caps.encode_input_picture_granularity.width,
                h264_gen_encode_caps.encode_input_picture_granularity.height
            );
            println!(
                "  maxQualityLevels: {}",
                h264_gen_encode_caps.max_quality_levels
            );
            println!(
                "  maxPPictureL0ReferenceCount (H264): {}",
                h264_encode_caps.max_p_picture_l0_reference_count
            );
            println!(
                "  maxBPictureL0ReferenceCount (H264): {}",
                h264_encode_caps.max_b_picture_l0_reference_count
            );
            println!(
                "  maxL1ReferenceCount (H264): {}",
                h264_encode_caps.max_l1_reference_count
            );
            println!("  minQp (H264): {}", h264_encode_caps.min_qp);
            println!("  maxQp (H264): {}", h264_encode_caps.max_qp);
            println!(
                "  prefersGopRemainingFrames (H264): {}",
                h264_encode_caps.prefers_gop_remaining_frames != 0
            );
            println!(
                "  requiresGopRemainingFrames (H264): {}",
                h264_encode_caps.requires_gop_remaining_frames != 0
            );
        }
        Err(e) => {
            println!("  H.264 encode cap query FAILED: {:?}", e);
        }
    }
    println!();

    // ------------------------------------------------------------------
    // Cleanup
    // ------------------------------------------------------------------
    println!("=======================================================");
    println!("  Done.");
    println!("=======================================================");

    unsafe { instance.destroy_instance(None) };
}

/// Convert StdVideoH265LevelIdc to a human-readable string.
fn level_idc_to_string(level: vk::video::StdVideoH265LevelIdc) -> &'static str {
    match level.0 {
        0 => "1.0",
        1 => "2.0",
        2 => "2.1",
        3 => "3.0",
        4 => "3.1",
        5 => "4.0",
        6 => "4.1",
        7 => "5.0",
        8 => "5.1",
        9 => "5.2",
        10 => "6.0",
        11 => "6.1",
        12 => "6.2",
        _ => "UNKNOWN",
    }
}
