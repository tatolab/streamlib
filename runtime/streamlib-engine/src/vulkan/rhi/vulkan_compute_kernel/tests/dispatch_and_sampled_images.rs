// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn run_blend_kernel_for(
    device: &Arc<HostVulkanDevice>,
    input_count: u32,
    element_count: u32,
) -> (Vec<PixelBuffer>, PixelBuffer) {
    let bindings = blend_descriptor(input_count);
    let kernel = VulkanComputeKernel::new(
        device,
        &ComputeKernelDescriptor {
            entry_point: "main",
            label: "test_blend",
            spv: blend_spv(input_count),
            bindings: &bindings,
            push_constant_size: 4,
        },
    )
    .expect("kernel creation");

    let inputs: Vec<PixelBuffer> = (0..input_count)
        .map(|i| {
            let buf = make_storage_buffer(device, element_count);
            let pattern: Vec<u32> = (0..element_count).map(|j| (j + 1) * (i + 1)).collect();
            write_buffer_u32(&buf, &pattern);
            buf
        })
        .collect();
    let output = make_storage_buffer(device, element_count);

    for (i, buf) in inputs.iter().enumerate() {
        kernel
            .set_storage_buffer_pixel(i as u32, buf)
            .expect("set_storage_buffer_pixel for input");
    }
    kernel
        .set_storage_buffer_pixel(8, &output)
        .expect("set_storage_buffer_pixel for output");

    let push: [u32; 1] = [element_count];
    kernel
        .set_push_constants_value(&push)
        .expect("push constants");

    let group_count_x = element_count.div_ceil(64);
    kernel
        .dispatch(group_count_x, 1, 1)
        .expect("kernel dispatch");

    (inputs, output)
}

fn expected_blend(inputs: &[PixelBuffer], element_count: u32) -> Vec<u32> {
    let active: Vec<Vec<u32>> = inputs
        .iter()
        .map(|b| read_buffer_u32(b, element_count as usize))
        .collect();
    (0..element_count as usize)
        .map(|j| active.iter().map(|v| v[j]).sum::<u32>())
        .collect()
}

fn assert_blend_for(input_count: u32) {
    let device = vulkan_device_for_dispatch_tests();
    let elem_count = 256u32;
    let (inputs, output) = run_blend_kernel_for(&device, input_count, elem_count);
    let actual = read_buffer_u32(&output, elem_count as usize);
    let expected = expected_blend(&inputs, elem_count);
    assert_eq!(
        actual, expected,
        "blend output mismatch for input_count={input_count}"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn dispatch_matches_expected_blend_for_one_input() {
    assert_blend_for(1);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn dispatch_matches_expected_blend_for_two_inputs() {
    assert_blend_for(2);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn dispatch_matches_expected_blend_for_four_inputs() {
    assert_blend_for(4);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn dispatch_matches_expected_blend_for_eight_inputs() {
    assert_blend_for(8);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn dispatch_without_setting_bindings_fails_loud() {
    let device = vulkan_device_for_dispatch_tests();
    let bindings = blend_descriptor(2);
    let kernel = VulkanComputeKernel::new(
        &device,
        &ComputeKernelDescriptor {
            entry_point: "main",
            label: "missing-set",
            spv: blend_spv(2),
            bindings: &bindings,
            push_constant_size: 4,
        },
    )
    .expect("kernel creation");
    kernel
        .set_push_constants_value(&[1u32])
        .expect("push constants");
    let err = kernel
        .dispatch(1, 1, 1)
        .expect_err("expected dispatch failure");
    let msg = format!("{err}");
    assert!(
        msg.contains("not set before dispatch"),
        "expected missing-binding error, got: {msg}"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn dispatch_completes_within_reasonable_budget() {
    let device = vulkan_device_for_dispatch_tests();
    // Performance smoke: a small kernel build + first dispatch should
    // round-trip in well under a couple seconds on any reasonable GPU.
    // Catches catastrophic regressions like accidentally recreating
    // Vulkan objects per dispatch. The budget is loose enough to absorb
    // queue-mutex contention from sibling kernel tests running in
    // parallel and cold-pipeline-cache compilation on the first run.
    let elem_count = 4096u32;
    let start = std::time::Instant::now();
    let (_inputs, _output) = run_blend_kernel_for(&device, 8, elem_count);
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(1500),
        "kernel build + first dispatch took {elapsed:?} (>1500ms); regression?"
    );
}

// ---- Sampled-image binding kind tests ------------------------------------

const SAMPLED_IMAGE_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/test_sampled_image.spv"));

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn sampled_image_binding_dispatches_with_raw_view() {
    let device = vulkan_device_for_dispatch_tests();

    let bindings = [
        ComputeBindingSpec::sampled_image(0),
        ComputeBindingSpec::storage_image(1),
    ];
    let kernel = VulkanComputeKernel::new(
        &device,
        &ComputeKernelDescriptor {
            entry_point: "main",
            label: "test-sampled-image",
            spv: SAMPLED_IMAGE_SPV,
            bindings: &bindings,
            push_constant_size: 0,
        },
    )
    .expect("kernel construction with SampledImage binding");

    // Lock the reflected kind so the SPIR-V → ComputeBindingKind path
    // continues to produce SampledImage. Reverts to SampledTexture
    // would be silently wrong before this test ran.
    assert!(
        kernel
            .bindings()
            .iter()
            .any(|s| s.binding == 0 && s.kind == ComputeBindingKind::SampledImage),
        "expected binding 0 reflected as SampledImage, got {:?}",
        kernel.bindings()
    );

    // Build a tiny 1x1 RGBA8 SAMPLED image (the input) and a 1x1
    // RGBA8 STORAGE image (the output). Each gets its own VkImageView.
    let device_handle = device.device().clone();
    let allocator = device.allocator().clone();
    let make_image =
        |usage: vk::ImageUsageFlags| -> (vk::Image, vulkanalia_vma::Allocation, vk::ImageView) {
            let info = vk::ImageCreateInfo::builder()
                .image_type(vk::ImageType::_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
                .extent(vk::Extent3D {
                    width: 1,
                    height: 1,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(usage)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .build();
            let alloc_opts = vulkanalia_vma::AllocationOptions {
                required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
                ..Default::default()
            };
            use vulkanalia_vma::Alloc;
            let (image, alloc) =
                unsafe { allocator.create_image(info, &alloc_opts) }.expect("create_image");
            let view_info = vk::ImageViewCreateInfo::builder()
                .image(image)
                .view_type(vk::ImageViewType::_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .build();
            let view = unsafe { device_handle.create_image_view(&view_info, None) }
                .expect("create_image_view");
            (image, alloc, view)
        };

    let (in_image, in_alloc, in_view) =
        make_image(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST);
    let (out_image, out_alloc, out_view) = make_image(vk::ImageUsageFlags::STORAGE);

    // Transition both images out of UNDEFINED so the shader can sample
    // / write without producing undefined contents on debug drivers.
    // One-shot command buffer.
    use vulkanalia::vk::HasBuilder;
    let queue_family_index = device.queue_family_index();
    let pool_info = vk::CommandPoolCreateInfo::builder()
        .queue_family_index(queue_family_index)
        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
        .build();
    let pool = unsafe { device_handle.create_command_pool(&pool_info, None) }
        .expect("create_command_pool");
    let alloc_info = vk::CommandBufferAllocateInfo::builder()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1)
        .build();
    let cb = unsafe { device_handle.allocate_command_buffers(&alloc_info) }
        .expect("allocate_command_buffers")[0];
    let begin = vk::CommandBufferBeginInfo::builder()
        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
        .build();
    unsafe { device_handle.begin_command_buffer(cb, &begin) }.expect("begin_command_buffer");

    let barriers = [
        vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
            .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(in_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .build(),
        vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
            .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(out_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .build(),
    ];
    let dep = vk::DependencyInfo::builder()
        .image_memory_barriers(&barriers)
        .build();
    unsafe { device_handle.cmd_pipeline_barrier2(cb, &dep) };
    unsafe { device_handle.end_command_buffer(cb) }.expect("end_command_buffer");

    let fence_info = vk::FenceCreateInfo::builder().build();
    let fence = unsafe { device_handle.create_fence(&fence_info, None) }.expect("create_fence");
    let cb_submit = vk::CommandBufferSubmitInfo::builder()
        .command_buffer(cb)
        .build();
    let cb_submits = [cb_submit];
    let submit = vk::SubmitInfo2::builder()
        .command_buffer_infos(&cb_submits)
        .build();
    unsafe {
        device
            .submit_to_queue(device.queue(), &[submit], fence)
            .expect("submit_to_queue");
    }
    unsafe {
        device_handle
            .wait_for_fences(&[fence], true, u64::MAX)
            .expect("wait_for_fences");
        device_handle.destroy_fence(fence, None);
        device_handle.destroy_command_pool(pool, None);
    }

    // Bind via the raw-view setters and dispatch. The shader is a
    // single-thread no-op-ish texelFetch — what we lock is that
    // descriptor-layout + descriptor write + bind + dispatch run
    // end-to-end without validation errors.
    kernel
        .set_sampled_image_view(0, in_view, SampledImageReadLayout::ShaderReadOnlyOptimal)
        .expect("set_sampled_image_view");
    kernel
        .set_storage_image_view(1, out_view)
        .expect("set_storage_image_view");
    kernel.dispatch(1, 1, 1).expect("dispatch");

    // Cleanup the test resources we allocated outside the kernel.
    unsafe {
        device_handle.destroy_image_view(in_view, None);
        device_handle.destroy_image_view(out_view, None);

        allocator.destroy_image(in_image, in_alloc);
        allocator.destroy_image(out_image, out_alloc);
    }
}
