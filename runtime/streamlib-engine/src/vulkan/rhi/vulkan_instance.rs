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
    pub(crate) window_system_surface_extensions: WindowSystemSurfaceExtensionsEnabledOnInstance,
}

/// A platform extension that creates a `VkSurfaceKHR` for one window system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowSystemSurfaceExtension {
    KhrWayland,
    KhrXcb,
    KhrXlib,
    ExtMetal,
}

impl WindowSystemSurfaceExtension {
    /// The window systems a window on this platform can come from.
    const OFFERED_ON_THIS_PLATFORM: &[Self] = if cfg!(target_os = "linux") {
        &[Self::KhrWayland, Self::KhrXcb, Self::KhrXlib]
    } else if cfg!(any(target_os = "macos", target_os = "ios")) {
        &[Self::ExtMetal]
    } else {
        &[]
    };

    /// The extension's registry name.
    pub(crate) fn extension_name(self) -> &'static CStr {
        match self {
            Self::KhrWayland => c"VK_KHR_wayland_surface",
            Self::KhrXcb => c"VK_KHR_xcb_surface",
            Self::KhrXlib => c"VK_KHR_xlib_surface",
            Self::ExtMetal => c"VK_EXT_metal_surface",
        }
    }
}

const KHR_SURFACE_EXTENSION_NAME: &CStr = c"VK_KHR_surface";

/// The window-system surface extensions an instance enabled: which windows a
/// surface can be created for, so no surface call reaches a command the
/// loader never loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WindowSystemSurfaceExtensionsEnabledOnInstance {
    khr_surface_enabled: bool,
    platform_surface_extensions_enabled: Vec<WindowSystemSurfaceExtension>,
}

impl WindowSystemSurfaceExtensionsEnabledOnInstance {
    /// Enable `VK_KHR_surface` and every surface extension of this platform's
    /// window systems the loader offers.
    fn select_from_available(available_extension_names: &[&CStr]) -> Self {
        Self {
            khr_surface_enabled: available_extension_names.contains(&KHR_SURFACE_EXTENSION_NAME),
            platform_surface_extensions_enabled:
                WindowSystemSurfaceExtension::OFFERED_ON_THIS_PLATFORM
                    .iter()
                    .copied()
                    .filter(|extension| {
                        available_extension_names.contains(&extension.extension_name())
                    })
                    .collect(),
        }
    }

    /// The names to enable on the instance.
    fn instance_extension_names(&self) -> impl Iterator<Item = &'static CStr> + '_ {
        self.khr_surface_enabled
            .then_some(KHR_SURFACE_EXTENSION_NAME)
            .into_iter()
            .chain(
                self.platform_surface_extensions_enabled
                    .iter()
                    .map(|extension| extension.extension_name()),
            )
    }

    /// Whether a window from at least one of this platform's window systems
    /// can get a surface; a headless surface does not count.
    pub(crate) fn can_create_a_surface_for_some_window_system(&self) -> bool {
        self.khr_surface_enabled && !self.platform_surface_extensions_enabled.is_empty()
    }

    /// Refuse, naming the missing extension, a surface through `extension`
    /// this instance did not enable.
    pub(crate) fn refuse_a_surface_this_instance_cannot_create(
        &self,
        extension: WindowSystemSurfaceExtension,
    ) -> Result<()> {
        let missing_extension_name = if !self.khr_surface_enabled {
            KHR_SURFACE_EXTENSION_NAME
        } else if !self
            .platform_surface_extensions_enabled
            .contains(&extension)
        {
            extension.extension_name()
        } else {
            return Ok(());
        };
        Err(Error::DisplaySurfaceUnavailable(format!(
            "the Vulkan instance has no {}, so no surface can be created for this window: the \
             driver carries no support for its window system",
            missing_extension_name.to_string_lossy()
        )))
    }
}

impl HostVulkanInstance {
    /// Open the loader and create the instance at the engine's API version
    /// with every window-system and validation extension the host offers.
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

        let window_system_surface_extensions =
            WindowSystemSurfaceExtensionsEnabledOnInstance::select_from_available(
                &available_ext_names,
            );
        instance_extensions.extend(
            window_system_surface_extensions
                .instance_extension_names()
                .map(CStr::as_ptr),
        );
        let window_system_surface_extension_names: Vec<_> = window_system_surface_extensions
            .instance_extension_names()
            .map(CStr::to_string_lossy)
            .collect();
        if window_system_surface_extensions.can_create_a_surface_for_some_window_system() {
            tracing::info!(
                extensions = ?window_system_surface_extension_names,
                "window-system surface extensions enabled"
            );
        } else {
            tracing::info!(
                extensions = ?window_system_surface_extension_names,
                "the Vulkan instance offers no window-system surface extension: no window can \
                 be opened on it"
            );
        }

        #[cfg(target_os = "linux")]
        {
            // VK_EXT_headless_surface — enables creating a Vulkan surface without
            // a window. Used by unit tests to exercise the swapchain code path
            // without requiring a display server.
            let headless_ext = c"VK_EXT_headless_surface";
            if available_ext_names.contains(&headless_ext) {
                instance_extensions.push(headless_ext.as_ptr());
                tracing::info!("VK_EXT_headless_surface available");
            }
        }

        // VK_EXT_swapchain_colorspace — exposes the wide-gamut + HDR
        // VkColorSpaceKHR enumerants (HDR10_ST2084_EXT,
        // EXTENDED_SRGB_LINEAR_EXT, DISPLAY_P3_*, BT709_*, etc.)
        // through `vkGetPhysicalDeviceSurfaceFormatsKHR`. Without it,
        // every WSI surface advertises only `SRGB_NONLINEAR_KHR` and
        // the swapchain colorspace priority walk has nothing to walk.
        let swapchain_colorspace_ext = c"VK_EXT_swapchain_colorspace";
        if available_ext_names.contains(&swapchain_colorspace_ext) {
            instance_extensions.push(swapchain_colorspace_ext.as_ptr());
            tracing::info!("VK_EXT_swapchain_colorspace enabled");
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
            window_system_surface_extensions,
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

    fn every_window_system_surface_extension_of_this_platform() -> Vec<&'static CStr> {
        WindowSystemSurfaceExtension::OFFERED_ON_THIS_PLATFORM
            .iter()
            .map(|extension| extension.extension_name())
            .collect()
    }

    /// The bundled lavapipe is built headless: it offers the platform-agnostic
    /// surface and the headless one, and no window system's (#2775).
    #[test]
    fn a_driver_offering_only_the_headless_surface_can_create_no_window_surface() {
        let enabled = WindowSystemSurfaceExtensionsEnabledOnInstance::select_from_available(&[
            c"VK_KHR_surface",
            c"VK_EXT_headless_surface",
        ]);

        assert!(
            !enabled.can_create_a_surface_for_some_window_system(),
            "{enabled:?}"
        );
        for extension in WindowSystemSurfaceExtension::OFFERED_ON_THIS_PLATFORM {
            let refusal = enabled
                .refuse_a_surface_this_instance_cannot_create(*extension)
                .expect_err("a surface through an extension the instance lacks must be refused");
            assert!(
                refusal
                    .to_string()
                    .contains(&*extension.extension_name().to_string_lossy()),
                "the refusal must name the missing extension: {refusal}"
            );
        }
    }

    #[test]
    fn a_driver_offering_this_platforms_window_systems_can_create_a_surface_for_each() {
        let mut available = every_window_system_surface_extension_of_this_platform();
        available.push(c"VK_KHR_surface");
        let enabled =
            WindowSystemSurfaceExtensionsEnabledOnInstance::select_from_available(&available);

        assert_eq!(
            enabled.can_create_a_surface_for_some_window_system(),
            !WindowSystemSurfaceExtension::OFFERED_ON_THIS_PLATFORM.is_empty()
        );
        for extension in WindowSystemSurfaceExtension::OFFERED_ON_THIS_PLATFORM {
            enabled
                .refuse_a_surface_this_instance_cannot_create(*extension)
                .expect("an enabled window-system extension must not be refused");
        }
        let mut enabled_names: Vec<&CStr> = enabled.instance_extension_names().collect();
        enabled_names.sort();
        available.sort();
        assert_eq!(enabled_names, available);
    }

    #[test]
    fn a_window_system_extension_without_the_platform_agnostic_surface_is_refused_naming_it() {
        let enabled = WindowSystemSurfaceExtensionsEnabledOnInstance::select_from_available(
            &every_window_system_surface_extension_of_this_platform(),
        );

        assert!(!enabled.can_create_a_surface_for_some_window_system());
        for extension in WindowSystemSurfaceExtension::OFFERED_ON_THIS_PLATFORM {
            let refusal = enabled
                .refuse_a_surface_this_instance_cannot_create(*extension)
                .expect_err("no surface can be created without VK_KHR_surface");
            assert!(refusal.to_string().contains("VK_KHR_surface"), "{refusal}");
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
