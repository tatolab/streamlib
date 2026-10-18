// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Vulkan instance every candidate device is built on.

use std::ffi::{CStr, c_char};

use vulkanalia::loader::LibloadingLoader;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::{Error, Result};

use streamlib_consumer_rhi::REQUESTED_VULKAN_INSTANCE_API_VERSION;

use super::vulkan_validation_messenger::{
    VulkanValidationConfiguration, VulkanValidationInstanceSetup, VulkanValidationMessenger,
};

/// What to install when the loader opened but no ICD behind it is usable.
pub(crate) const NO_USABLE_VULKAN_DRIVER_GUIDANCE: &str =
    if cfg!(any(target_os = "macos", target_os = "ios")) {
        "No usable Vulkan driver (ICD) was found. MoltenVK is the Vulkan driver on Apple \
         hardware; check that its ICD manifest is on the loader's search path."
    } else {
        "No usable Vulkan driver (ICD) was found. Install your GPU vendor's Vulkan \
         driver — the proprietary NVIDIA driver, or `mesa-vulkan-drivers` for \
         AMD/Intel — then check `vulkaninfo` runs."
    };

/// What to install when no loader library opened at all.
const NO_VULKAN_LOADER_LIBRARY_GUIDANCE: &str = if cfg!(any(target_os = "macos", target_os = "ios"))
{
    "No Vulkan loader library could be opened. Apple hardware reaches the GPU through \
     the Vulkan loader and MoltenVK, and neither was found at any of these paths:"
} else {
    "No Vulkan loader library could be opened. Install your GPU vendor's Vulkan \
     driver and the loader (`libvulkan1` / `vulkan-loader`), then check `vulkaninfo` \
     runs. Tried:"
};

/// Open the first Vulkan loader library on the shared search list, or refuse
/// naming every candidate tried so the failure says where it looked.
fn load_the_first_vulkan_loader_library_that_opens() -> Result<LibloadingLoader> {
    streamlib_consumer_rhi::open_the_first_vulkan_loader_library_that_opens().map_err(|not_found| {
        Error::GpuError(format!("{NO_VULKAN_LOADER_LIBRARY_GUIDANCE}\n{not_found}"))
    })
}

/// The loaded entry points, the instance, and its validation messenger,
/// destroyed together when the last device built on them — or the device
/// selection that refused every candidate — lets go.
pub(crate) struct HostVulkanInstance {
    pub(crate) entry: vulkanalia::Entry,
    pub(crate) instance: vulkanalia::Instance,
    /// Must outlive `destroy_instance`, whose own findings reach the
    /// pNext-chained messenger through the same user-data pointer.
    pub(crate) validation_messenger: Option<VulkanValidationMessenger>,
}

impl HostVulkanInstance {
    /// Open the loader and create the instance at the engine's API version
    /// with every validation extension the host offers.
    pub(crate) fn new() -> Result<Self> {
        // 1. Load Vulkan entry points via libloading
        let loader = load_the_first_vulkan_loader_library_that_opens()?;
        let entry = unsafe { vulkanalia::Entry::new(loader) }
            .map_err(|e| Error::GpuError(format!("Failed to load Vulkan entry points: {e}")))?;

        // 2. Enumerate available instance extensions
        let available_extensions =
            unsafe { entry.enumerate_instance_extension_properties(None) }
                .map_err(|e| Error::GpuError(format!("Failed to enumerate extensions: {e}")))?;

        let available_ext_names: Vec<&CStr> = available_extensions
            .iter()
            .map(|ext| ext.extension_name.as_cstr())
            .collect();

        // 3. Build extension list
        let mut instance_extensions: Vec<*const c_char> = Vec::new();

        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            // MoltenVK is a non-conformant portability implementation: the loader
            // hides it from enumeration unless this is asked for, together with
            // ENUMERATE_PORTABILITY_KHR on the create flags below.
            let portability_enum = c"VK_KHR_portability_enumeration";
            if available_ext_names.contains(&portability_enum) {
                instance_extensions.push(portability_enum.as_ptr());
                tracing::info!("VK_KHR_portability_enumeration enabled");
            }
        }

        // 4. Create Vulkan instance at API version 1.4
        let app_info = vk::ApplicationInfo::builder()
            .application_name(b"StreamLib\0")
            .application_version(vk::make_version(0, 1, 0))
            .engine_name(b"StreamLib\0")
            .engine_version(vk::make_version(0, 1, 0))
            .api_version(REQUESTED_VULKAN_INSTANCE_API_VERSION)
            .build();

        #[allow(unused_mut)]
        let mut instance_create_flags = vk::InstanceCreateFlags::empty();

        // On macOS/iOS, enable portability enumeration flag
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            instance_create_flags |= vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR;
        }

        // Optional Khronos validation layer — opt-in so it doesn't bloat
        // the production deviceless path. Enabled via the
        // `STREAMLIB_VULKAN_VALIDATION=1` env var; a sibling alternative
        // to `VK_LOADER_LAYERS_ENABLE` for cases where the loader-side
        // hook is shadowed (notably some Bazel / Docker / cargo-test
        // configurations).
        let validation_setup = VulkanValidationInstanceSetup::resolve(
            &entry,
            VulkanValidationConfiguration::from_environment(),
            &available_ext_names,
        );
        instance_extensions.extend_from_slice(&validation_setup.enabled_extension_names);

        let mut instance_creation_messenger_info =
            validation_setup.instance_creation_messenger_info();
        let mut validation_features = vk::ValidationFeaturesEXT::builder()
            .enabled_validation_features(&validation_setup.enabled_validation_features)
            .build();

        let mut instance_info = vk::InstanceCreateInfo::builder()
            .application_info(&app_info)
            .enabled_extension_names(&instance_extensions)
            .enabled_layer_names(&validation_setup.enabled_layer_names)
            .flags(instance_create_flags);
        let mut layer_settings = vk::LayerSettingsCreateInfoEXT::builder()
            .settings(&validation_setup.enabled_layer_settings)
            .build();
        if !validation_setup.enabled_validation_features.is_empty() {
            instance_info = instance_info.push_next(&mut validation_features);
        }
        if !validation_setup.enabled_layer_settings.is_empty() {
            instance_info = instance_info.push_next(&mut layer_settings);
        }
        if let Some(messenger_info) = instance_creation_messenger_info.as_mut() {
            instance_info = instance_info.push_next(messenger_info);
        }
        // VK_EXT_metal_objects exports the device's MTLDevice only from an
        // instance created with this in its pNext chain
        // (VUID-VkExportMetalObjectsInfoEXT-pNext-06791); the registry makes
        // it extend VkInstanceCreateInfo, not VkDeviceCreateInfo.
        #[cfg(target_os = "macos")]
        let mut metal_device_export_declaration = vk::ExportMetalObjectCreateInfoEXT::builder()
            .export_object_type(vk::ExportMetalObjectTypeFlagsEXT::METAL_DEVICE)
            .build();
        #[cfg(target_os = "macos")]
        {
            instance_info = instance_info.push_next(&mut metal_device_export_declaration);
        }
        let instance_info = instance_info.build();

        let instance = unsafe { entry.create_instance(&instance_info, None) }.map_err(|e| {
            Error::GpuError(if e == vk::ErrorCode::INCOMPATIBLE_DRIVER {
                NO_USABLE_VULKAN_DRIVER_GUIDANCE.to_string()
            } else {
                format!("Failed to create Vulkan instance: {e}")
            })
        })?;

        let validation_messenger = validation_setup.into_installed_messenger(&instance);

        Ok(Self {
            entry,
            instance,
            validation_messenger,
        })
    }
}

impl Drop for HostVulkanInstance {
    fn drop(&mut self) {
        // SAFETY: every device built on this instance holds it and has been
        // destroyed before the last holder drops.
        unsafe {
            if let Some(messenger) = self.validation_messenger.as_mut() {
                messenger.destroy_handle(&self.instance);
            }
            self.instance.destroy_instance(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runtime unit's lend carries the loader and MoltenVK (#2362), so nothing
    /// a user can read may send them to a third-party package manager; a
    /// contributor who happens to have them from Homebrew or the LunarG SDK is
    /// served by the search list, which is not text anybody reads. Owner,
    /// 2026-09-20.
    #[test]
    fn the_guidance_a_user_reads_never_names_a_third_party_package_manager() {
        const THIRD_PARTY_PACKAGE_MANAGERS: &[&str] = &["brew", "homebrew", "port ", "macports"];

        for (label, guidance) in [
            ("no-loader", NO_VULKAN_LOADER_LIBRARY_GUIDANCE),
            ("no-driver", NO_USABLE_VULKAN_DRIVER_GUIDANCE),
        ] {
            let lowercased = guidance.to_lowercase();
            for package_manager in THIRD_PARTY_PACKAGE_MANAGERS {
                assert!(
                    !lowercased.contains(package_manager),
                    "the {label} guidance sends a user to `{package_manager}`; the runtime \
                     unit's lend carries the driver: {guidance}"
                );
            }
        }
    }

    /// The floor that makes the promoted 1.3 entry points resolve. Asserted on
    /// the request the engine makes, never on what a device reports back: the
    /// reported value is clamped to the request under MoltenVK, so a device
    /// probe would pass for the wrong reason on every driver.
    #[test]
    fn the_requested_instance_api_version_clears_the_promoted_entry_point_floor() {
        let requested = REQUESTED_VULKAN_INSTANCE_API_VERSION;
        let major = vk::version_major(requested);
        let minor = vk::version_minor(requested);

        assert!(
            major > 1 || (major == 1 && minor >= 3),
            "cmd_pipeline_barrier2, cmd_begin_rendering, queue_submit2 and wait_semaphores are \
             promoted into core 1.3 and resolve only because the instance asks for at least \
             that; the request is {major}.{minor}"
        );
    }
}
