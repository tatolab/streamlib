// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// The message carries the requested import path verbatim — the caller
/// asked for a class by that name, and matching their spelling is what
/// makes a typo findable. The install fix-it died with the module system.
#[test]
fn unknown_processor_type_names_the_class_that_is_not_registered() {
    let msg = Error::UnknownProcessorType {
        ident: ProcessorClassImportPath::new("my_app.filters:BlurProcessor").unwrap(),
    }
    .to_string();
    assert!(
        msg.contains("my_app.filters:BlurProcessor"),
        "the requested path must reach the user unaltered: {msg}"
    );
    assert!(
        !msg.contains("streamlib add"),
        "no install fix-it survives the module-system removal: {msg}"
    );
}

#[test]
fn a_describe_refusal_names_every_type_it_refused_and_why() {
    let msg = Error::NodeTypesNotDescribed {
        node_types: vec![
            ProcessorClassImportPath::new("my_app.filters:Blur").unwrap(),
            ProcessorClassImportPath::new("my_app.filters:Sharpen").unwrap(),
        ],
        refusal: "the interpreter printed no describe document".to_string(),
    }
    .to_string();
    assert_eq!(
        msg,
        "cannot describe `my_app.filters:Blur`, `my_app.filters:Sharpen`: the interpreter \
         printed no describe document"
    );
}

/// A stream names no runtime version, so the refusal of a type this runtime
/// lacks names the runtime's own, to compare with the stream's library.
#[test]
fn an_unknown_node_type_names_this_runtimes_own_version() {
    let msg = Error::UnknownProcessorType {
        ident: ProcessorClassImportPath::new("tatolab.stream:NewThing").unwrap(),
    }
    .to_string();
    assert_eq!(
        msg,
        format!(
            "this runtime ({}) has no node type `tatolab.stream:NewThing`",
            env!("CARGO_PKG_VERSION")
        )
    );
}

/// These two words cross the helper-process boundary: the engine writes
/// one into the `unwire_link` command and the child branches on it to pick
/// which of its own ports to release. A drift here is silent on both sides
/// — the child logs an unknown direction and the port stays leaked — so
/// the spelling is pinned literally rather than derived from the variant.
#[test]
fn port_direction_wire_spelling_is_pinned() {
    assert_eq!(PortDirection::Input.as_wire_str(), "input");
    assert_eq!(PortDirection::Output.as_wire_str(), "output");
    // Display is the same words, so a log and a frame never disagree.
    assert_eq!(PortDirection::Input.to_string(), "input");
    assert_eq!(PortDirection::Output.to_string(), "output");
}

#[test]
fn a_frame_allocation_refused_past_a_heap_budget_reads_its_heap_budget_usage_and_pool_bytes() {
    let msg = Error::FrameAllocationRefusedPastTheDeviceMemoryHeapBudget {
        heap_index: 1,
        heap_locality: DeviceMemoryHeapLocality::NotDeviceLocal,
        heap_budget_bytes: 8_000_000_000,
        heap_usage_bytes: 7_990_000_000,
        bytes_this_runtimes_allocator_pools_hold_on_the_heap: 1_200_000_000,
        requested_bytes: 3_686_400,
        headroom_bytes: 67_108_864,
        heap_budget_source:
            DeviceMemoryHeapBudgetSource::EstimatedAndCappedByTheSystemsAvailableMemory,
        frame_allocation_site: "HostVulkanBuffer::new".to_string(),
    }
    .to_string();
    assert_eq!(
        msg,
        "HostVulkanBuffer::new refused a 3686400-byte frame allocation before allocating: \
         device memory heap 1 (not device-local) has a budget of 8000000000 bytes (estimated \
         as 80% of the heap, the driver reporting no budget, and capped by the system's \
         available memory) and uses 7990000000 bytes, of which this runtime's allocator pools \
         hold 1200000000 bytes; the allocation would leave less than the 67108864-byte headroom"
    );
}

#[test]
fn a_refused_one_off_acquisition_reads_its_shape_and_the_count() {
    let msg = Error::EveryOneOffAcquisitionOfThisShapeTheNodeMayHoldIsHeld {
        acquisition_shape: "1280x720 rgba8_unorm texture (usage copy_src|copy_dst|texture_binding)"
            .to_string(),
        one_off_acquisitions_one_node_may_hold_of_one_shape: 16,
    }
    .to_string();
    assert_eq!(
        msg,
        "this node already holds 16 one-off acquisitions of 1280x720 rgba8_unorm texture \
         (usage copy_src|copy_dst|texture_binding), the most one node may hold of one shape at \
         once, so this one is refused before anything is allocated; release one it holds, or \
         publish per-frame output from a node output pool"
    );
}

#[test]
fn a_frame_allocation_refused_near_the_allocation_limit_reads_the_count_and_the_limit() {
    let msg = Error::FrameAllocationRefusedNearTheDeviceMemoryAllocationCountLimit {
        live_device_memory_allocation_count: 3_970,
        max_memory_allocation_count: 4_096,
        allocations_kept_in_reserve: 128,
        frame_allocation_site: "HostVulkanTexture::new".to_string(),
    }
    .to_string();
    assert_eq!(
        msg,
        "HostVulkanTexture::new refused a frame allocation before allocating: this process \
         holds 3970 device memory allocations of the 4096 the device allows, fewer than 128 \
         short of its limit"
    );
}

#[test]
fn a_frame_allocation_refused_past_the_metal_working_set_reads_the_working_set_and_its_usage() {
    let msg = Error::FrameAllocationRefusedPastTheMetalWorkingSetBudget {
        recommended_max_working_set_bytes: 11_453_251_584,
        metal_current_allocated_bytes: 11_400_000_000,
        requested_bytes: 4_194_304,
        headroom_bytes: 67_108_864,
        frame_allocation_site: "HostVulkanTexture::new_iosurface_backed".to_string(),
    }
    .to_string();
    assert_eq!(
        msg,
        "HostVulkanTexture::new_iosurface_backed refused a 4194304-byte frame allocation \
         before allocating: Metal's recommended working set is 11453251584 bytes and the \
         Metal device has allocated 11400000000 bytes; the allocation would leave less than \
         the 67108864-byte headroom"
    );
}

#[test]
fn a_device_memory_heap_reads_its_locality_and_where_its_budget_came_from() {
    assert_eq!(
        DeviceMemoryHeapLocality::DeviceLocal.to_string(),
        "device-local"
    );
    assert_eq!(
        DeviceMemoryHeapLocality::NotDeviceLocal.to_string(),
        "not device-local"
    );
    assert_eq!(
        DeviceMemoryHeapBudgetSource::ReportedByTheDriver.to_string(),
        "reported by the driver"
    );
    assert_eq!(
        DeviceMemoryHeapBudgetSource::EstimatedAsEightyPercentOfTheHeap.to_string(),
        "estimated as 80% of the heap, the driver reporting no budget"
    );
}
