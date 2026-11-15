// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(target_os = "linux")]
use super::*;
#[cfg(target_os = "linux")]
use crate::vulkan::rhi::device::drm_modifier_probe;

/// Set in the child process a device-selection test re-runs in; names the
/// device the child must build on.
#[cfg(target_os = "linux")]
const EXPECTED_SELECTED_VULKAN_DEVICE_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_TEST_EXPECTED_SELECTED_VULKAN_DEVICE";

/// The device a device-selection test's child process must build on.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
enum ExpectedSelectedVulkanDevice {
    /// The hardware device, though the loader lists lavapipe first.
    HardwareListedAfterLavapipe,
    /// Lavapipe, the only device visible.
    LavapipeAlone,
}

#[cfg(target_os = "linux")]
impl ExpectedSelectedVulkanDevice {
    fn as_environment_value(self) -> &'static str {
        match self {
            Self::HardwareListedAfterLavapipe => "hardware-listed-after-lavapipe",
            Self::LavapipeAlone => "lavapipe-alone",
        }
    }

    fn from_this_process_environment() -> Option<Self> {
        let value = std::env::var_os(EXPECTED_SELECTED_VULKAN_DEVICE_ENVIRONMENT_VARIABLE)?;
        [Self::HardwareListedAfterLavapipe, Self::LavapipeAlone]
            .into_iter()
            .find(|expected| value == expected.as_environment_value())
            .or_else(|| panic!("no expected device is named {value:?}"))
    }
}

/// The ICD manifest whose file name starts with `manifest_file_name_prefix`,
/// from the loader variables this process carries, else the loader's
/// system search directories.
#[cfg(target_os = "linux")]
fn an_icd_manifest_named(manifest_file_name_prefix: &str) -> Option<std::path::PathBuf> {
    let named_by_a_loader_variable = ["VK_ADD_DRIVER_FILES", "VK_DRIVER_FILES"]
        .into_iter()
        .filter_map(std::env::var_os)
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>());
    let in_a_system_search_directory = [
        "/etc/vulkan/icd.d",
        "/usr/local/share/vulkan/icd.d",
        "/usr/share/vulkan/icd.d",
    ]
    .into_iter()
    .filter_map(|directory| std::fs::read_dir(directory).ok())
    .flatten()
    .filter_map(|entry| entry.ok().map(|entry| entry.path()));
    named_by_a_loader_variable
        .chain(in_a_system_search_directory)
        .find(|manifest| {
            manifest
                .file_name()
                .and_then(|file_name| file_name.to_str())
                .is_some_and(|file_name| file_name.starts_with(manifest_file_name_prefix))
                && manifest.is_file()
        })
}

/// In the child: build the device under the `VK_DRIVER_FILES` the parent
/// set and hold it to what the parent expects of it.
#[cfg(target_os = "linux")]
fn the_device_this_process_builds_on_is_the_expected_one(
    expected_device: ExpectedSelectedVulkanDevice,
) {
    if let ExpectedSelectedVulkanDevice::HardwareListedAfterLavapipe = expected_device {
        let vulkan_instance = HostVulkanInstance::new().expect("the instance creates");
        let first_listed_device = unsafe {
            let physical_devices = vulkan_instance
                .instance
                .enumerate_physical_devices()
                .expect("the loader enumerates");
            vulkan_instance
                .instance
                .get_physical_device_properties(physical_devices[0])
        };
        assert_eq!(
            first_listed_device.device_type,
            vk::PhysicalDeviceType::CPU,
            "the loader must list lavapipe first for this test to prove the engine's own \
             CPU-type-last order"
        );
    }

    let device = HostVulkanDevice::new().expect("a device builds under these driver files");
    let device_name = device.name();
    match expected_device {
        ExpectedSelectedVulkanDevice::HardwareListedAfterLavapipe => {
            assert!(
                !device.device_is_software(),
                "the engine built on {device_name}, a software device, with a hardware \
                 device visible"
            );
            assert!(
                device.supports_semaphore_export(),
                "{device_name} exports a timeline semaphore"
            );
            assert!(
                device.supports_dma_buf_export(),
                "{device_name} exports DMA-BUF"
            );
        }
        ExpectedSelectedVulkanDevice::LavapipeAlone => {
            assert!(
                device.device_is_software(),
                "{device_name} must report itself software"
            );
            assert!(
                device_name.contains("llvmpipe"),
                "the only visible device is lavapipe, got {device_name}"
            );
            assert!(
                !device.supports_semaphore_export(),
                "lavapipe exports no semaphore"
            );
            assert!(
                !device.supports_dma_buf_export(),
                "a software device exports no DMA-BUF, whatever /dev/udmabuf permits"
            );
            assert!(
                device
                    .drm_modifier_table()
                    .rt_modifiers(drm_modifier_probe::fourcc::DRM_FORMAT_ARGB8888)
                    .is_empty(),
                "a device that exports no DMA-BUF takes no EGL modifier table"
            );
        }
    }
}

/// Re-run `test_path` in a child process that sees only
/// `vulkan_driver_files`, in the order the loader enumerates them, and
/// expects `expected_device`.
#[cfg(target_os = "linux")]
fn rerun_expecting_the_selected_device(
    test_path: &str,
    vulkan_driver_files: &[std::path::PathBuf],
    expected_device: ExpectedSelectedVulkanDevice,
) {
    let vulkan_driver_files =
        std::env::join_paths(vulkan_driver_files).expect("ICD manifest paths join");
    // Both switches off the reordering a loader and Mesa's implicit
    // device-select layer apply, so the engine sees the drivers in the
    // order the loader enumerates them: the reverse of the list.
    let child = crate::core::test_support::rerun_this_test_in_a_child_process_with_environment(
        test_path,
        &[
            ("VK_DRIVER_FILES", vulkan_driver_files.as_os_str()),
            ("VK_LOADER_DISABLE_SELECT", std::ffi::OsStr::new("1")),
            ("NODEVICE_SELECT", std::ffi::OsStr::new("1")),
            (
                EXPECTED_SELECTED_VULKAN_DEVICE_ENVIRONMENT_VARIABLE,
                std::ffi::OsStr::new(expected_device.as_environment_value()),
            ),
        ],
    );
    assert!(
        child.status.success(),
        "the child process built on the wrong device or refused: {}\n{}\n{}",
        child.status,
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr),
    );
}

/// The loader lists lavapipe first, its own sorting switched off; the
/// engine still builds on the hardware device, which exports DMA-BUF and
/// a timeline semaphore.
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn with_lavapipe_listed_before_the_nvidia_driver_the_engine_builds_on_the_nvidia_device() {
    if let Some(expected_device) = ExpectedSelectedVulkanDevice::from_this_process_environment() {
        the_device_this_process_builds_on_is_the_expected_one(expected_device);
        return;
    }
    let lavapipe_manifest =
        an_icd_manifest_named("lvp_icd").expect("the rig carries Mesa's lavapipe");
    // The hardware device this asserts on is the one that exports DMA-BUF
    // with modifiers; a machine with no hardware driver has none to list.
    let Some(nvidia_manifest) = an_icd_manifest_named("nvidia_icd") else {
        skip_because_this_machine_lacks(RigOnlyCapability::DmaBufModifiers);
        return;
    };

    rerun_expecting_the_selected_device(
        "vulkan::rhi::device::vulkan_device::tests::expected_selected_device::with_lavapipe_listed_before_the_nvidia_driver_the_engine_builds_on_the_nvidia_device",
        &[nvidia_manifest, lavapipe_manifest],
        ExpectedSelectedVulkanDevice::HardwareListedAfterLavapipe,
    );
}

/// With lavapipe the only driver the engine builds on it, and the
/// snapshot says software, no DMA-BUF and no semaphore export.
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn with_only_lavapipe_visible_the_engine_builds_on_a_software_device_that_exports_neither_dma_buf_nor_a_semaphore()
 {
    if let Some(expected_device) = ExpectedSelectedVulkanDevice::from_this_process_environment() {
        the_device_this_process_builds_on_is_the_expected_one(expected_device);
        return;
    }
    let lavapipe_manifest =
        an_icd_manifest_named("lvp_icd").expect("the rig carries Mesa's lavapipe");

    rerun_expecting_the_selected_device(
        "vulkan::rhi::device::vulkan_device::tests::expected_selected_device::with_only_lavapipe_visible_the_engine_builds_on_a_software_device_that_exports_neither_dma_buf_nor_a_semaphore",
        &[lavapipe_manifest],
        ExpectedSelectedVulkanDevice::LavapipeAlone,
    );
}
