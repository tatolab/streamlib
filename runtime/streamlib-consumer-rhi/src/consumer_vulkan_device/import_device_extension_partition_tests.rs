// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Which import device extensions a device enables, given what it advertises and whether it is software.

use super::*;

const REQUIRED: &[&CStr] = &[c"VK_KHR_external_memory", c"VK_KHR_external_memory_fd"];
const PROBED: &[&CStr] = &[
    c"VK_EXT_external_memory_dma_buf",
    c"VK_EXT_image_drm_format_modifier",
    c"VK_KHR_external_semaphore_fd",
];

/// A driver advertising the fd extensions but no DMA-BUF still yields a
/// device; the DMA-BUF arms refuse later, by name.
#[test]
fn a_driver_without_dma_buf_enables_only_the_required_extensions() {
    let advertised = [
        c"VK_KHR_external_memory",
        c"VK_KHR_external_memory_fd",
        c"VK_KHR_swapchain",
    ];
    assert_eq!(
        import_device_extensions_to_enable(&advertised, REQUIRED, PROBED, &[]),
        Ok(REQUIRED.to_vec())
    );
}

/// Lavapipe advertises DMA-BUF and modifiers where `/dev/udmabuf` is
/// reachable; a software device enables neither, so the rig and a
/// container import the same flavours.
#[test]
fn a_software_device_advertising_dma_buf_enables_neither_dma_buf_nor_modifiers() {
    let advertised = [
        c"VK_KHR_external_memory",
        c"VK_KHR_external_memory_fd",
        c"VK_EXT_external_memory_dma_buf",
        c"VK_EXT_image_drm_format_modifier",
        c"VK_KHR_external_semaphore_fd",
    ];
    assert_eq!(
        import_device_extensions_to_enable(
            &advertised,
            REQUIRED,
            PROBED,
            PROBED_DEVICE_EXTENSIONS_WITHHELD_FROM_A_SOFTWARE_DEVICE
        ),
        Ok(vec![
            c"VK_KHR_external_memory",
            c"VK_KHR_external_memory_fd",
            c"VK_KHR_external_semaphore_fd",
        ])
    );
}

#[test]
fn every_advertised_probed_extension_is_enabled_and_no_other() {
    let advertised = [
        c"VK_KHR_external_semaphore_fd",
        c"VK_KHR_external_memory_fd",
        c"VK_EXT_external_memory_dma_buf",
        c"VK_KHR_external_memory",
    ];
    assert_eq!(
        import_device_extensions_to_enable(&advertised, REQUIRED, PROBED, &[]),
        Ok(vec![
            c"VK_KHR_external_memory",
            c"VK_KHR_external_memory_fd",
            c"VK_EXT_external_memory_dma_buf",
            c"VK_KHR_external_semaphore_fd",
        ])
    );
}

#[test]
fn a_missing_required_extension_is_named_as_the_refusal() {
    let advertised = [c"VK_KHR_external_memory", c"VK_EXT_external_memory_dma_buf"];
    assert_eq!(
        import_device_extensions_to_enable(&advertised, REQUIRED, PROBED, &[]),
        Err(c"VK_KHR_external_memory_fd")
    );
}

/// The platform partition itself: the DMA-BUF, modifier and semaphore-fd
/// extensions are never what decides whether a Linux device exists.
#[cfg(target_os = "linux")]
#[test]
fn on_linux_only_the_external_memory_fd_extensions_are_required() {
    assert_eq!(REQUIRED_DEVICE_EXTENSIONS, REQUIRED);
    assert_eq!(PROBED_DEVICE_EXTENSIONS, PROBED);
}
