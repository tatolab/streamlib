// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// `into_parts` hands the buffer and its allocation to the caller, but
/// the device reference it held must still be released, or that device
/// and every pool on it are never dropped.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn an_acceleration_structure_buffer_taken_apart_releases_its_device() {
    let device = match HostVulkanDevice::new() {
        Ok(d) => d,
        Err(e) => {
            println!("Skipping — no Vulkan device: {e}");
            return;
        }
    };
    let device_references_before_the_buffer = Arc::strong_count(&device);

    let buffer = AsBuffer::new(
        &device,
        64,
        vk::BufferUsageFlags::TRANSFER_DST,
        "into-parts-under-test",
    )
    .expect("a 64-byte buffer allocates");
    assert_eq!(
        Arc::strong_count(&device),
        device_references_before_the_buffer + 1,
        "the buffer holds the device it allocated from"
    );

    let (vk_buffer, allocation, _device_address) = buffer.into_parts();
    assert_eq!(
        Arc::strong_count(&device),
        device_references_before_the_buffer,
        "taking the buffer apart must release its device reference"
    );
    unsafe { device.allocator().destroy_buffer(vk_buffer, allocation) };
}

#[test]
fn every_defined_geometry_instance_flag_survives_the_raw_bitmask() {
    let defined = vk::GeometryInstanceFlagsKHR::all();
    assert_eq!(
        geometry_instance_flags_from_raw_bitmask(defined.bits()).expect("every defined bit"),
        defined
    );
    assert_eq!(
        geometry_instance_flags_from_raw_bitmask(vk::GeometryInstanceFlagsKHR::FORCE_OPAQUE.bits())
            .expect("one defined bit"),
        vk::GeometryInstanceFlagsKHR::FORCE_OPAQUE
    );
}

#[test]
fn every_index_inside_the_supplied_vertices_is_accepted() {
    refuse_an_index_past_the_last_vertex("in-range", 3, &[0, 1, 2])
        .expect("three indices over three vertices name only vertices that exist");
}

#[test]
fn an_index_past_the_last_vertex_is_refused() {
    let refusal = refuse_an_index_past_the_last_vertex("one-past-the-end", 3, &[0, 1, 3])
        .expect_err("index 3 over three vertices reads past the vertex buffer");
    let message = refusal.to_string();
    assert!(
        message.contains("index 3") && message.contains("outside the 3 supplied"),
        "the refusal must name the index and the vertex count: {message}"
    );
    assert!(
        message.contains("one-past-the-end"),
        "the refusal must name the acceleration structure: {message}"
    );
}

#[test]
fn a_bit_no_geometry_instance_flag_owns_is_refused() {
    let undefined_bit = !vk::GeometryInstanceFlagsKHR::all().bits() & (1 << 31);
    assert_ne!(undefined_bit, 0, "bit 31 must stay undefined for this test");
    let refusal = geometry_instance_flags_from_raw_bitmask(
        vk::GeometryInstanceFlagsKHR::FORCE_OPAQUE.bits() | undefined_bit,
    )
    .expect_err("a bit no flag owns cannot be silently dropped");
    let message = refusal.to_string();
    assert!(
        message.contains("set a bit no VkGeometryInstanceFlagsKHR value owns"),
        "the refusal must say the bit is unowned: {message}"
    );
    assert!(
        message.contains(&format!(
            "{:#x}",
            vk::GeometryInstanceFlagsKHR::all().bits()
        )),
        "the refusal must name the bits that are defined: {message}"
    );
}
