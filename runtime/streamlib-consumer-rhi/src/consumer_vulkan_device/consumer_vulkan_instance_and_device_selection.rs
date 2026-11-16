// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The consumer's own `VkInstance`, the physical devices the loader enumerates on it, and the choice of the one the runtime named.

use super::*;

/// A `VkInstance` destroyed when dropped unless [`Self::keep`] hands it on, so
/// every early return of a device's construction tears it down.
pub(super) struct VulkanInstanceDestroyedUnlessKept(vulkanalia::Instance);

impl VulkanInstanceDestroyedUnlessKept {
    pub(super) fn keep(self) -> vulkanalia::Instance {
        std::mem::ManuallyDrop::new(self).0.clone()
    }
}

impl std::ops::Deref for VulkanInstanceDestroyedUnlessKept {
    type Target = vulkanalia::Instance;

    fn deref(&self) -> &vulkanalia::Instance {
        &self.0
    }
}

impl Drop for VulkanInstanceDestroyedUnlessKept {
    fn drop(&mut self) {
        unsafe { self.0.destroy_instance(None) };
    }
}

#[doc(hidden)]
pub fn vulkan_device_uuid_of_the_first_device_the_loader_enumerates() -> Result<[u8; 16]> {
    vulkan_devices_the_loader_enumerates()?
        .into_iter()
        .next()
        .map(|first_enumerated_device| first_enumerated_device.device_uuid)
        .ok_or_else(|| ConsumerRhiError::Gpu("No Vulkan physical devices found".into()))
}

/// The loader's library, entry and the minimal instance a consumer device
/// enumerates on: no surface extensions, since the consumer never renders to
/// a window.
pub(super) fn create_the_consumer_vulkan_instance() -> Result<(
    vulkanalia::Entry,
    VulkanInstanceDestroyedUnlessKept,
    vk::PFN_vkGetInstanceProcAddr,
)> {
    let loader = crate::open_the_first_vulkan_loader_library_that_opens().map_err(|not_found| {
        ConsumerRhiError::Gpu(format!(
            "Failed to load the Vulkan loader library; tried:\n{not_found}"
        ))
    })?;
    // SAFETY: the symbol is the loader's own `vkGetInstanceProcAddr`, and the
    // library stays open for as long as the entry built on the same loader.
    let get_instance_proc_addr = unsafe {
        let untyped =
            vulkanalia::loader::Loader::load(&loader, b"vkGetInstanceProcAddr").map_err(|e| {
                ConsumerRhiError::Gpu(format!("the loader has no vkGetInstanceProcAddr: {e}"))
            })?;
        std::mem::transmute::<extern "system" fn(), vk::PFN_vkGetInstanceProcAddr>(untyped)
    };
    let entry = unsafe { vulkanalia::Entry::new(loader) }
        .map_err(|e| ConsumerRhiError::Gpu(format!("Failed to load Vulkan entry: {e}")))?;

    let app_info = vk::ApplicationInfo::builder()
        .application_name(b"StreamLibConsumer\0")
        .application_version(vk::make_version(0, 1, 0))
        .engine_name(b"StreamLib\0")
        .engine_version(vk::make_version(0, 1, 0))
        .api_version(crate::REQUESTED_VULKAN_INSTANCE_API_VERSION)
        .build();

    // MoltenVK is a non-conformant portability implementation: the loader
    // enumerates it only for an instance that opts in.
    #[cfg(target_os = "macos")]
    let (instance_extensions, instance_create_flags) = (
        [c"VK_KHR_portability_enumeration".as_ptr()],
        vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR,
    );
    #[cfg(not(target_os = "macos"))]
    let (instance_extensions, instance_create_flags): ([*const c_char; 0], _) =
        ([], vk::InstanceCreateFlags::empty());
    let instance_info = vk::InstanceCreateInfo::builder()
        .application_info(&app_info)
        .enabled_extension_names(&instance_extensions)
        .flags(instance_create_flags)
        .build();

    let instance = unsafe { entry.create_instance(&instance_info, None) }
        .map_err(|e| ConsumerRhiError::Gpu(format!("Failed to create Vulkan instance: {e}")))?;
    Ok((
        entry,
        VulkanInstanceDestroyedUnlessKept(instance),
        get_instance_proc_addr,
    ))
}

/// Each physical device `instance` enumerates, in the loader's order, with
/// how selection by `deviceUUID` sees it.
pub(super) fn physical_devices_the_loader_enumerates_on(
    instance: &vulkanalia::Instance,
) -> Result<Vec<(vk::PhysicalDevice, VulkanDeviceTheLoaderEnumerates)>> {
    let physical_devices = unsafe { instance.enumerate_physical_devices() }
        .map_err(|e| ConsumerRhiError::Gpu(format!("Failed to enumerate physical devices: {e}")))?;
    Ok(physical_devices
        .into_iter()
        .map(|physical_device| {
            let mut id_properties = vk::PhysicalDeviceIDProperties::default();
            let mut properties2 = vk::PhysicalDeviceProperties2::builder()
                .push_next(&mut id_properties)
                .build();
            unsafe { instance.get_physical_device_properties2(physical_device, &mut properties2) };
            let device_properties = properties2.properties;
            (
                physical_device,
                VulkanDeviceTheLoaderEnumerates {
                    device_name: device_properties
                        .device_name
                        .as_cstr()
                        .to_string_lossy()
                        .into_owned(),
                    device_uuid: id_properties.device_uuid.into(),
                    device_type: device_properties.device_type,
                },
            )
        })
        .collect())
}

/// The index in `enumerated_devices` of the one carrying
/// `vulkan_device_uuid_the_runtime_named`, or the refusal naming that UUID and
/// every device enumerated — never a fallback to another device, since an
/// OPAQUE_FD import on any other reads the wrong memory.
pub(super) fn index_of_the_device_the_runtime_named(
    enumerated_devices: &[VulkanDeviceTheLoaderEnumerates],
    vulkan_device_uuid_the_runtime_named: [u8; 16],
) -> Result<usize> {
    if let Some(index) = enumerated_devices.iter().position(|enumerated_device| {
        enumerated_device.device_uuid == vulkan_device_uuid_the_runtime_named
    }) {
        return Ok(index);
    }
    let devices_seen = if enumerated_devices.is_empty() {
        "none".to_owned()
    } else {
        enumerated_devices
            .iter()
            .map(|enumerated_device| {
                format!(
                    "{} ({:?}, {})",
                    enumerated_device.device_name,
                    enumerated_device.device_type,
                    crate::lowercase_hex_of_vulkan_device_uuid(enumerated_device.device_uuid)
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    Err(ConsumerRhiError::Gpu(format!(
        "the runtime built on Vulkan device {}, and no device this process enumerates carries \
         that UUID — it saw: {devices_seen}. A node interpreter inherits the runtime's \
         loader environment; a driver variable set for this process alone can hide the \
         runtime's device",
        crate::lowercase_hex_of_vulkan_device_uuid(vulkan_device_uuid_the_runtime_named)
    )))
}
