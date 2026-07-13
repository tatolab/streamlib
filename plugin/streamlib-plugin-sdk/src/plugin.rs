// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Plugin ABI cdylib-side glue.
//!
//! Companion to `streamlib-plugin-abi`'s [`HostServices`] ABI contract.
//! This module owns the **cdylib arm** of the plugin-registration machinery:
//!
//! - [`HostCallbacks`] + [`HOST_CALLBACKS`] + [`host_callbacks`] — the
//!   per-plugin cache of the host's fn pointers, populated once by
//!   [`install_host_services`].
//! - [`install_host_services`] — the cdylib entry point the
//!   `export_plugin!` macro calls. Validates layout versions, caches the
//!   callbacks, installs the cdylib's tracing + iceoryx2 forwarders, and
//!   returns a [`RegisterHelper`].
//! - [`RegisterHelper`] — handed back to the macro; its `register::<P>()`
//!   monomorphizes a [`ProcessorVTable`] per processor type and routes
//!   through the host's `processor_register` callback.
//!
//! The host backings (`HOST_*_VTABLE` statics, the host callback impls,
//! the `runtime_facing` payload builder, `PROCESSOR_REGISTRY`) stay in the
//! engine — this crate carries only the cdylib code, which is why it can
//! compile without `streamlib-engine`.

use std::ffi::c_void;
use std::sync::OnceLock;

use streamlib_plugin_abi::{
    AudioClockVTable, GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION, GpuContextFullAccessVTable,
    GpuContextLimitedAccessVTable, HOST_SERVICES_LAYOUT_VERSION, HostHandle, HostInterest,
    HostLogLevel, HostServices, ProcessorVTable, RuntimeContextVTable, RuntimeOpsVTable,
    SURFACE_STORE_VTABLE_LAYOUT_VERSION, SurfaceStoreVTable,
};

pub mod forwarding_subscriber;
pub mod iceoryx2_log_forwarder;
pub mod processor_vtable;

// =============================================================================
// HostCallbacks — per-plugin cache of the host's fn pointers
// =============================================================================

/// Cached copy of the host's callback table, stored in [`HOST_CALLBACKS`]
/// by [`install_host_services`] so the cdylib's PUBSUB / schema-registry /
/// tracing / iceoryx2-log forwarders can reach the host without
/// indirecting through [`HostServices`] on every call.
//
// Many fields are read only by cdylib code paths that this minimal SDK
// surface hasn't relocated yet (the GPU FullAccess views, pubsub/schema
// shims). They're part of the full ABI contract and must be carried so the
// callback cache is complete; allow them to sit unread until the consuming
// code paths land.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct HostCallbacks {
    pub host: HostHandle,
    pub tracing_register_callsite: unsafe extern "C" fn(
        host: HostHandle,
        target_ptr: *const u8,
        target_len: usize,
        level: HostLogLevel,
    ) -> HostInterest,
    pub tracing_enabled: unsafe extern "C" fn(
        host: HostHandle,
        target_ptr: *const u8,
        target_len: usize,
        level: HostLogLevel,
    ) -> bool,
    pub tracing_emit: unsafe extern "C" fn(
        host: HostHandle,
        target_ptr: *const u8,
        target_len: usize,
        level: HostLogLevel,
        message_ptr: *const u8,
        message_len: usize,
        fields_msgpack_ptr: *const u8,
        fields_msgpack_len: usize,
    ),
    pub pubsub_publish: unsafe extern "C" fn(
        host: HostHandle,
        topic_ptr: *const u8,
        topic_len: usize,
        event_msgpack_ptr: *const u8,
        event_msgpack_len: usize,
    ),
    pub schema_register: unsafe extern "C" fn(
        host: HostHandle,
        canonical_id_ptr: *const u8,
        canonical_id_len: usize,
        yaml_ptr: *const u8,
        yaml_len: usize,
    ),
    pub schema_lookup: unsafe extern "C" fn(
        host: HostHandle,
        canonical_id_ptr: *const u8,
        canonical_id_len: usize,
        result_callback: extern "C" fn(userdata: *mut c_void, yaml_ptr: *const u8, yaml_len: usize),
        result_userdata: *mut c_void,
    ),
    pub iceoryx_log_emit: unsafe extern "C" fn(
        host: HostHandle,
        level: HostLogLevel,
        origin_ptr: *const u8,
        origin_len: usize,
        message_ptr: *const u8,
        message_len: usize,
    ),
    pub processor_register: unsafe extern "C" fn(
        host: HostHandle,
        descriptor_msgpack_ptr: *const u8,
        descriptor_msgpack_len: usize,
        vtable: *const ProcessorVTable,
    ) -> i32,
    /// Host-installed [`RuntimeContextVTable`] pointer.
    pub runtime_context_vtable: *const RuntimeContextVTable,
    /// Host-installed [`AudioClockVTable`] pointer.
    pub audio_clock_vtable: *const AudioClockVTable,
    /// Host-installed [`RuntimeOpsVTable`] pointer.
    pub runtime_ops_vtable: *const RuntimeOpsVTable,
    /// Host-installed [`GpuContextLimitedAccessVTable`] pointer. May be null.
    pub gpu_context_limited_access_vtable: *const GpuContextLimitedAccessVTable,
    /// Host-installed [`SurfaceStoreVTable`] pointer. May be null.
    pub surface_store_vtable: *const SurfaceStoreVTable,
    /// Host-installed [`GpuContextFullAccessVTable`] pointer. May be null.
    pub gpu_context_full_access_vtable: *const GpuContextFullAccessVTable,
    /// Host-installed `TextureRingMethodsVTable` pointer. May be null.
    pub texture_ring_methods_vtable: *const streamlib_plugin_abi::TextureRingMethodsVTable,
    /// Host-installed `VulkanComputeKernelMethodsVTable` pointer. May be null.
    pub vulkan_compute_kernel_methods_vtable:
        *const streamlib_plugin_abi::VulkanComputeKernelMethodsVTable,
    /// Host-installed `VulkanGraphicsKernelMethodsVTable` pointer. May be null.
    pub vulkan_graphics_kernel_methods_vtable:
        *const streamlib_plugin_abi::VulkanGraphicsKernelMethodsVTable,
    /// Host-installed `VulkanRayTracingKernelMethodsVTable` pointer. May be null.
    pub vulkan_ray_tracing_kernel_methods_vtable:
        *const streamlib_plugin_abi::VulkanRayTracingKernelMethodsVTable,
    /// Host-installed `VulkanAccelerationStructureMethodsVTable` pointer. May be null.
    pub vulkan_acceleration_structure_methods_vtable:
        *const streamlib_plugin_abi::VulkanAccelerationStructureMethodsVTable,
    /// Host-installed `RhiColorConverterMethodsVTable` pointer. May be null.
    pub rhi_color_converter_methods_vtable:
        *const streamlib_plugin_abi::RhiColorConverterMethodsVTable,
    /// Host-installed `RhiCommandRecorderMethodsVTable` pointer. May be null.
    pub rhi_command_recorder_methods_vtable:
        *const streamlib_plugin_abi::RhiCommandRecorderMethodsVTable,
    /// Host-installed [`streamlib_plugin_abi::OutputWriterVTable`] pointer.
    /// May be null when the host doesn't wire iceoryx2 transport.
    pub output_writer_vtable: *const streamlib_plugin_abi::OutputWriterVTable,
    /// Host-installed [`streamlib_plugin_abi::InputMailboxesVTable`] pointer.
    /// May be null when the host doesn't wire iceoryx2 transport.
    pub input_mailboxes_vtable: *const streamlib_plugin_abi::InputMailboxesVTable,
}

// Safety: every field is a fn pointer or a raw pointer the host promises
// stays valid for the cdylib's process lifetime.
unsafe impl Send for HostCallbacks {}
unsafe impl Sync for HostCallbacks {}

/// Per-plugin cache of the host's callback table. `OnceLock` semantics:
/// the cdylib's [`install_host_services`] writes once at register time;
/// subsequent reads retrieve the same value.
static HOST_CALLBACKS: OnceLock<HostCallbacks> = OnceLock::new();

/// Returns this plugin's callback table if [`install_host_services`] has
/// populated it. `None` before a cdylib has registered.
pub fn host_callbacks() -> Option<&'static HostCallbacks> {
    HOST_CALLBACKS.get()
}

// =============================================================================
// install_host_services — cdylib entry point
// =============================================================================

/// Wire the host's services into this plugin. Called by a plugin cdylib's
/// `STREAMLIB_PLUGIN.register` callback via the
/// [`streamlib_plugin_abi::export_plugin!`] macro.
///
/// Validates [`HostServices::abi_layout_version`] and every non-null inner
/// vtable's layout_version, stores the callback table in
/// [`HOST_CALLBACKS`], installs the cdylib's tracing forwarder + iceoryx2
/// log forwarder, and returns a [`RegisterHelper`].
///
/// # Returns
///
/// `Some(RegisterHelper)` on success. `None` on layout-version mismatch or
/// null pointer.
///
/// # Safety
///
/// `host_services_ptr` must point at a [`HostServices`] value initialized
/// by the host. The host's loader guarantees this.
pub unsafe fn install_host_services(host_services_ptr: *const c_void) -> Option<RegisterHelper> {
    if host_services_ptr.is_null() {
        return None;
    }

    // SAFETY: per the caller's promise. Read `abi_layout_version` before
    // touching any other field — if the layout doesn't match, the rest of
    // the struct's shape may have drifted.
    let services = unsafe { &*(host_services_ptr as *const HostServices) };

    if services.abi_layout_version != HOST_SERVICES_LAYOUT_VERSION {
        return None;
    }

    // Validate every inner vtable's layout_version before storing the
    // pointers. Inner vtables are validated only when non-null.
    use streamlib_plugin_abi::{
        AUDIO_CLOCK_VTABLE_LAYOUT_VERSION, GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION,
        RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION, RUNTIME_OPS_VTABLE_LAYOUT_VERSION,
    };
    if !services.runtime_context_vtable.is_null() {
        // SAFETY: when non-null this points at a `&'static
        // RuntimeContextVTable` owned by the host; `layout_version` is at
        // offset 0.
        let v = unsafe { (*services.runtime_context_vtable).layout_version };
        if v != RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.audio_clock_vtable.is_null() {
        // SAFETY: same shape as runtime_context_vtable.
        let v = unsafe { (*services.audio_clock_vtable).layout_version };
        if v != AUDIO_CLOCK_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.runtime_ops_vtable.is_null() {
        // SAFETY: same shape as runtime_context_vtable.
        let v = unsafe { (*services.runtime_ops_vtable).layout_version };
        if v != RUNTIME_OPS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.gpu_context_limited_access_vtable.is_null() {
        // SAFETY: same shape as runtime_context_vtable.
        let v = unsafe { (*services.gpu_context_limited_access_vtable).layout_version };
        if v != GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.surface_store_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations.
        let v = unsafe { (*services.surface_store_vtable).layout_version };
        if v != SURFACE_STORE_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.gpu_context_full_access_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations.
        let v = unsafe { (*services.gpu_context_full_access_vtable).layout_version };
        if v != GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.texture_ring_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations.
        let v = unsafe { (*services.texture_ring_methods_vtable).layout_version };
        if v != streamlib_plugin_abi::TEXTURE_RING_METHODS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.vulkan_compute_kernel_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations.
        let v = unsafe { (*services.vulkan_compute_kernel_methods_vtable).layout_version };
        if v != streamlib_plugin_abi::VULKAN_COMPUTE_KERNEL_METHODS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.vulkan_graphics_kernel_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations.
        let v = unsafe { (*services.vulkan_graphics_kernel_methods_vtable).layout_version };
        if v != streamlib_plugin_abi::VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.vulkan_ray_tracing_kernel_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations.
        let v = unsafe { (*services.vulkan_ray_tracing_kernel_methods_vtable).layout_version };
        if v != streamlib_plugin_abi::VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services
        .vulkan_acceleration_structure_methods_vtable
        .is_null()
    {
        // SAFETY: same shape as the other vtable validations.
        let v = unsafe { (*services.vulkan_acceleration_structure_methods_vtable).layout_version };
        if v != streamlib_plugin_abi::VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.rhi_color_converter_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations.
        let v = unsafe { (*services.rhi_color_converter_methods_vtable).layout_version };
        if v != streamlib_plugin_abi::RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.rhi_command_recorder_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations.
        let v = unsafe { (*services.rhi_command_recorder_methods_vtable).layout_version };
        if v != streamlib_plugin_abi::RHI_COMMAND_RECORDER_METHODS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.output_writer_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations.
        let v = unsafe { (*services.output_writer_vtable).layout_version };
        if v != streamlib_plugin_abi::OUTPUT_WRITER_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.input_mailboxes_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations.
        let v = unsafe { (*services.input_mailboxes_vtable).layout_version };
        if v != streamlib_plugin_abi::INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }

    let callbacks = HostCallbacks {
        host: services.host,
        tracing_register_callsite: services.tracing_register_callsite,
        tracing_enabled: services.tracing_enabled,
        tracing_emit: services.tracing_emit,
        pubsub_publish: services.pubsub_publish,
        schema_register: services.schema_register,
        schema_lookup: services.schema_lookup,
        iceoryx_log_emit: services.iceoryx_log_emit,
        processor_register: services.processor_register,
        runtime_context_vtable: services.runtime_context_vtable,
        audio_clock_vtable: services.audio_clock_vtable,
        runtime_ops_vtable: services.runtime_ops_vtable,
        gpu_context_limited_access_vtable: services.gpu_context_limited_access_vtable,
        surface_store_vtable: services.surface_store_vtable,
        gpu_context_full_access_vtable: services.gpu_context_full_access_vtable,
        texture_ring_methods_vtable: services.texture_ring_methods_vtable,
        vulkan_compute_kernel_methods_vtable: services.vulkan_compute_kernel_methods_vtable,
        vulkan_graphics_kernel_methods_vtable: services.vulkan_graphics_kernel_methods_vtable,
        vulkan_ray_tracing_kernel_methods_vtable: services.vulkan_ray_tracing_kernel_methods_vtable,
        vulkan_acceleration_structure_methods_vtable: services
            .vulkan_acceleration_structure_methods_vtable,
        rhi_color_converter_methods_vtable: services.rhi_color_converter_methods_vtable,
        rhi_command_recorder_methods_vtable: services.rhi_command_recorder_methods_vtable,
        output_writer_vtable: services.output_writer_vtable,
        input_mailboxes_vtable: services.input_mailboxes_vtable,
    };

    // Cache the callbacks BEFORE installing tracing — the
    // `ForwardingSubscriber` reads `HOST_CALLBACKS` on every emit.
    let _ = HOST_CALLBACKS.set(callbacks);

    // Install the tracing forwarder as the cdylib's global dispatcher.
    forwarding_subscriber::install_for_self();

    // Install the iceoryx2 log forwarder.
    iceoryx2_log_forwarder::install_for_self();

    Some(RegisterHelper {})
}

/// Helper handed back to the cdylib's `export_plugin!` macro for
/// registering processors with the host's registry. The `register::<P>()`
/// call shape is source-compatible with the engine's host-side helper.
pub struct RegisterHelper {}

impl RegisterHelper {
    /// Register a processor type with the host's registry.
    ///
    /// Builds the static per-P [`ProcessorVTable`], serializes
    /// `P::descriptor()` to msgpack, and calls the host's
    /// `processor_register` callback. In a cdylib the callback table was
    /// populated by [`install_host_services`]; if it wasn't (no host
    /// installed), registration is silently skipped — the cdylib never
    /// owns a host-static `PROCESSOR_REGISTRY`.
    pub fn register<P>(&self)
    where
        P: crate::processors::GeneratedProcessor + 'static,
        P::Config: crate::processors::Config,
    {
        if let Some(callbacks) = host_callbacks() {
            register_via_callback::<P>(callbacks);
        }
        // No host-static arm: the engine-free SDK has no `PROCESSOR_REGISTRY`.
    }
}

/// Cdylib-side registration: build a vtable + descriptor msgpack and call
/// the host's `processor_register` callback.
fn register_via_callback<P>(callbacks: &HostCallbacks)
where
    P: crate::processors::GeneratedProcessor + 'static,
    P::Config: crate::processors::Config,
{
    let descriptor = match <P as crate::processors::GeneratedProcessor>::descriptor() {
        Some(d) => d,
        None => {
            tracing::warn!(
                "Processor {} has no descriptor, skipping registration",
                std::any::type_name::<P>()
            );
            return;
        }
    };

    let descriptor_msgpack = match rmp_serde::to_vec_named(&descriptor) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(
                "Failed to serialize descriptor for {}: {}",
                std::any::type_name::<P>(),
                e
            );
            return;
        }
    };

    let vtable = processor_vtable::vtable_for::<P>();

    // SAFETY: msgpack bytes and vtable pointer live in this plugin's
    // process address space for the duration of the call. The host's
    // implementation copies any data it needs to retain.
    let rc = unsafe {
        (callbacks.processor_register)(
            callbacks.host,
            descriptor_msgpack.as_ptr(),
            descriptor_msgpack.len(),
            vtable as *const ProcessorVTable,
        )
    };

    if rc != 0 {
        tracing::warn!(
            "processor_register for {} returned non-zero rc={}",
            descriptor.name,
            rc
        );
    }
}

// =============================================================================
// Plugin ABI conversions (cdylib-consumed)
// =============================================================================

/// Map a [`tracing::Level`] to the ABI's [`HostLogLevel`]. Consumed by the
/// cdylib's tracing forwarder.
pub(crate) fn tracing_level_to_host(level: tracing::Level) -> HostLogLevel {
    match level {
        tracing::Level::TRACE => HostLogLevel::Trace,
        tracing::Level::DEBUG => HostLogLevel::Debug,
        tracing::Level::INFO => HostLogLevel::Info,
        tracing::Level::WARN => HostLogLevel::Warn,
        tracing::Level::ERROR => HostLogLevel::Error,
    }
}

/// Map the ABI's [`HostInterest`] to a [`tracing::subscriber::Interest`].
/// Consumed by the cdylib's tracing forwarder.
pub(crate) fn host_interest_to_tracing(interest: HostInterest) -> tracing::subscriber::Interest {
    match interest {
        HostInterest::Never => tracing::subscriber::Interest::never(),
        HostInterest::Sometimes => tracing::subscriber::Interest::sometimes(),
        HostInterest::Always => tracing::subscriber::Interest::always(),
    }
}
