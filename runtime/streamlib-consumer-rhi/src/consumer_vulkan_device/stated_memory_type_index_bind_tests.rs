// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The bind check of a stated memory type index against an imported resource's memory type bits.

use super::*;

/// The bind check is a pure bit test over the imported resource's own
/// `memoryTypeBits`, so it locks with no device: OPAQUE_FD carries no
/// `vkGetMemoryFdPropertiesKHR`, and binding an index the resource
/// cannot take is undefined behaviour rather than a Vulkan error.
#[test]
fn an_index_the_resource_can_bind_is_accepted_and_one_it_cannot_is_refused() {
    // types 0, 2 and 31 allowed.
    let memory_type_bits = 0b1000_0000_0000_0000_0000_0000_0000_0101u32;

    for allowed in [0u32, 2, 31] {
        assert!(
            refuse_unless_the_imported_resource_can_bind_the_stated_memory_type_index(
                "ConsumerVulkanBuffer",
                memory_type_bits,
                allowed
            )
            .is_ok(),
            "index {allowed} is set in memoryTypeBits and must bind"
        );
    }
    for refused in [1u32, 3, 30] {
        assert!(
            refuse_unless_the_imported_resource_can_bind_the_stated_memory_type_index(
                "ConsumerVulkanBuffer",
                memory_type_bits,
                refused
            )
            .is_err(),
            "index {refused} is clear in memoryTypeBits and must be refused"
        );
    }
}

/// An index at or past VK_MAX_MEMORY_TYPES names no memory type. The
/// bit test must answer "refused" rather than shift out of range —
/// `1u32 << 32` panics in debug and wraps to bit 0 in release, which
/// would let `u32::MAX` bind whatever type 0 happens to be.
#[test]
fn an_index_past_vk_max_memory_types_is_refused_rather_than_overflowing() {
    let every_type_allowed = u32::MAX;
    assert!(
        refuse_unless_the_imported_resource_can_bind_the_stated_memory_type_index(
            "ConsumerVulkanBuffer",
            every_type_allowed,
            31
        )
        .is_ok(),
        "31 is the last real memory type index"
    );
    for past_the_end in [32u32, 33, 64, u32::MAX] {
        assert!(
            refuse_unless_the_imported_resource_can_bind_the_stated_memory_type_index(
                "ConsumerVulkanBuffer",
                every_type_allowed,
                past_the_end
            )
            .is_err(),
            "index {past_the_end} names no memory type even when every bit is set"
        );
    }
}

/// The refusal has to be actionable: it names the index the exporter
/// stated and the mask the importer derived, which is the whole
/// diagnosis of an exporter/importer disagreement.
#[test]
fn the_refusal_names_the_resource_the_stated_index_and_its_memory_type_bits() {
    let refusal = refuse_unless_the_imported_resource_can_bind_the_stated_memory_type_index(
        "ConsumerVulkanBuffer",
        0b1010,
        7,
    )
    .expect_err("index 7 is clear in 0b1010");
    let refusal = refusal.to_string();
    assert!(
        refusal.contains("memory type index 7"),
        "must name the stated index: {refusal}"
    );
    assert!(
        refusal.contains("memoryTypeBits=0xa"),
        "must name what the resource can bind: {refusal}"
    );
    assert!(
        refusal.contains("ConsumerVulkanBuffer"),
        "must name the importing resource: {refusal}"
    );
}
