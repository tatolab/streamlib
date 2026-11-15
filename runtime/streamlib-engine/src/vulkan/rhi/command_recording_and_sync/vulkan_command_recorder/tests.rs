// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::rhi::{ComputeBindingSpec, ComputeKernelDescriptor, PixelBuffer};
use crate::vulkan::rhi::HostVulkanBuffer;

fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
    match HostVulkanDevice::new() {
        Ok(d) => Some(d),
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            None
        }
    }
}

fn make_storage_buffer(device: &Arc<HostVulkanDevice>, element_count: u32) -> PixelBuffer {
    let vk_buf = HostVulkanBuffer::new(device, (element_count as u64) * 4).expect("storage buffer");
    PixelBuffer::from_host_vulkan_buffer(
        Arc::new(vk_buf),
        element_count,
        1,
        4,
        crate::core::rhi::PixelFormat::Bgra32,
    )
}

fn write_buffer_u32(buf: &PixelBuffer, values: &[u32]) {
    let ptr = buf.buffer_ref().inner.mapped_ptr() as *mut u32;
    unsafe {
        std::ptr::copy_nonoverlapping(values.as_ptr(), ptr, values.len());
    }
}

fn read_buffer_u32(buf: &PixelBuffer, len: usize) -> Vec<u32> {
    let ptr = buf.buffer_ref().inner.mapped_ptr() as *const u32;
    let mut out = vec![0u32; len];
    unsafe {
        std::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), len);
    }
    out
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn record_before_begin_is_typed_error() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let mut rec = RhiCommandRecorder::new(&device, "no-begin").expect("create");
    let buf = make_storage_buffer(&device, 4);
    let err = rec
        .record_buffer_barrier(
            &buf,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::HOST,
            VulkanAccess::TRANSFER_WRITE,
            VulkanAccess::HOST_READ,
        )
        .expect_err("expected typed error");
    let msg = format!("{err}");
    assert!(msg.contains("outside an active recording"), "got: {msg}");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn double_begin_is_typed_error() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let mut rec = RhiCommandRecorder::new(&device, "double-begin").expect("create");
    rec.begin().expect("first begin");
    let err = rec.begin().expect_err("expected typed error");
    let msg = format!("{err}");
    assert!(msg.contains("already in progress"), "got: {msg}");
    // submit so Drop cleans up cleanly.
    rec.submit_and_wait().expect("submit_and_wait");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn submit_without_begin_is_typed_error() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let mut rec = RhiCommandRecorder::new(&device, "no-begin-submit").expect("create");
    let err = rec.submit().expect_err("expected typed error");
    let msg = format!("{err}");
    assert!(msg.contains("without an active recording"), "got: {msg}");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn first_begin_does_not_block_on_unsignaled_fence() {
    // `submission_in_flight=false` at construction must gate the
    // fence wait — otherwise the first `begin()` would block forever
    // on the unsignaled fence (the fence is created without
    // `SIGNALED` flag now, since the gate makes pre-signaling
    // unnecessary). Mentally revert the gate: the wait_for_fences
    // call at begin() blocks indefinitely and this test hangs.
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let mut rec = RhiCommandRecorder::new(&device, "first-begin-no-block").expect("create");
    rec.begin()
        .expect("first begin must not block on unsignaled fence");
    rec.submit_and_wait().expect("submit_and_wait");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn begin_after_submit_succeeds() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let mut rec = RhiCommandRecorder::new(&device, "begin-after-submit").expect("create");
    rec.begin().expect("begin 1");
    rec.submit_and_wait().expect("submit_and_wait");
    // Second begin should pick up the now-Idle state.
    rec.begin().expect("begin 2");
    rec.submit_and_wait().expect("submit_and_wait 2");
}

// Mirrors the camera-shape pattern: compute dispatch into a storage
// buffer, post-compute buffer barrier so HOST reads see the result,
// submit signaling a timeline at value N, wait on the timeline,
// verify mapped contents.

fn blend_descriptor(input_count: u32) -> Vec<ComputeBindingSpec> {
    let mut bindings: Vec<ComputeBindingSpec> = (0..input_count)
        .map(ComputeBindingSpec::storage_buffer)
        .collect();
    bindings.push(ComputeBindingSpec::storage_buffer(8));
    bindings
}

fn blend_spv(input_count: u32) -> &'static [u8] {
    match input_count {
        1 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_1.spv")),
        2 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_2.spv")),
        _ => panic!("unexpected input_count for record-test fixture"),
    }
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn dispatch_via_recorder_matches_direct_kernel_dispatch() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let element_count = 256u32;
    let bindings = blend_descriptor(2);
    let kernel = VulkanComputeKernel::new(
        &device,
        &ComputeKernelDescriptor {
            entry_point: "main",
            label: "recorder-test",
            spv: blend_spv(2),
            bindings: &bindings,
            push_constant_size: 4,
        },
    )
    .expect("kernel");

    let input_a = make_storage_buffer(&device, element_count);
    let input_b = make_storage_buffer(&device, element_count);
    let output = make_storage_buffer(&device, element_count);

    let pattern_a: Vec<u32> = (0..element_count).map(|i| i + 1).collect();
    let pattern_b: Vec<u32> = (0..element_count).map(|i| (i + 1) * 2).collect();
    write_buffer_u32(&input_a, &pattern_a);
    write_buffer_u32(&input_b, &pattern_b);

    kernel.set_storage_buffer_pixel(0, &input_a).expect("set 0");
    kernel.set_storage_buffer_pixel(1, &input_b).expect("set 1");
    kernel.set_storage_buffer_pixel(8, &output).expect("set 8");
    let push: [u32; 1] = [element_count];
    kernel
        .set_push_constants_value(&push)
        .expect("push constants");

    let timeline = HostVulkanTimelineSemaphore::new(device.device(), 0).expect("timeline");

    let mut rec = RhiCommandRecorder::new(&device, "recorder-test").expect("recorder");
    rec.begin().expect("begin");
    let group_count_x = element_count.div_ceil(64);
    rec.record_dispatch(&kernel, group_count_x, 1, 1)
        .expect("dispatch");
    rec.record_buffer_barrier(
        &output,
        VulkanStage::COMPUTE_SHADER,
        VulkanStage::HOST,
        VulkanAccess::SHADER_WRITE,
        VulkanAccess::HOST_READ,
    )
    .expect("post-dispatch barrier");
    rec.submit_signaling_timeline(&timeline, 1).expect("submit");

    timeline.wait(1, u64::MAX).expect("wait");

    let actual = read_buffer_u32(&output, element_count as usize);
    let expected: Vec<u32> = (0..element_count as usize)
        .map(|i| pattern_a[i] + pattern_b[i])
        .collect();
    assert_eq!(actual, expected, "recorder dispatch result mismatch");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn back_to_back_recordings_signal_distinct_timeline_values() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let element_count = 64u32;
    let bindings = blend_descriptor(1);
    let kernel = VulkanComputeKernel::new(
        &device,
        &ComputeKernelDescriptor {
            entry_point: "main",
            label: "recorder-back-to-back",
            spv: blend_spv(1),
            bindings: &bindings,
            push_constant_size: 4,
        },
    )
    .expect("kernel");

    let input = make_storage_buffer(&device, element_count);
    let output = make_storage_buffer(&device, element_count);
    let pattern: Vec<u32> = (0..element_count).map(|i| i + 1).collect();
    write_buffer_u32(&input, &pattern);

    let timeline = HostVulkanTimelineSemaphore::new(device.device(), 0).expect("timeline");
    let mut rec = RhiCommandRecorder::new(&device, "recorder-back-to-back").expect("recorder");

    for frame in 1..=3u64 {
        kernel.set_storage_buffer_pixel(0, &input).expect("set 0");
        kernel.set_storage_buffer_pixel(8, &output).expect("set 8");
        let push: [u32; 1] = [element_count];
        kernel.set_push_constants_value(&push).expect("push");

        rec.begin().expect("begin");
        let group_count_x = element_count.div_ceil(64);
        rec.record_dispatch(&kernel, group_count_x, 1, 1)
            .expect("dispatch");
        rec.record_buffer_barrier(
            &output,
            VulkanStage::COMPUTE_SHADER,
            VulkanStage::HOST,
            VulkanAccess::SHADER_WRITE,
            VulkanAccess::HOST_READ,
        )
        .expect("barrier");
        rec.submit_signaling_timeline(&timeline, frame)
            .expect("submit");

        timeline.wait(frame, u64::MAX).expect("wait");
        assert_eq!(
            timeline.current_value().expect("current_value"),
            frame,
            "timeline counter mismatch after frame {frame}"
        );
    }
}
