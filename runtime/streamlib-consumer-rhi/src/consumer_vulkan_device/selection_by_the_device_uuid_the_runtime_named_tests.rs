// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Choosing the enumerated device that carries the UUID the runtime named.

use super::*;

use super::consumer_vulkan_instance_and_device_selection::index_of_the_device_the_runtime_named;

fn enumerated_device(
    device_name: &str,
    uuid_byte: u8,
    device_type: vk::PhysicalDeviceType,
) -> VulkanDeviceTheLoaderEnumerates {
    VulkanDeviceTheLoaderEnumerates {
        device_name: device_name.to_owned(),
        device_uuid: [uuid_byte; 16],
        device_type,
    }
}

fn a_discrete_gpu_then_a_software_device() -> Vec<VulkanDeviceTheLoaderEnumerates> {
    vec![
        enumerated_device(
            "NVIDIA GeForce RTX 3090",
            0x94,
            vk::PhysicalDeviceType::DISCRETE_GPU,
        ),
        enumerated_device("llvmpipe", 0x6c, vk::PhysicalDeviceType::CPU),
    ]
}

/// The device the runtime named wins wherever the loader put it — the
/// software device second, behind a discrete GPU.
#[test]
fn the_device_carrying_the_named_uuid_is_chosen_wherever_it_is_enumerated() {
    let enumerated_devices = a_discrete_gpu_then_a_software_device();
    assert_eq!(
        index_of_the_device_the_runtime_named(&enumerated_devices, [0x6c; 16])
            .expect("llvmpipe carries the UUID"),
        1
    );
    assert_eq!(
        index_of_the_device_the_runtime_named(&enumerated_devices, [0x94; 16])
            .expect("the discrete GPU carries the UUID"),
        0
    );
}

#[test]
fn a_uuid_no_enumerated_device_carries_is_refused_naming_it_and_every_device_seen() {
    let refusal =
        index_of_the_device_the_runtime_named(&a_discrete_gpu_then_a_software_device(), [0x01; 16])
            .expect_err("no enumerated device carries the UUID")
            .to_string();
    for named in [
        "01010101010101010101010101010101",
        "NVIDIA GeForce RTX 3090",
        "DISCRETE_GPU",
        "94949494949494949494949494949494",
        "llvmpipe",
        "CPU",
        "6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c",
    ] {
        assert!(refusal.contains(named), "{named:?} is named in: {refusal}");
    }
}

#[test]
fn a_process_that_enumerates_no_device_refuses_saying_it_saw_none() {
    let refusal = index_of_the_device_the_runtime_named(&[], [0x94; 16])
        .expect_err("no device carries any UUID")
        .to_string();
    assert!(
        refusal.contains("94949494949494949494949494949494") && refusal.contains("saw: none"),
        "{refusal}"
    );
}
