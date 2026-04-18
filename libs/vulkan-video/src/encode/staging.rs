// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! NV12 staging upload, image layout transitions, and GPU-resident RGBA
//! encode path (RGB→NV12 compute shader).

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc};
use std::ffi::CStr;
use std::ptr;
use std::sync::Arc;

use crate::video_context::{VideoContext, VideoError};

use super::config::{Codec, EncodePacket, FrameType, SimpleEncoderConfig};
use super::SimpleEncoder;

impl SimpleEncoder {
    /// Create the encoder (all Vulkan setup, unsafe due to raw Vulkan calls).
    pub(crate) unsafe fn create_internal(config: SimpleEncoderConfig) -> Result<SimpleEncoder, VideoError> {
        // 1. Load Vulkan
        let entry = vulkanalia::Entry::new(
            vulkanalia::loader::LibloadingLoader::new(vulkanalia::loader::LIBRARY)
                .map_err(|e| VideoError::BitstreamError(format!("Failed to load Vulkan loader: {}", e)))?,
        ).map_err(|e| VideoError::BitstreamError(format!("Failed to load Vulkan: {}", e)))?;

        // 2. Create instance
        let app_info = vk::ApplicationInfo::builder()
            .application_name(b"nvpro-simple-encoder\0")
            .api_version(crate::video_context::REQUIRED_VULKAN_API_VERSION);

        let instance = entry
            .create_instance(
                &vk::InstanceCreateInfo::builder().application_info(&app_info),
                None,
            )
            .map_err(VideoError::from)?;

        // 3. Find physical device with encode support
        let physical_devices = instance
            .enumerate_physical_devices()
            .map_err(VideoError::from)?;

        if physical_devices.is_empty() {
            instance.destroy_instance(None);
            return Err(VideoError::BitstreamError(
                "No Vulkan physical devices found".to_string(),
            ));
        }

        let codec_flag = match config.codec {
            Codec::H264 => vk::VideoCodecOperationFlagsKHR::ENCODE_H264,
            Codec::H265 => vk::VideoCodecOperationFlagsKHR::ENCODE_H265,
        };

        // Find a device with encode, transfer, and compute queue families
        let mut selected_device = None;
        let mut encode_qf = 0u32;
        let mut transfer_qf = 0u32;
        let mut compute_qf = 0u32;

        for &pd in &physical_devices {
            let qf_props = instance.get_physical_device_queue_family_properties(pd);
            let mut found_encode = false;
            let mut found_transfer = false;
            let mut found_compute = false;

            for (i, p) in qf_props.iter().enumerate() {
                if p.queue_flags.contains(vk::QueueFlags::VIDEO_ENCODE_KHR) && !found_encode {
                    encode_qf = i as u32;
                    found_encode = true;
                }
                if (p.queue_flags.contains(vk::QueueFlags::TRANSFER)
                    || p.queue_flags.contains(vk::QueueFlags::GRAPHICS))
                    && !found_transfer
                {
                    transfer_qf = i as u32;
                    found_transfer = true;
                }
                if p.queue_flags.contains(vk::QueueFlags::COMPUTE) && !found_compute {
                    compute_qf = i as u32;
                    found_compute = true;
                }
            }

            if found_encode && found_transfer && found_compute {
                selected_device = Some(pd);
                break;
            }
        }

        let physical_device = match selected_device {
            Some(pd) => pd,
            None => {
                instance.destroy_instance(None);
                return Err(VideoError::NoVideoQueueFamily);
            }
        };

        // Reject software renderers (llvmpipe, lavapipe, swiftshader, etc.)
        crate::video_context::reject_software_renderer(&instance, physical_device)?;

        // 4. Create device with required extensions
        let codec_ext_name: &CStr = match config.codec {
            Codec::H264 => vk::KHR_VIDEO_ENCODE_H264_EXTENSION.name.as_cstr(),
            Codec::H265 => vk::KHR_VIDEO_ENCODE_H265_EXTENSION.name.as_cstr(),
        };

        let video_maint1_name =
            unsafe { CStr::from_bytes_with_nul_unchecked(b"VK_KHR_video_maintenance1\0") };

        let required_extensions: Vec<&CStr> = vec![
            vk::KHR_VIDEO_QUEUE_EXTENSION.name.as_cstr(),
            vk::KHR_VIDEO_ENCODE_QUEUE_EXTENSION.name.as_cstr(),
            codec_ext_name,
            vk::KHR_SYNCHRONIZATION2_EXTENSION.name.as_cstr(),
            vk::KHR_PUSH_DESCRIPTOR_EXTENSION.name.as_cstr(),
            video_maint1_name,
        ];

        // Validate all required extensions are supported by the device.
        let available = instance
            .enumerate_device_extension_properties(physical_device, None)
            .map_err(VideoError::from)?;
        for req in &required_extensions {
            let found = available.iter().any(|ext| {
                let name = unsafe { CStr::from_ptr(ext.extension_name.as_ptr()) };
                name == *req
            });
            if !found {
                return Err(VideoError::BitstreamError(format!(
                    "Required device extension {:?} not supported", req
                )));
            }
        }

        let device_extensions: Vec<*const i8> =
            required_extensions.iter().map(|c| c.as_ptr()).collect();

        let queue_priorities = [1.0f32];
        let mut queue_families_requested = vec![encode_qf];
        if transfer_qf != encode_qf {
            queue_families_requested.push(transfer_qf);
        }
        if compute_qf != encode_qf && compute_qf != transfer_qf {
            queue_families_requested.push(compute_qf);
        }

        let queue_create_infos: Vec<_> = queue_families_requested
            .iter()
            .map(|&qf| {
                vk::DeviceQueueCreateInfo::builder()
                    .queue_family_index(qf)
                    .queue_priorities(&queue_priorities)
            })
            .collect();

        let mut sync2 =
            vk::PhysicalDeviceSynchronization2Features::builder().synchronization2(true);
        let mut video_maint1 =
            vk::PhysicalDeviceVideoMaintenance1FeaturesKHR::builder().video_maintenance1(true);

        let device_info = vk::DeviceCreateInfo::builder()
            .queue_create_infos(&queue_create_infos)
            .enabled_extension_names(&device_extensions)
            .push_next(&mut sync2)
            .push_next(&mut video_maint1);

        let device = instance
            .create_device(physical_device, &device_info, None)
            .map_err(VideoError::from)?;

        let encode_queue = device.get_device_queue(encode_qf, 0);
        let transfer_queue = device.get_device_queue(transfer_qf, 0);
        let compute_queue = device.get_device_queue(compute_qf, 0);

        // 5. Create VideoContext
        let ctx = Arc::new(VideoContext::new(
            instance.clone(),
            device.clone(),
            physical_device,
        )?);

        // 6. Build the encoder state (construct Self with zeroed fields, then configure)
        let enc_config = config.to_encode_config();
        let gop = config.to_gop_structure();
        let prepend_header = config.effective_prepend_header();

        let submitter = crate::rhi::RawQueueSubmitter::new(device.clone());

        let mut this = SimpleEncoder {
            _entry: entry,
            _instance: instance.clone(),
            device: device.clone(),
            // Merged Encoder fields (zeroed, filled by configure())
            ctx: ctx.clone(),
            codec_flag,
            encode_config: None,
            video_session: vk::VideoSessionKHR::null(),
            session_memory: Vec::new(),
            session_params: vk::VideoSessionParametersKHR::null(),
            dpb_image: vk::Image::null(),
            dpb_allocation: unsafe { std::mem::zeroed() },
            dpb_separate_images: Vec::new(),
            dpb_separate_allocations: Vec::new(),
            dpb_slots: Vec::new(),
            bitstream_buffer: vk::Buffer::null(),
            bitstream_allocation: unsafe { std::mem::zeroed() },
            bitstream_buffer_size: 0,
            bitstream_mapped_ptr: ptr::null_mut(),
            command_pool: vk::CommandPool::null(),
            command_buffer: vk::CommandBuffer::null(),
            query_pool: vk::QueryPool::null(),
            fence: vk::Fence::null(),
            frame_count: 0,
            encode_order_count: 0,
            poc_counter: 0,
            rate_control_sent: false,
            aligned_width: 0,
            aligned_height: 0,
            configured: false,
            effective_quality_level: 0,
            h265_encoder: None,
            h265_config: None,
            h264_encoder: None,
            h264_config: None,
            // SimpleEncoder's own fields
            source_image: vk::Image::null(),
            source_view: vk::ImageView::null(),
            source_allocation: unsafe { std::mem::zeroed() },
            staging_buffer: vk::Buffer::null(),
            staging_allocation: unsafe { std::mem::zeroed() },
            staging_mapped_ptr: ptr::null_mut(),
            staging_size: 0,
            transfer_pool: vk::CommandPool::null(),
            transfer_cb: vk::CommandBuffer::null(),
            transfer_fence: vk::Fence::null(),
            transfer_queue,
            transfer_queue_family: transfer_qf,
            encode_queue,
            encode_queue_family: encode_qf,
            compute_queue,
            compute_queue_family: compute_qf,
            rgb_to_nv12: None,
            gop,
            gop_state: Default::default(),
            frame_counter: 0,
            force_idr_flag: false,
            reorder_buffer: Vec::new(),
            cached_header: Vec::new(),
            config,
            prepend_header,
            submitter,
        };

        // Configure encoder (creates video session, DPB, etc.)
        this.configure(&enc_config)?;

        // Extract header and patch VUI/timing (driver emits broken timing)
        let raw_header = this.extract_header().unwrap_or_default();
        this.cached_header = super::vui_patch::patch_header_timing(
            &raw_header,
            this.codec_flag,
            this.config.fps,
            1,
        );
        let (aligned_w, aligned_h) = this.aligned_extent();

        // 8. Create source image with VIDEO_ENCODE_SRC + TRANSFER_DST
        // Use the driver-reported aligned dimensions (may be larger than the
        // config's 16-aligned values, e.g. H.265 may require 32-alignment).

        let mut h264_profile = vk::VideoEncodeH264ProfileInfoKHR::builder().std_profile_idc(
            this.h264_profile_idc(),
        );
        let mut h265_profile = vk::VideoEncodeH265ProfileInfoKHR::builder().std_profile_idc(
            vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN,
        );

        // Source image profile MUST match the video session profile exactly,
        // including the encode_usage pNext chain. Without this, the validation
        // layer reports VUID-vkCmdEncodeVideoKHR-pEncodeInfo-08206.
        let mut src_encode_usage = vk::VideoEncodeUsageInfoKHR::builder()
            .tuning_mode(vk::VideoEncodeTuningModeKHR::LOW_LATENCY);

        let mut profile_info = vk::VideoProfileInfoKHR::builder()
            .video_codec_operation(codec_flag)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
            .push_next(&mut src_encode_usage);

        if codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            profile_info = profile_info.push_next(&mut h264_profile);
        } else {
            profile_info = profile_info.push_next(&mut h265_profile);
        }

        let profile_list =
            vk::VideoProfileListInfoKHR::builder().profiles(std::slice::from_ref(&profile_info));

        // When transfer and encode are on different queue families, the
        // source image must use CONCURRENT sharing mode so both families can
        // access it without explicit queue family ownership transfers.
        let src_queue_families = [transfer_qf, encode_qf];
        let mut src_image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
            .extent(vk::Extent3D {
                width: aligned_w,
                height: aligned_h,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR | vk::ImageUsageFlags::TRANSFER_DST,
            )
            .initial_layout(vk::ImageLayout::UNDEFINED);

        if transfer_qf != encode_qf {
            src_image_info = src_image_info
                .sharing_mode(vk::SharingMode::CONCURRENT)
                .queue_family_indices(&src_queue_families);
        } else {
            src_image_info = src_image_info
                .sharing_mode(vk::SharingMode::EXCLUSIVE);
        }

        src_image_info.next =
            &*profile_list as *const vk::VideoProfileListInfoKHR as *const std::ffi::c_void;

        let allocator = ctx.allocator();
        let src_alloc_options = vma::AllocationOptions {
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let (source_image, source_allocation) =
            allocator.create_image(src_image_info, &src_alloc_options)?;

        let source_view = device.create_image_view(
            &vk::ImageViewCreateInfo::builder()
                .image(source_image)
                .view_type(vk::ImageViewType::_2D)
                .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                }),
            None,
        )?;

        // 9. Create staging buffer (host-visible, sized for one NV12 frame)
        let staging_size = (aligned_w * aligned_h * 3 / 2) as usize;

        let stg_create_info = vk::BufferCreateInfo::builder()
            .size(staging_size as u64)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        let stg_alloc_options = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };

        let (staging_buffer, staging_allocation) =
            allocator.create_buffer(stg_create_info, &stg_alloc_options)?;

        let stg_info = allocator.get_allocation_info(staging_allocation);
        let staging_mapped_ptr = stg_info.pMappedData as *mut u8;
        if staging_mapped_ptr.is_null() {
            allocator.destroy_buffer(staging_buffer, staging_allocation);
            return Err(VideoError::Vulkan(vk::Result::ERROR_MEMORY_MAP_FAILED));
        }

        // 10. Transfer command pool / buffer / fence
        let transfer_pool = device.create_command_pool(
            &vk::CommandPoolCreateInfo::builder()
                .queue_family_index(transfer_qf)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )?;

        let transfer_cb = device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::builder()
                .command_pool(transfer_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )?[0];

        let transfer_fence =
            device.create_fence(&vk::FenceCreateInfo::default(), None)?;

        this.source_image = source_image;
        this.source_view = source_view;
        this.source_allocation = source_allocation;
        this.staging_buffer = staging_buffer;
        this.staging_allocation = staging_allocation;
        this.staging_mapped_ptr = staging_mapped_ptr;
        this.staging_size = staging_size;
        this.transfer_pool = transfer_pool;
        this.transfer_cb = transfer_cb;
        this.transfer_fence = transfer_fence;

        Ok(this)
    }

    /// Create the encoder from an externally-owned Vulkan device (skips device creation).
    pub(crate) unsafe fn create_from_external(
        config: SimpleEncoderConfig,
        instance: vulkanalia::Instance,
        device: vulkanalia::Device,
        physical_device: vk::PhysicalDevice,
        allocator: Arc<vma::Allocator>,
        submitter: Arc<dyn crate::rhi::RhiQueueSubmitter>,
        encode_queue: vk::Queue,
        encode_queue_family: u32,
        transfer_queue: vk::Queue,
        transfer_queue_family: u32,
        compute_queue: vk::Queue,
        compute_queue_family: u32,
    ) -> Result<SimpleEncoder, VideoError> {
        let codec_flag = match config.codec {
            Codec::H264 => vk::VideoCodecOperationFlagsKHR::ENCODE_H264,
            Codec::H265 => vk::VideoCodecOperationFlagsKHR::ENCODE_H265,
        };

        // Use the external device's allocator — no new VMA allocator created.
        let ctx = Arc::new(VideoContext::from_external(
            instance.clone(),
            device.clone(),
            physical_device,
            allocator,
        )?);

        let enc_config = config.to_encode_config();
        let gop = config.to_gop_structure();
        let prepend_header = config.effective_prepend_header();

        // Create a dummy Entry — not used for anything when device is external,
        // but the struct requires it. Load the Vulkan library to satisfy the field.
        let entry = vulkanalia::Entry::new(
            vulkanalia::loader::LibloadingLoader::new(vulkanalia::loader::LIBRARY)
                .map_err(|e| VideoError::BitstreamError(format!("Failed to load Vulkan loader: {}", e)))?,
        ).map_err(|e| VideoError::BitstreamError(format!("Failed to load Vulkan: {}", e)))?;

        let mut this = SimpleEncoder {
            _entry: entry,
            _instance: instance.clone(),
            device: device.clone(),
            ctx: ctx.clone(),
            codec_flag,
            encode_config: None,
            video_session: vk::VideoSessionKHR::null(),
            session_memory: Vec::new(),
            session_params: vk::VideoSessionParametersKHR::null(),
            dpb_image: vk::Image::null(),
            dpb_allocation: unsafe { std::mem::zeroed() },
            dpb_separate_images: Vec::new(),
            dpb_separate_allocations: Vec::new(),
            dpb_slots: Vec::new(),
            bitstream_buffer: vk::Buffer::null(),
            bitstream_allocation: unsafe { std::mem::zeroed() },
            bitstream_buffer_size: 0,
            bitstream_mapped_ptr: ptr::null_mut(),
            command_pool: vk::CommandPool::null(),
            command_buffer: vk::CommandBuffer::null(),
            query_pool: vk::QueryPool::null(),
            fence: vk::Fence::null(),
            frame_count: 0,
            encode_order_count: 0,
            poc_counter: 0,
            rate_control_sent: false,
            aligned_width: 0,
            aligned_height: 0,
            configured: false,
            effective_quality_level: 0,
            h265_encoder: None,
            h265_config: None,
            h264_encoder: None,
            h264_config: None,
            source_image: vk::Image::null(),
            source_view: vk::ImageView::null(),
            source_allocation: unsafe { std::mem::zeroed() },
            staging_buffer: vk::Buffer::null(),
            staging_allocation: unsafe { std::mem::zeroed() },
            staging_mapped_ptr: ptr::null_mut(),
            staging_size: 0,
            transfer_pool: vk::CommandPool::null(),
            transfer_cb: vk::CommandBuffer::null(),
            transfer_fence: vk::Fence::null(),
            transfer_queue,
            transfer_queue_family,
            encode_queue,
            encode_queue_family,
            compute_queue,
            compute_queue_family,
            rgb_to_nv12: None,
            gop,
            gop_state: Default::default(),
            frame_counter: 0,
            force_idr_flag: false,
            reorder_buffer: Vec::new(),
            cached_header: Vec::new(),
            config,
            prepend_header,
            submitter,
        };

        // Configure encoder (creates video session, DPB, etc.) — same as create_internal
        this.configure(&enc_config)?;

        let raw_header = this.extract_header().unwrap_or_default();
        this.cached_header = super::vui_patch::patch_header_timing(
            &raw_header,
            this.codec_flag,
            this.config.fps,
            1,
        );
        let (aligned_w, aligned_h) = this.aligned_extent();

        // Source NV12 image — same setup as create_internal
        let mut h264_profile = vk::VideoEncodeH264ProfileInfoKHR::builder().std_profile_idc(
            this.h264_profile_idc(),
        );
        let mut h265_profile = vk::VideoEncodeH265ProfileInfoKHR::builder().std_profile_idc(
            vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN,
        );
        let mut src_encode_usage = vk::VideoEncodeUsageInfoKHR::builder()
            .tuning_mode(vk::VideoEncodeTuningModeKHR::LOW_LATENCY);

        let mut profile_info = vk::VideoProfileInfoKHR::builder()
            .video_codec_operation(codec_flag)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
            .push_next(&mut src_encode_usage);

        if codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            profile_info = profile_info.push_next(&mut h264_profile);
        } else {
            profile_info = profile_info.push_next(&mut h265_profile);
        }

        let profile_list =
            vk::VideoProfileListInfoKHR::builder().profiles(std::slice::from_ref(&profile_info));

        let src_queue_families = [transfer_queue_family, encode_queue_family];
        let mut src_image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
            .extent(vk::Extent3D { width: aligned_w, height: aligned_h, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR | vk::ImageUsageFlags::TRANSFER_DST)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        if transfer_queue_family != encode_queue_family {
            src_image_info = src_image_info
                .sharing_mode(vk::SharingMode::CONCURRENT)
                .queue_family_indices(&src_queue_families);
        } else {
            src_image_info = src_image_info.sharing_mode(vk::SharingMode::EXCLUSIVE);
        }

        src_image_info.next =
            &*profile_list as *const vk::VideoProfileListInfoKHR as *const std::ffi::c_void;

        let allocator = ctx.allocator();
        let src_alloc_options = vma::AllocationOptions {
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let (source_image, source_allocation) =
            allocator.create_image(src_image_info, &src_alloc_options)?;

        let source_view = device.create_image_view(
            &vk::ImageViewCreateInfo::builder()
                .image(source_image)
                .view_type(vk::ImageViewType::_2D)
                .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0, level_count: 1,
                    base_array_layer: 0, layer_count: 1,
                }),
            None,
        )?;

        let staging_size = (aligned_w * aligned_h * 3 / 2) as usize;
        let stg_create_info = vk::BufferCreateInfo::builder()
            .size(staging_size as u64)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let stg_alloc_options = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };
        let (staging_buffer, staging_allocation) =
            allocator.create_buffer(stg_create_info, &stg_alloc_options)?;
        let stg_info = allocator.get_allocation_info(staging_allocation);
        let staging_mapped_ptr = stg_info.pMappedData as *mut u8;
        if staging_mapped_ptr.is_null() {
            allocator.destroy_buffer(staging_buffer, staging_allocation);
            return Err(VideoError::Vulkan(vk::Result::ERROR_MEMORY_MAP_FAILED));
        }

        let transfer_pool = device.create_command_pool(
            &vk::CommandPoolCreateInfo::builder()
                .queue_family_index(transfer_queue_family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )?;
        let transfer_cb = device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::builder()
                .command_pool(transfer_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )?[0];
        let transfer_fence = device.create_fence(&vk::FenceCreateInfo::default(), None)?;

        this.source_image = source_image;
        this.source_view = source_view;
        this.source_allocation = source_allocation;
        this.staging_buffer = staging_buffer;
        this.staging_allocation = staging_allocation;
        this.staging_mapped_ptr = staging_mapped_ptr;
        this.staging_size = staging_size;
        this.transfer_pool = transfer_pool;
        this.transfer_cb = transfer_cb;
        this.transfer_fence = transfer_fence;

        Ok(this)
    }

    /// Upload NV12 data, encode one frame, return the packet.
    pub(crate) unsafe fn upload_and_encode(
        &mut self,
        nv12_data: &[u8],
        frame_type: FrameType,
        display_pts: u64,
        timestamp_ns: Option<i64>,
    ) -> Result<EncodePacket, VideoError> {
        let width = self.config.width;
        let height = self.config.height;
        let enc_cfg = self.encode_config().unwrap();
        let aligned_w = enc_cfg.aligned_width();
        let aligned_h = enc_cfg.aligned_height();

        // Upload NV12 data to staging buffer
        let y_size = (width * height) as usize;
        let uv_size = (width * height / 2) as usize;
        let copy_size = y_size + uv_size;
        ptr::copy_nonoverlapping(
            nv12_data.as_ptr(),
            self.staging_mapped_ptr,
            copy_size.min(self.staging_size),
        );

        // Record transfer commands: barrier -> copy -> barrier
        self.device.reset_command_buffer(
            self.transfer_cb,
            vk::CommandBufferResetFlags::empty(),
        )?;

        self.device.begin_command_buffer(
            self.transfer_cb,
            &vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;

        // Barrier: UNDEFINED -> TRANSFER_DST
        let barrier_to_transfer = vk::ImageMemoryBarrier::builder()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(self.source_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);

        let no_mem_barriers: &[vk::MemoryBarrier] = &[];
        let no_buf_barriers: &[vk::BufferMemoryBarrier] = &[];
        self.device.cmd_pipeline_barrier(
            self.transfer_cb,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            no_mem_barriers,
            no_buf_barriers,
            &[barrier_to_transfer],
        );

        // Copy Y plane
        let y_region = vk::BufferImageCopy::builder()
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::PLANE_0,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_extent(vk::Extent3D {
                width: aligned_w,
                height: aligned_h,
                depth: 1,
            });

        // Copy UV plane
        let uv_region = vk::BufferImageCopy::builder()
            .buffer_offset(y_size as u64)
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::PLANE_1,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_extent(vk::Extent3D {
                width: aligned_w / 2,
                height: aligned_h / 2,
                depth: 1,
            });

        self.device.cmd_copy_buffer_to_image(
            self.transfer_cb,
            self.staging_buffer,
            self.source_image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[y_region, uv_region],
        );

        // Barrier: TRANSFER_DST -> VIDEO_ENCODE_SRC
        let barrier_to_encode = vk::ImageMemoryBarrier::builder()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::VIDEO_ENCODE_SRC_KHR)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(self.source_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });

        self.device.cmd_pipeline_barrier(
            self.transfer_cb,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            vk::DependencyFlags::empty(),
            no_mem_barriers,
            no_buf_barriers,
            &[barrier_to_encode],
        );

        self.device.end_command_buffer(self.transfer_cb)?;

        // Submit transfer
        let submit = vk::SubmitInfo::builder()
            .command_buffers(std::slice::from_ref(&self.transfer_cb))
            .build();

        self.device.reset_fences(&[self.transfer_fence])?;
        self.submitter
            .submit_to_queue_legacy(self.transfer_queue, &[submit], self.transfer_fence)?;
        self.device
            .wait_for_fences(&[self.transfer_fence], true, u64::MAX)?;

        // Encode
        let output = self.encode_frame(
            self.source_image,
            self.source_view,
            frame_type,
        )?;

        // Build packet
        let is_keyframe = frame_type == FrameType::Idr;
        let mut data = Vec::new();

        // Prepend header on first frame or on every IDR if configured
        if is_keyframe && (display_pts == 0 || self.prepend_header) {
            data.extend_from_slice(&self.cached_header);
        }

        data.extend_from_slice(&output.data);

        Ok(EncodePacket {
            data,
            frame_type,
            pts: display_pts,
            is_keyframe,
            timestamp_ns,
        })
    }

    /// Internal implementation of encode_image (GPU-resident RGBA path).
    pub(crate) unsafe fn encode_image_internal(
        &mut self,
        rgba_image_view: vk::ImageView,
        timestamp_ns: Option<i64>,
    ) -> Result<Vec<EncodePacket>, VideoError> {
        // Lazily create the RGB→NV12 converter on first call.
        if self.rgb_to_nv12.is_none() {
            let (aligned_w, aligned_h) = self.aligned_extent();
            let codec_flag = match self.config.codec {
                Codec::H264 => vk::VideoCodecOperationFlagsKHR::ENCODE_H264,
                Codec::H265 => vk::VideoCodecOperationFlagsKHR::ENCODE_H265,
            };
            let converter = crate::rgb_to_nv12::RgbToNv12Converter::new(
                &self.ctx,
                aligned_w,
                aligned_h,
                self.compute_queue_family,
                self.compute_queue,
                self.encode_queue_family,
                codec_flag,
                self.submitter.clone(),
            )?;
            self.rgb_to_nv12 = Some(converter);
        }

        // Run RGB→NV12 conversion on the GPU.
        let converter = self.rgb_to_nv12.as_mut().unwrap();
        let (nv12_image, nv12_view) = converter.convert(rgba_image_view)?;

        // Determine frame type via GOP (B-frames not supported for GPU
        // image path since we can't buffer GPU images; promote to P).
        let mut frame_type = self.decide_frame_type();
        if frame_type == FrameType::B {
            frame_type = FrameType::P;
        }

        let display_pts = self.frame_counter;
        self.frame_counter += 1;

        // Encode from the NV12 image (already in VIDEO_ENCODE_SRC layout).
        let output = self.encode_frame(nv12_image, nv12_view, frame_type)?;

        let is_keyframe = frame_type == FrameType::Idr;
        let mut data = Vec::new();

        if is_keyframe && (display_pts == 0 || self.prepend_header) {
            data.extend_from_slice(&self.cached_header);
        }

        data.extend_from_slice(&output.data);

        Ok(vec![EncodePacket {
            data,
            frame_type,
            pts: display_pts,
            is_keyframe,
            timestamp_ns,
        }])
    }
}
