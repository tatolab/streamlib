// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Khronos validation-layer wiring: which layer and layer features the
//! instance asks for, and a debug-utils messenger that forwards every
//! finding into `tracing` and counts it.
//!
//! Without a messenger the layer prints through C stdio, which bypasses the
//! test harness's output capture; registering one silences that stdout path
//! entirely, so findings travel through `tracing` and the counter and
//! nowhere else. See `docs/testing-hardware.md` for the env vars and the
//! whole-sweep gate.

use std::ffi::{CStr, c_char, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vulkanalia::vk::{self, EntryV1_0, ExtDebugUtilsExtensionInstanceCommands, HasBuilder};

/// Instance layer name of the Khronos validation layer.
const KHRONOS_VALIDATION_LAYER_NAME: &CStr = c"VK_LAYER_KHRONOS_validation";

/// Instance extension carrying the debug-utils messenger entry points.
/// Loader-provided — present in the unfiltered instance-extension
/// enumeration whether or not any layer is enabled.
const DEBUG_UTILS_EXTENSION_NAME: &CStr = c"VK_EXT_debug_utils";

/// Instance extension carrying `VkValidationFeaturesEXT`. Provided by the
/// validation layer itself, so it is absent from the loader's unfiltered
/// enumeration and must be looked up under the layer's own name.
const VALIDATION_FEATURES_EXTENSION_NAME: &CStr = c"VK_EXT_validation_features";

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
pub(crate) struct VulkanValidationConfiguration {
    /// Load `VK_LAYER_KHRONOS_validation` and install a counting messenger.
    pub(crate) enable_validation_layer: bool,
    /// Add `VK_VALIDATION_FEATURE_ENABLE_SYNCHRONIZATION_VALIDATION_EXT`.
    pub(crate) enable_synchronization_validation: bool,
    /// Abort the process on the first validation error rather than only
    /// counting and logging it.
    pub(crate) abort_process_on_validation_error: bool,
}

impl VulkanValidationConfiguration {
    /// Read the three `STREAMLIB_VULKAN_*` validation env vars.
    pub(crate) fn from_environment() -> Self {
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

/// Validation findings a device's messenger has seen, by severity.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct VulkanValidationMessageCounts {
    /// Findings at `ERROR` severity — the gate's subject.
    pub error_count: usize,
    /// Findings at `WARNING` severity.
    pub warning_count: usize,
}

/// Everything the messenger callback reaches through `pUserData`: the two
/// counters it bumps and the policy it consults.
///
/// Held behind an `Arc` rather than a `Box` because the layer keeps a raw
/// pointer into the allocation for longer than any borrow can be expressed:
/// a `Box` is a unique owner, so moving it would retag and invalidate a
/// pointer derived from it, while an `Arc`'s payload is shared by
/// construction and only ever handed out as `&self`.
#[derive(Debug)]
pub(crate) struct VulkanValidationMessengerCallbackState {
    error_count: AtomicUsize,
    warning_count: AtomicUsize,
    abort_process_on_validation_error: bool,
}

impl VulkanValidationMessengerCallbackState {
    fn new(abort_process_on_validation_error: bool) -> Self {
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

/// The layers, extensions and layer features an instance should ask for,
/// resolved against what this host actually has installed. Empty on every
/// field when validation is off.
pub(crate) struct VulkanValidationInstanceSetup {
    pub(crate) enabled_layer_names: Vec<*const c_char>,
    pub(crate) enabled_extension_names: Vec<*const c_char>,
    pub(crate) enabled_validation_features: Vec<vk::ValidationFeatureEnableEXT>,
    messenger_callback_state: Option<Arc<VulkanValidationMessengerCallbackState>>,
}

impl VulkanValidationInstanceSetup {
    /// Decide what to ask `vkCreateInstance` for. Every absence is a
    /// warning and never a failure, so a host without the layer — CI —
    /// creates exactly the instance it did before.
    pub(crate) fn resolve(
        entry: &vulkanalia::Entry,
        configuration: VulkanValidationConfiguration,
        available_instance_extension_names: &[&CStr],
    ) -> Self {
        let mut setup = Self {
            enabled_layer_names: Vec::new(),
            enabled_extension_names: Vec::new(),
            enabled_validation_features: Vec::new(),
            messenger_callback_state: None,
        };
        if !configuration.enable_validation_layer {
            return setup;
        }

        let layer_properties =
            unsafe { entry.enumerate_instance_layer_properties() }.unwrap_or_default();
        let layer_is_installed = layer_properties
            .iter()
            .any(|properties| properties.layer_name.as_cstr() == KHRONOS_VALIDATION_LAYER_NAME);
        if !layer_is_installed {
            tracing::warn!(
                "{VALIDATION_ENV_VAR} set but VK_LAYER_KHRONOS_validation is not installed"
            );
            return setup;
        }
        setup
            .enabled_layer_names
            .push(KHRONOS_VALIDATION_LAYER_NAME.as_ptr());
        tracing::info!("VK_LAYER_KHRONOS_validation enabled ({VALIDATION_ENV_VAR})");

        if available_instance_extension_names.contains(&DEBUG_UTILS_EXTENSION_NAME) {
            setup
                .enabled_extension_names
                .push(DEBUG_UTILS_EXTENSION_NAME.as_ptr());
            setup.messenger_callback_state =
                Some(Arc::new(VulkanValidationMessengerCallbackState::new(
                    configuration.abort_process_on_validation_error,
                )));
        } else {
            tracing::warn!(
                "VK_EXT_debug_utils unavailable — validation findings will not be counted"
            );
        }

        if configuration.enable_synchronization_validation {
            setup.enable_synchronization_validation(entry);
        }
        setup
    }

    fn enable_synchronization_validation(&mut self, entry: &vulkanalia::Entry) {
        // VK_EXT_validation_features is advertised by the layer itself, so
        // it is absent from the loader's unfiltered instance-extension
        // enumeration and has to be looked up under the layer's name.
        let layer_extension_properties = unsafe {
            entry.enumerate_instance_extension_properties(Some(KHRONOS_VALIDATION_LAYER_NAME))
        }
        .unwrap_or_default();
        let advertises_validation_features = layer_extension_properties.iter().any(|properties| {
            properties.extension_name.as_cstr() == VALIDATION_FEATURES_EXTENSION_NAME
        });
        if !advertises_validation_features {
            tracing::warn!(
                "{SYNC_VALIDATION_ENV_VAR} set but the installed \
                 VK_LAYER_KHRONOS_validation does not advertise VK_EXT_validation_features"
            );
            return;
        }
        self.enabled_extension_names
            .push(VALIDATION_FEATURES_EXTENSION_NAME.as_ptr());
        self.enabled_validation_features
            .push(vk::ValidationFeatureEnableEXT::SYNCHRONIZATION_VALIDATION);
        tracing::info!("Vulkan synchronization validation enabled ({SYNC_VALIDATION_ENV_VAR})");
    }

    /// Create-info to chain into `VkInstanceCreateInfo::pNext`, covering the
    /// two calls the persistent messenger cannot: `vkCreateInstance` and
    /// `vkDestroyInstance`, where the object-lifetime "was not destroyed"
    /// reports land.
    ///
    /// The returned struct holds a raw pointer into the callback state with
    /// no lifetime tying the two. [`Self::into_installed_messenger`] moves
    /// that state into the messenger, which the device then holds for as
    /// long as the instance lives.
    pub(crate) fn instance_creation_messenger_info(
        &self,
    ) -> Option<vk::DebugUtilsMessengerCreateInfoEXT> {
        self.messenger_callback_state
            .as_ref()
            .map(messenger_create_info)
    }

    /// Register the messenger on the instance this setup was resolved for.
    pub(crate) fn into_installed_messenger(
        self,
        instance: &vulkanalia::Instance,
    ) -> Option<VulkanValidationMessenger> {
        let callback_state = self.messenger_callback_state?;
        let create_info = messenger_create_info(&callback_state);
        let messenger =
            match unsafe { instance.create_debug_utils_messenger_ext(&create_info, None) } {
                Ok(messenger) => Some(messenger),
                Err(e) => {
                    tracing::warn!(
                        "VK_LAYER_KHRONOS_validation loaded but vkCreateDebugUtilsMessengerEXT \
                     failed ({e}) — findings will not be counted"
                    );
                    None
                }
            };
        // Returned even when the handle failed: the pNext-chained messenger
        // recorded the same `pUserData` pointer at vkCreateInstance and is
        // re-invoked at vkDestroyInstance, so the callback state has to
        // outlive the instance either way.
        Some(VulkanValidationMessenger {
            messenger,
            callback_state,
        })
    }
}

fn messenger_create_info(
    callback_state: &Arc<VulkanValidationMessengerCallbackState>,
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
        .user_callback(Some(count_log_and_maybe_abort_on_validation_message))
        .build();
    create_info.user_data = Arc::as_ptr(callback_state) as *mut c_void;
    create_info
}

/// A `VkDebugUtilsMessengerEXT` plus the state it writes into.
pub(crate) struct VulkanValidationMessenger {
    messenger: Option<vk::DebugUtilsMessengerEXT>,
    callback_state: Arc<VulkanValidationMessengerCallbackState>,
}

impl VulkanValidationMessenger {
    /// Findings seen so far, or `None` when no messenger handle is live —
    /// findings are then possible but uncounted, which must never read as
    /// zero.
    pub(crate) fn counts(&self) -> Option<VulkanValidationMessageCounts> {
        self.messenger.map(|_| self.callback_state.counts())
    }

    /// Destroy the messenger handle, keeping the callback state alive so
    /// the `pNext`-chained messenger the loader still holds across
    /// `vkDestroyInstance` has valid `pUserData`.
    ///
    /// Not a `Drop` impl: the handle must die before the instance while the
    /// state must outlive it, an ordering no `Drop` can express. A
    /// `HostVulkanDevice::new` that fails after this messenger is installed
    /// leaks the handle — as it already leaks the instance itself, which
    /// `vulkanalia::Instance` never destroys on drop.
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
unsafe extern "system" fn count_log_and_maybe_abort_on_validation_message(
    message_severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    message_types: vk::DebugUtilsMessageTypeFlagsEXT,
    callback_data: *const vk::DebugUtilsMessengerCallbackDataEXT,
    user_data: *mut c_void,
) -> vk::Bool32 {
    let Some(callback_state) =
        (unsafe { (user_data as *const VulkanValidationMessengerCallbackState).as_ref() })
    else {
        return vk::FALSE;
    };
    let Some(callback_message_data) = (unsafe { callback_data.as_ref() }) else {
        return vk::FALSE;
    };

    let vuid = unsafe { owned_string_from_layer_cstr(callback_message_data.message_id_name) };
    let message = unsafe { owned_string_from_layer_cstr(callback_message_data.message) };

    if message_severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        callback_state.error_count.fetch_add(1, Ordering::Relaxed);
        tracing::error!(vuid = %vuid, message_type = ?message_types, "Vulkan validation: {message}");
        if callback_state.abort_process_on_validation_error {
            // Panic rather than `process::abort` so the reason reaches
            // the console through the panic hook, surviving the test
            // harness's output capture: a run with no `tracing`
            // subscriber — every `cargo test` binary — would otherwise
            // take a bare SIGABRT with nothing said. Unwinding out of an
            // `extern "system"` fn is a defined abort, not UB.
            panic!("{ABORT_ON_ERROR_ENV_VAR} is set — Vulkan validation error {vuid}: {message}");
        }
    } else {
        callback_state.warning_count.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(vuid = %vuid, message_type = ?message_types, "Vulkan validation: {message}");
    }

    vk::FALSE
}

/// Copy a layer-owned NUL-terminated string out of the callback data.
/// Owned rather than borrowed because a `&str` built from a raw pointer
/// carries an unbounded lifetime the compiler cannot check.
///
/// # Safety
/// `ptr` must be null or point at a valid NUL-terminated string.
unsafe fn owned_string_from_layer_cstr(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
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
    fn callback_state_starts_at_zero_and_counts_each_severity_apart() {
        let callback_state = VulkanValidationMessengerCallbackState::new(false);
        assert_eq!(
            callback_state.counts(),
            VulkanValidationMessageCounts::default()
        );
        callback_state.error_count.fetch_add(3, Ordering::Relaxed);
        callback_state.warning_count.fetch_add(2, Ordering::Relaxed);
        assert_eq!(
            callback_state.counts(),
            VulkanValidationMessageCounts {
                error_count: 3,
                warning_count: 2,
            }
        );
    }

    #[test]
    fn a_messenger_whose_handle_never_landed_reports_not_measured_rather_than_zero() {
        let messenger = VulkanValidationMessenger {
            messenger: None,
            callback_state: Arc::new(VulkanValidationMessengerCallbackState::new(false)),
        };
        assert_eq!(messenger.counts(), None);
        messenger
            .callback_state
            .error_count
            .fetch_add(1, Ordering::Relaxed);
        assert_eq!(
            messenger.counts(),
            None,
            "an uncounted finding must never read as zero findings"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod hardware_tests {
    use std::sync::Arc;

    use vulkanalia::prelude::v1_4::*;
    use vulkanalia::vk;

    use super::{VulkanValidationConfiguration, VulkanValidationMessageCounts};
    use crate::core::rhi::{
        Texture, TextureDescriptor, TextureFormat, TextureUsages, VulkanLayout,
    };
    use crate::host_rhi::HostTextureExt;
    use crate::vulkan::rhi::{
        HostVulkanBuffer, HostVulkanDevice, HostVulkanTexture, ImageCopyRegion, RhiCommandRecorder,
        VulkanAccess, VulkanStage,
    };

    /// A device whose validation findings can actually be counted, plus the
    /// count at the moment it was handed over. `None` when this run cannot
    /// measure — no GPU, or no messenger because validation is off.
    fn device_counting_validation_messages()
    -> Option<(Arc<HostVulkanDevice>, VulkanValidationMessageCounts)> {
        let device = match HostVulkanDevice::new() {
            Ok(device) => device,
            Err(e) => {
                println!("Skipping — Vulkan not available: {e}");
                return None;
            }
        };
        match device.validation_layer_message_counts() {
            Some(counts) => Some((device, counts)),
            None => {
                println!(
                    "Skipping — no validation messenger installed. Re-run with \
                     STREAMLIB_VULKAN_VALIDATION=1 and VK_LAYER_KHRONOS_validation present."
                );
                None
            }
        }
    }

    /// Without this, the zero-findings gate the other test asserts could
    /// hold at zero because the layer is silent rather than because the
    /// engine is clean.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_deliberately_invalid_vulkan_call_moves_the_validation_error_count() {
        // Abort-on-error is the whole-sweep gate; this test is the one
        // place that raises a finding on purpose, so under that mode it
        // would kill the sweep it is meant to keep honest.
        if VulkanValidationConfiguration::from_environment().abort_process_on_validation_error {
            println!(
                "Skipping — STREAMLIB_VULKAN_VALIDATION_ABORT_ON_ERROR is set, and this test \
                 provokes a validation error on purpose."
            );
            return;
        }
        let Some((device, before)) = device_counting_validation_messages() else {
            return;
        };

        let raw_device = device.device();
        let command_pool = unsafe {
            raw_device.create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(device.queue_family_index())
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                    .build(),
                None,
            )
        }
        .expect("command pool");
        let command_buffer = unsafe {
            raw_device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1)
                    .build(),
            )
        }
        .expect("command buffer")[0];

        // VUID-vkEndCommandBuffer-commandBuffer-00059: the buffer is in the
        // initial state, never begun. The layer reports it and skips the
        // down-call, so nothing reaches the driver.
        let _ = unsafe { raw_device.end_command_buffer(command_buffer) };

        let after = device
            .validation_layer_message_counts()
            .expect("messenger still installed");
        unsafe { raw_device.destroy_command_pool(command_pool, None) };

        assert!(
            after.error_count > before.error_count,
            "the validation layer reported nothing for a known-bad vkEndCommandBuffer, so a \
             zero-findings assertion would pass vacuously (before {before:?}, after {after:?})"
        );
    }

    /// The gate itself: a real upload → image → readback round trip through
    /// the engine's own recorder raises nothing.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_clean_gpu_round_trip_leaves_the_validation_message_counts_unmoved() {
        let Some((device, before)) = device_counting_validation_messages() else {
            return;
        };

        const WIDTH: u32 = 32;
        const HEIGHT: u32 = 32;
        let byte_size = u64::from(WIDTH) * u64::from(HEIGHT) * 4;

        let upload_buffer = HostVulkanBuffer::new(&device, byte_size).expect("upload buffer");
        let readback_buffer = HostVulkanBuffer::new(&device, byte_size).expect("readback buffer");
        let uploaded_bytes: Vec<u8> = (0..byte_size).map(|i| (i % 251) as u8).collect();
        unsafe {
            std::ptr::copy_nonoverlapping(
                uploaded_bytes.as_ptr(),
                upload_buffer.mapped_ptr(),
                uploaded_bytes.len(),
            );
        }

        let texture = <Texture as HostTextureExt>::from_vulkan(
            HostVulkanTexture::new(
                &device,
                &TextureDescriptor {
                    width: WIDTH,
                    height: HEIGHT,
                    format: TextureFormat::Bgra8Unorm,
                    usage: TextureUsages::COPY_SRC | TextureUsages::COPY_DST,
                    label: Some("validation-clean-round-trip"),
                },
            )
            .expect("texture"),
        );

        let mut recorder = RhiCommandRecorder::new(&device, "validation-clean-round-trip")
            .expect("command recorder");
        recorder.begin().expect("begin");
        recorder
            .record_image_barrier(
                &texture,
                VulkanLayout::UNDEFINED,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                VulkanStage::TOP_OF_PIPE,
                VulkanStage::COPY,
                VulkanAccess::NONE,
                VulkanAccess::TRANSFER_WRITE,
            )
            .expect("barrier into TRANSFER_DST_OPTIMAL");
        recorder
            .record_copy_buffer_to_image(
                &upload_buffer,
                &texture,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                ImageCopyRegion::tightly_packed(WIDTH, HEIGHT),
            )
            .expect("upload copy");
        recorder
            .record_image_barrier(
                &texture,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                VulkanLayout::TRANSFER_SRC_OPTIMAL,
                VulkanStage::COPY,
                VulkanStage::COPY,
                VulkanAccess::TRANSFER_WRITE,
                VulkanAccess::TRANSFER_READ,
            )
            .expect("barrier into TRANSFER_SRC_OPTIMAL");
        recorder
            .record_copy_image_to_buffer(
                &texture,
                VulkanLayout::TRANSFER_SRC_OPTIMAL,
                &readback_buffer,
                ImageCopyRegion::tightly_packed(WIDTH, HEIGHT),
            )
            .expect("readback copy");
        recorder
            .record_buffer_barrier(
                &readback_buffer,
                VulkanStage::COPY,
                VulkanStage::HOST,
                VulkanAccess::TRANSFER_WRITE,
                VulkanAccess::HOST_READ,
            )
            .expect("barrier for the host read");
        recorder.submit_and_wait().expect("submit and wait");

        let mut landed_bytes = vec![0u8; uploaded_bytes.len()];
        unsafe {
            std::ptr::copy_nonoverlapping(
                readback_buffer.mapped_ptr(),
                landed_bytes.as_mut_ptr(),
                landed_bytes.len(),
            );
        }
        assert_eq!(
            landed_bytes, uploaded_bytes,
            "the round trip did not move the pixels, so a zero-findings result proves nothing"
        );

        let after = device
            .validation_layer_message_counts()
            .expect("messenger still installed");
        assert_eq!(
            after, before,
            "a clean upload → image → readback round trip raised validation findings"
        );
    }
}
