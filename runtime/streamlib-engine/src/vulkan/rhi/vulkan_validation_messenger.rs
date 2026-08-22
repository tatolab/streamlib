// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Khronos validation-layer wiring: which layer and layer features the
//! instance asks for, and a debug-utils messenger that forwards every
//! finding into `tracing` and counts it.
//!
//! Without a messenger the layer prints through C stdio, which bypasses
//! both the test harness's output capture and the engine's own logging —
//! findings scroll past and nothing notices. The counter is what lets a
//! rig test hold a GPU path at zero findings.

use std::ffi::{CStr, c_char, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vulkanalia::vk::{self, ExtDebugUtilsExtensionInstanceCommands, HasBuilder};

/// Instance layer name of the Khronos validation layer.
pub(crate) const KHRONOS_VALIDATION_LAYER_NAME: &CStr = c"VK_LAYER_KHRONOS_validation";

/// Instance extension carrying the debug-utils messenger entry points.
/// Loader-provided — present in the unfiltered instance-extension
/// enumeration whether or not any layer is enabled.
pub(crate) const DEBUG_UTILS_EXTENSION_NAME: &CStr = c"VK_EXT_debug_utils";

/// Instance extension carrying `VkValidationFeaturesEXT`. Provided by the
/// validation layer itself, so it is absent from the loader's unfiltered
/// enumeration and must be looked up under the layer's own name.
pub(crate) const VALIDATION_FEATURES_EXTENSION_NAME: &CStr = c"VK_EXT_validation_features";

/// Env var that loads the Khronos validation layer.
const VALIDATION_ENV_VAR: &str = "STREAMLIB_VULKAN_VALIDATION";

/// Env var that adds synchronization validation.
const SYNC_VALIDATION_ENV_VAR: &str = "STREAMLIB_VULKAN_SYNC_VALIDATION";

/// Env var that turns the first validation error into a process abort.
const ABORT_ON_ERROR_ENV_VAR: &str = "STREAMLIB_VULKAN_VALIDATION_ABORT_ON_ERROR";

/// What the environment asked the Khronos validation layer to do.
///
/// Any of the three env vars loads the layer: sync validation is a feature
/// of the layer and abort-on-error is a behaviour of its messenger, so
/// neither can be meant without it.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct VulkanValidationConfiguration {
    /// Load `VK_LAYER_KHRONOS_validation` and install a counting messenger.
    pub enable_validation_layer: bool,
    /// Add `VK_VALIDATION_FEATURE_ENABLE_SYNCHRONIZATION_VALIDATION_EXT`.
    pub enable_synchronization_validation: bool,
    /// Abort the process on the first validation error rather than only
    /// counting and logging it.
    pub abort_process_on_validation_error: bool,
}

impl VulkanValidationConfiguration {
    /// Read the three `STREAMLIB_VULKAN_*` validation env vars.
    pub fn from_environment() -> Self {
        Self::from_environment_variable_values(
            std::env::var(VALIDATION_ENV_VAR).ok().as_deref(),
            std::env::var(SYNC_VALIDATION_ENV_VAR).ok().as_deref(),
            std::env::var(ABORT_ON_ERROR_ENV_VAR).ok().as_deref(),
        )
    }

    fn from_environment_variable_values(
        validation: Option<&str>,
        synchronization_validation: Option<&str>,
        abort_on_error: Option<&str>,
    ) -> Self {
        let enable_synchronization_validation = is_truthy(synchronization_validation);
        let abort_process_on_validation_error = is_truthy(abort_on_error);
        Self {
            enable_validation_layer: is_truthy(validation)
                || enable_synchronization_validation
                || abort_process_on_validation_error,
            enable_synchronization_validation,
            abort_process_on_validation_error,
        }
    }
}

fn is_truthy(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "true" | "yes"))
}

/// Validation findings this device's messenger has seen, by severity.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct VulkanValidationMessageCounts {
    pub error_count: usize,
    pub warning_count: usize,
}

impl VulkanValidationMessageCounts {
    /// Total findings across both severities.
    pub fn total(self) -> usize {
        self.error_count + self.warning_count
    }
}

/// The messenger's `pUserData`. Lives behind an `Arc` held by
/// [`VulkanValidationMessenger`]; the layer stores the raw pointer, so the
/// `Arc` must outlive both the messenger and `vkDestroyInstance`.
#[derive(Debug, Default)]
pub(crate) struct VulkanValidationMessageTally {
    error_count: AtomicUsize,
    warning_count: AtomicUsize,
    abort_process_on_validation_error: bool,
}

impl VulkanValidationMessageTally {
    pub(crate) fn new(abort_process_on_validation_error: bool) -> Self {
        Self {
            error_count: AtomicUsize::new(0),
            warning_count: AtomicUsize::new(0),
            abort_process_on_validation_error,
        }
    }

    fn counts(&self) -> VulkanValidationMessageCounts {
        VulkanValidationMessageCounts {
            error_count: self.error_count.load(Ordering::Relaxed),
            warning_count: self.warning_count.load(Ordering::Relaxed),
        }
    }
}

/// A `VkDebugUtilsMessengerEXT` plus the tally it writes into.
pub(crate) struct VulkanValidationMessenger {
    messenger: Option<vk::DebugUtilsMessengerEXT>,
    tally: Arc<VulkanValidationMessageTally>,
}

impl VulkanValidationMessenger {
    /// Build the create-info shared by the instance-creation `pNext` chain
    /// and the persistent messenger.
    ///
    /// The returned struct holds a raw pointer into `tally`'s allocation
    /// with no lifetime tying the two: every use must be dominated by a
    /// live clone of that `Arc`.
    pub(crate) fn create_info(
        tally: &Arc<VulkanValidationMessageTally>,
    ) -> vk::DebugUtilsMessengerCreateInfoEXT {
        let mut create_info = vk::DebugUtilsMessengerCreateInfoEXT::builder()
            .message_severity(
                vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                    | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
            )
            .message_type(
                vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                    | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                    | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
            )
            .user_callback(Some(forward_validation_message_to_tracing))
            .build();
        create_info.user_data = Arc::as_ptr(tally) as *mut c_void;
        create_info
    }

    /// Register the messenger on `instance`. `None` when the layer did not
    /// supply `vkCreateDebugUtilsMessengerEXT` — findings then reach stdout
    /// only, as before, and no count is available.
    pub(crate) fn install(
        instance: &vulkanalia::Instance,
        tally: Arc<VulkanValidationMessageTally>,
    ) -> Option<Self> {
        let create_info = Self::create_info(&tally);
        match unsafe { instance.create_debug_utils_messenger_ext(&create_info, None) } {
            Ok(messenger) => Some(Self {
                messenger: Some(messenger),
                tally,
            }),
            Err(e) => {
                tracing::warn!(
                    "VK_LAYER_KHRONOS_validation loaded but vkCreateDebugUtilsMessengerEXT \
                     failed ({e}) — findings will not be counted"
                );
                None
            }
        }
    }

    pub(crate) fn counts(&self) -> VulkanValidationMessageCounts {
        self.tally.counts()
    }

    /// Destroy the messenger handle, keeping the tally alive so the
    /// `pNext`-chained messenger the loader still holds across
    /// `vkDestroyInstance` has valid `pUserData`.
    ///
    /// # Safety
    /// `instance` must be the instance the messenger was installed on, and
    /// must not yet have been destroyed.
    pub(crate) unsafe fn destroy_handle(&mut self, instance: &vulkanalia::Instance) {
        if let Some(messenger) = self.messenger.take() {
            unsafe { instance.destroy_debug_utils_messenger_ext(messenger, None) };
        }
    }
}

/// `PFN_vkDebugUtilsMessengerCallbackEXT`. Always returns `VK_FALSE`: per
/// the spec `VK_TRUE` is reserved for layer-development use and aborts the
/// call the message came from.
unsafe extern "system" fn forward_validation_message_to_tracing(
    message_severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    message_types: vk::DebugUtilsMessageTypeFlagsEXT,
    callback_data: *const vk::DebugUtilsMessengerCallbackDataEXT,
    user_data: *mut c_void,
) -> vk::Bool32 {
    let Some(tally) = (unsafe { (user_data as *const VulkanValidationMessageTally).as_ref() })
    else {
        return vk::FALSE;
    };
    let Some(data) = (unsafe { callback_data.as_ref() }) else {
        return vk::FALSE;
    };

    let vuid = unsafe { owned_string_from_layer_cstr(data.message_id_name) };
    let message = unsafe { owned_string_from_layer_cstr(data.message) };

    if message_severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        tally.error_count.fetch_add(1, Ordering::Relaxed);
        tracing::error!(vuid = %vuid, message_type = ?message_types, "Vulkan validation: {message}");
        if tally.abort_process_on_validation_error {
            tracing::error!(
                "{ABORT_ON_ERROR_ENV_VAR} is set — aborting on the validation error above"
            );
            std::process::abort();
        }
    } else {
        tally.warning_count.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(vuid = %vuid, message_type = ?message_types, "Vulkan validation: {message}");
    }

    vk::FALSE
}

/// Copy a layer-owned NUL-terminated string out of the callback data. The
/// layer's storage is valid only for the duration of the callback, so the
/// text is owned rather than borrowed.
///
/// # Safety
/// `ptr` must be null or point at a valid NUL-terminated string.
unsafe fn owned_string_from_layer_cstr(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_environment_asks_for_no_validation() {
        let configuration =
            VulkanValidationConfiguration::from_environment_variable_values(None, None, None);
        assert_eq!(configuration, VulkanValidationConfiguration::default());
    }

    #[test]
    fn an_explicit_zero_asks_for_no_validation() {
        let configuration = VulkanValidationConfiguration::from_environment_variable_values(
            Some("0"),
            Some("0"),
            Some("0"),
        );
        assert_eq!(configuration, VulkanValidationConfiguration::default());
    }

    #[test]
    fn the_validation_env_var_loads_the_layer_and_nothing_else() {
        for truthy in ["1", "true", "yes"] {
            let configuration = VulkanValidationConfiguration::from_environment_variable_values(
                Some(truthy),
                None,
                None,
            );
            assert_eq!(
                configuration,
                VulkanValidationConfiguration {
                    enable_validation_layer: true,
                    enable_synchronization_validation: false,
                    abort_process_on_validation_error: false,
                },
                "{truthy}"
            );
        }
    }

    #[test]
    fn sync_validation_on_its_own_loads_the_layer_it_is_a_feature_of() {
        let configuration =
            VulkanValidationConfiguration::from_environment_variable_values(None, Some("1"), None);
        assert_eq!(
            configuration,
            VulkanValidationConfiguration {
                enable_validation_layer: true,
                enable_synchronization_validation: true,
                abort_process_on_validation_error: false,
            }
        );
    }

    #[test]
    fn abort_on_error_on_its_own_loads_the_layer_it_is_a_behaviour_of() {
        let configuration =
            VulkanValidationConfiguration::from_environment_variable_values(None, None, Some("1"));
        assert_eq!(
            configuration,
            VulkanValidationConfiguration {
                enable_validation_layer: true,
                enable_synchronization_validation: false,
                abort_process_on_validation_error: true,
            }
        );
    }

    #[test]
    fn a_tally_starts_at_zero_and_totals_both_severities() {
        let tally = VulkanValidationMessageTally::new(false);
        assert_eq!(tally.counts(), VulkanValidationMessageCounts::default());
        tally.error_count.fetch_add(3, Ordering::Relaxed);
        tally.warning_count.fetch_add(2, Ordering::Relaxed);
        assert_eq!(
            tally.counts(),
            VulkanValidationMessageCounts {
                error_count: 3,
                warning_count: 2,
            }
        );
        assert_eq!(tally.counts().total(), 5);
    }
}
