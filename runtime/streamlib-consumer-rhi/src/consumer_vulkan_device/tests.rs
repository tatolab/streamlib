// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Building a consumer device on a real GPU, its queue, and its queue family ownership transfers.

use super::*;
use std::sync::Arc;

/// Try to construct a ConsumerVulkanDevice; return None if Vulkan
/// is unavailable in this environment (CI sandboxes, no GPU).
fn try_create() -> Option<Arc<ConsumerVulkanDevice>> {
    match consumer_vulkan_device_on_the_first_device_the_loader_enumerates() {
        Ok(d) => Some(Arc::new(d)),
        Err(e) => {
            tracing::warn!("skipping — ConsumerVulkanDevice unavailable: {e}");
            None
        }
    }
}

#[test]
fn consumer_device_constructs() {
    let device = match try_create() {
        Some(d) => d,
        None => return,
    };
    assert!(!device.name().is_empty());
    // Sanity: the trait bound is wired to ConsumerMarker, not Host.
    fn assert_consumer<D: VulkanRhiDevice<Privilege = ConsumerMarker>>(_: &D) {}
    assert_consumer(&*device);
}

/// Run on a machine whose loader enumerates more than one device — the
/// rig's NVIDIA GPU beside the system lavapipe — and every device is the
/// one built on when the runtime names it, proven by its UUID.
#[test]
fn a_helper_on_a_machine_with_two_devices_imports_on_the_one_the_runtime_named() {
    let enumerated_devices = match vulkan_devices_the_loader_enumerates() {
        Ok(enumerated_devices) if enumerated_devices.len() >= 2 => enumerated_devices,
        Ok(enumerated_devices) => {
            tracing::info!(
                "Skipping test — the loader enumerates {} device(s), not two",
                enumerated_devices.len()
            );
            return;
        }
        Err(unavailable) => {
            tracing::info!("Skipping test — no Vulkan loader: {unavailable}");
            return;
        }
    };
    for named_device in &enumerated_devices {
        let device =
            ConsumerVulkanDevice::new_on_the_device_the_runtime_named(named_device.device_uuid)
                .unwrap_or_else(|refusal| {
                    panic!("a device on {}: {refusal}", named_device.device_name)
                });
        assert_eq!(device.physical_device_uuid(), named_device.device_uuid);
        assert_eq!(device.name(), named_device.device_name);
        assert_eq!(
            device.device_is_software(),
            named_device.device_type == vk::PhysicalDeviceType::CPU
        );
    }
}

#[test]
fn a_uuid_no_enumerated_device_carries_is_refused_before_any_device_is_built() {
    let enumerated_devices = match vulkan_devices_the_loader_enumerates() {
        Ok(enumerated_devices) => enumerated_devices,
        Err(unavailable) => {
            tracing::info!("Skipping test — no Vulkan loader: {unavailable}");
            return;
        }
    };
    let uuid_no_device_carries = (0..=u8::MAX)
        .map(|byte| [byte; 16])
        .find(|candidate| {
            enumerated_devices
                .iter()
                .all(|enumerated_device| enumerated_device.device_uuid != *candidate)
        })
        .expect("a UUID no enumerated device carries");
    let refusal =
        match ConsumerVulkanDevice::new_on_the_device_the_runtime_named(uuid_no_device_carries) {
            Ok(device) => panic!(
                "a UUID no device carries built a device on {}",
                device.name()
            ),
            Err(refusal) => refusal.to_string(),
        };
    assert!(
        refusal.contains(&crate::lowercase_hex_of_vulkan_device_uuid(
            uuid_no_device_carries
        )),
        "{refusal}"
    );
    for enumerated_device in &enumerated_devices {
        assert!(
            refusal.contains(&enumerated_device.device_name),
            "{refusal}"
        );
    }
}

#[test]
fn consumer_device_exposes_queue() {
    let device = match try_create() {
        Some(d) => d,
        None => return,
    };
    let _q = device.queue();
    let _qfi = device.queue_family_index();
    // Trait method also returns the same queue.
    let _q_via_trait = <ConsumerVulkanDevice as VulkanRhiDevice>::queue(&device);
}

/// `supports_qfot_acquire_unmodified` should equal the
/// `has_acquire_unmodified` extension flag collected at
/// construction. The trait-method route must agree with
/// the inherent method.
#[test]
fn supports_qfot_acquire_unmodified_consistent_across_inherent_and_trait() {
    let device = match try_create() {
        Some(d) => d,
        None => return,
    };
    let inherent = ConsumerVulkanDevice::supports_qfot_acquire_unmodified(&device);
    let via_trait =
        <ConsumerVulkanDevice as VulkanRhiDevice>::supports_qfot_acquire_unmodified(&device);
    assert_eq!(inherent, via_trait, "trait and inherent reports must agree");
    assert_eq!(
        inherent, device.has_acquire_unmodified,
        "supports_qfot_acquire_unmodified must equal has_acquire_unmodified \
         (VK_QUEUE_FAMILY_EXTERNAL is core 1.1 and always available)"
    );
}

/// The rule holds on the live device, not just in the partition: a
/// software device imports no DMA-BUF whatever its driver advertises,
/// while still coming up with the OPAQUE_FD import every pooled frame
/// there takes.
#[test]
fn a_software_device_comes_up_without_dma_buf_or_modifier_import() {
    let device = match try_create() {
        Some(d) => d,
        None => return,
    };
    if !device.device_is_software() {
        return;
    }
    assert!(!device.supports_dma_buf_memory_import());
    assert!(!device.supports_drm_format_modifier_image_import());
    let refusal = device
        .import_dma_buf_memory(-1, 4096, u32::MAX, vk::MemoryPropertyFlags::HOST_VISIBLE)
        .expect_err("a software device imports no DMA-BUF")
        .to_string();
    assert!(
        refusal.contains("VK_EXT_external_memory_dma_buf") && refusal.contains(device.name()),
        "the refusal names the extension and the device: {refusal}"
    );
}

/// QFOT acquire is a no-op when target is UNDEFINED — verifies
/// the early-return short-circuit at the top of
/// [`ConsumerVulkanDevice::acquire_from_foreign`]. **Honest
/// scope**: this asserts only that the call returns `Ok`, not
/// that no GPU work was issued. Reverting the early return
/// makes the helper attempt a barrier with `image = vk::Image::null()`;
/// validation layers reject that
/// (`VUID-VkImageMemoryBarrier2-image-parameter`), but raw
/// drivers may tolerate it silently. Real GPU-correctness for
/// QFOT comes from E2E scenarios (polyglot examples that hit
/// Path 2 with a real imported image, run with
/// `VK_LOADER_LAYERS_ENABLE=*validation*`).
#[test]
fn acquire_from_foreign_undefined_target_is_noop() {
    let device = match try_create() {
        Some(d) => d,
        None => return,
    };
    // A null VkImage is fine here because the no-op short-circuits
    // before any handle is dereferenced.
    let result = device.acquire_from_foreign(vk::Image::null(), vk::ImageLayout::UNDEFINED);
    assert!(
        result.is_ok(),
        "acquire_from_foreign with target=UNDEFINED must short-circuit Ok"
    );
}
