// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Plugin ABI host-services callback table.
//!
//! Companion to `streamlib-plugin-abi`'s [`HostServices`] ABI
//! contract. This module owns:
//!
//! - **Host-side callback impls** (`host_tracing_emit`,
//!   `host_pubsub_publish`, `host_schema_register`,
//!   `host_schema_lookup`, `host_iceoryx_log_emit`,
//!   `host_processor_register`) that the host's loader writes into a
//!   [`HostServices`] struct before invoking a cdylib's
//!   `STREAMLIB_PLUGIN.register` callback.
//! - **Cdylib-side `install_host_services` helper** that the cdylib's
//!   `export_plugin!` macro calls at register time. The helper
//!   validates layout, stores the callback table in a per-plugin
//!   [`HOST_CALLBACKS`] static, caches the host's tokio handle in
//!   [`HOST_TOKIO_HANDLE`] for cdylib-side async-lifecycle wrappers,
//!   installs the cdylib's tracing `ForwardingSubscriber` and
//!   iceoryx2 `Log` forwarder, and returns a [`RegisterHelper`] for
//!   the macro to register processors with.
//!
//! # Why this shape
//!
//! Rust mangled statics aren't in the dynsym table — every linked
//! copy of streamlib-engine (host binary, every dlopen'd cdylib) has
//! its own [`PUBSUB`], its own schema registry, its own
//! `tracing-core::GLOBAL_DISPATCH`, its own `iceoryx2_log::LOGGER`.
//! Passing `&'static T` references across the plugin ABI would couple
//! every consumer to byte-identical type layouts across plugins,
//! breaking streamlib's multi-builder deployment model.
//!
//! The callback-table shape removes that coupling: only `extern "C"
//! fn` signatures and primitive payloads cross the wire. The cdylib's
//! statically-linked engine copy keeps its own statics, but the read
//! paths through them (`PUBSUB.publish`, `register_schema`,
//! `get_embedded_schema_definition`, `tracing::*!`,
//! `iceoryx2_log::*`) route through the host's fn pointers instead
//! of through the local plugin's state.
//!
//! Processor registration follows the same shape: cdylib's
//! `RegisterHelper::register::<P>()` monomorphizes a [`ProcessorVTable`]
//! per processor type P and calls the host's `processor_register`
//! callback with the descriptor msgpack + vtable. The host's factory
//! stores `(descriptor, &'static ProcessorVTable)` and dispatches
//! every host-called method through extern "C" — retiring the
//! `Box<dyn DynGeneratedProcessor>` dyn-trait crossing class.
//!
//! # Deployment model this enables
//!
//! Computer A builds the host binary, computer B builds packages via
//! CI, computer C ships their own packages — all using different
//! rustc minor versions and different transitive-dep resolutions —
//! interoperate as long as they target the same triple and link the
//! same [`streamlib_plugin_abi::STREAMLIB_ABI_VERSION`]. No
//! commit-level coupling, no shared Cargo.lock.

use std::ffi::c_void;
use std::sync::OnceLock;

use streamlib_plugin_abi::{
    AudioClockVTable, GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION, GpuContextFullAccessVTable,
    GpuContextLimitedAccessVTable, HOST_SERVICES_LAYOUT_VERSION, HostHandle, HostInterest,
    HostLogLevel, HostServices, PROCESSOR_VTABLE_LAYOUT_VERSION, ProcessorVTable,
    RuntimeContextVTable, RuntimeOpsVTable, SURFACE_STORE_VTABLE_LAYOUT_VERSION,
    SurfaceStoreVTable,
};

// tokio is not exposed across the ABI. Lifecycle methods are
// synchronous at the trait surface; plugins that need async
// lifecycle work bring their own runtime. The host's tokio runtime
// stays invisible to plugins.

use crate::core::pubsub::Event;

mod shared;

mod acceleration_structure;
mod audio_clock;
mod color_converter;
mod command_recorder;
mod compute_kernel;
mod gpu_context;
mod input_mailboxes;
mod output_writer;
mod runtime_context;
mod runtime_ops;
mod surface_store;
mod texture_ring;
mod vulkan_kernels;
pub use acceleration_structure::{
    HOST_VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE,
    host_vulkan_acceleration_structure_methods_vtable,
};
pub use audio_clock::{HOST_AUDIO_CLOCK_VTABLE, host_audio_clock_vtable};
pub use color_converter::{
    HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE, host_rhi_color_converter_methods_vtable,
};
pub use command_recorder::{
    HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE, host_rhi_command_recorder_methods_vtable,
};
pub use compute_kernel::{
    HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE, host_vulkan_compute_kernel_methods_vtable,
};
pub use gpu_context::{
    HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE, HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE,
    host_gpu_context_full_access_vtable, host_gpu_context_limited_access_vtable,
};
use input_mailboxes::HOST_INPUT_MAILBOXES_VTABLE;
pub use input_mailboxes::host_input_mailboxes_vtable;
use output_writer::HOST_OUTPUT_WRITER_VTABLE;
pub use output_writer::host_output_writer_vtable;
pub use runtime_context::{HOST_RUNTIME_CONTEXT_VTABLE, host_runtime_context_vtable};
pub use runtime_ops::{
    HOST_RUNTIME_OPS_VTABLE, host_runtime_ops_vtable, install_host_runtime_tokio_handle,
};
pub use surface_store::{HOST_SURFACE_STORE_VTABLE, host_surface_store_vtable};
pub use texture_ring::{HOST_TEXTURE_RING_METHODS_VTABLE, host_texture_ring_methods_vtable};
pub use vulkan_kernels::{
    HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE, HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE,
    host_vulkan_graphics_kernel_methods_vtable, host_vulkan_ray_tracing_kernel_methods_vtable,
};

// =============================================================================
// HostCallbacks — per-plugin cache of the host's fn pointers
// =============================================================================

/// Cached copy of the host's callback table, stored in
/// [`HOST_CALLBACKS`] by `install_host_services` so the cdylib's
/// PUBSUB / schema-registry / tracing / iceoryx2-log forwarders can
/// reach the host without indirecting through [`HostServices`] on
/// every call.
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
    /// v3: host-installed [`RuntimeContextVTable`] pointer. Cached so
    /// the cdylib's shim constructors don't read [`HostServices`] on
    /// every shim build. The cdylib MUST read this from the cache
    /// (or `HostServices` direct) rather than reach for its local
    /// `&HOST_RUNTIME_CONTEXT_VTABLE` static — the local copy's fn
    /// pointers would dispatch into cdylib code instead of host code,
    /// which would break the no-shared-type-crossing invariant.
    pub runtime_context_vtable: *const RuntimeContextVTable,
    /// v3: host-installed [`AudioClockVTable`] pointer. Same rule as
    /// `runtime_context_vtable`.
    pub audio_clock_vtable: *const AudioClockVTable,
    /// v3: host-installed [`RuntimeOpsVTable`] pointer.
    pub runtime_ops_vtable: *const RuntimeOpsVTable,
    /// Host-installed [`GpuContextLimitedAccessVTable`] pointer.
    /// May be null on hosts that don't ship a GpuContext; cdylib
    /// must check before dispatching.
    pub gpu_context_limited_access_vtable: *const GpuContextLimitedAccessVTable,
    /// Host-installed [`SurfaceStoreVTable`] pointer. May be null
    /// on hosts that don't ship a `SurfaceStore`; cdylib must check
    /// before dispatching. Sourced from
    /// [`HostServices::surface_store_vtable`] at install time.
    pub surface_store_vtable: *const SurfaceStoreVTable,
    /// Host-installed [`GpuContextFullAccessVTable`] pointer. May be
    /// null on hosts that don't ship a GpuContext; cdylib must check
    /// before dispatching. Reachable from cdylib code only inside an
    /// `escalate(|full| ...)` scope (C3 wires the scope-token
    /// machinery). Sourced from
    /// [`HostServices::gpu_context_full_access_vtable`] at install
    /// time.
    pub gpu_context_full_access_vtable: *const GpuContextFullAccessVTable,
    /// Host-installed [`TextureRingMethodsVTable`] pointer. May be
    /// null on hosts that don't ship a GpuContext; cdylib must check
    /// before dispatching. Sourced from
    /// [`HostServices::texture_ring_methods_vtable`] at install time
    /// (issue #907 Phase E PR 1/5).
    pub texture_ring_methods_vtable: *const streamlib_plugin_abi::TextureRingMethodsVTable,
    /// Host-installed [`VulkanComputeKernelMethodsVTable`] pointer.
    /// May be null on hosts that don't ship a GpuContext; cdylib
    /// must check before dispatching. Sourced from
    /// [`HostServices::vulkan_compute_kernel_methods_vtable`] at
    /// install time (issue #907 Phase E PR 2/5).
    pub vulkan_compute_kernel_methods_vtable:
        *const streamlib_plugin_abi::VulkanComputeKernelMethodsVTable,
    /// Host-installed [`VulkanGraphicsKernelMethodsVTable`] pointer.
    /// May be null on hosts that don't ship a GpuContext; cdylib
    /// must check before dispatching. Sourced from
    /// [`HostServices::vulkan_graphics_kernel_methods_vtable`] at
    /// install time (issue #907 Phase E PR 3/5).
    pub vulkan_graphics_kernel_methods_vtable:
        *const streamlib_plugin_abi::VulkanGraphicsKernelMethodsVTable,
    /// Host-installed [`VulkanRayTracingKernelMethodsVTable`] pointer.
    /// May be null on hosts that don't ship a GpuContext; cdylib
    /// must check before dispatching. Sourced from
    /// [`HostServices::vulkan_ray_tracing_kernel_methods_vtable`] at
    /// install time (issue #907 Phase E PR 4/5).
    pub vulkan_ray_tracing_kernel_methods_vtable:
        *const streamlib_plugin_abi::VulkanRayTracingKernelMethodsVTable,
    /// Host-installed
    /// [`VulkanAccelerationStructureMethodsVTable`] pointer. May be
    /// null on hosts that don't ship a GpuContext; cdylib must check
    /// before dispatching. Sourced from
    /// [`HostServices::vulkan_acceleration_structure_methods_vtable`]
    /// at install time (issue #907 Phase E PR 5/5).
    pub vulkan_acceleration_structure_methods_vtable:
        *const streamlib_plugin_abi::VulkanAccelerationStructureMethodsVTable,
    /// Host-installed [`RhiColorConverterMethodsVTable`] pointer.
    /// May be null on hosts that don't ship a GpuContext; cdylib
    /// must check before dispatching. Sourced from
    /// [`HostServices::rhi_color_converter_methods_vtable`] at
    /// install time (Phase E sub-lift slice A).
    pub rhi_color_converter_methods_vtable:
        *const streamlib_plugin_abi::RhiColorConverterMethodsVTable,
    /// Host-installed [`RhiCommandRecorderMethodsVTable`] pointer.
    /// May be null on hosts that don't ship a GpuContext; cdylib
    /// must check before dispatching. Sourced from
    /// [`HostServices::rhi_command_recorder_methods_vtable`] at
    /// install time (Phase E sub-lift slice B).
    pub rhi_command_recorder_methods_vtable:
        *const streamlib_plugin_abi::RhiCommandRecorderMethodsVTable,
    /// Host-installed [`OutputWriterVTable`] pointer. May be null
    /// when the host doesn't wire iceoryx2 transport; cdylib's
    /// `OutputWriter` PluginAbiObject methods short-circuit cleanly when
    /// the vtable is null. Sourced from
    /// [`HostServices::output_writer_vtable`] at install time
    /// (issue #894).
    pub output_writer_vtable: *const streamlib_plugin_abi::OutputWriterVTable,
    /// Host-installed [`InputMailboxesVTable`] pointer. May be
    /// null when the host doesn't wire iceoryx2 transport; cdylib's
    /// `InputMailboxes` PluginAbiObject methods short-circuit cleanly when
    /// the vtable is null. Sourced from
    /// [`HostServices::input_mailboxes_vtable`] at install time
    /// (issue #894).
    pub input_mailboxes_vtable: *const streamlib_plugin_abi::InputMailboxesVTable,
}

// Safety: every field is a fn pointer or a raw pointer the host
// promises stays valid for the cdylib's process lifetime.
unsafe impl Send for HostCallbacks {}
unsafe impl Sync for HostCallbacks {}

/// Per-plugin cache of the host's callback table. `OnceLock` semantics:
/// the cdylib's `install_host_services` writes once at register
/// time; subsequent reads from `PUBSUB.publish`, `register_schema`,
/// the tracing `ForwardingSubscriber`, and the iceoryx2 forwarder
/// retrieve the same value. **The host binary never populates this**
/// — host-side code reads its local statics directly, bypassing the
/// callback table.
static HOST_CALLBACKS: OnceLock<HostCallbacks> = OnceLock::new();

/// Returns this plugin's callback table if a cdylib's
/// `install_host_services` has populated it. `None` in the host
/// binary; `Some(_)` in any cdylib that has registered.
pub fn host_callbacks() -> Option<&'static HostCallbacks> {
    HOST_CALLBACKS.get()
}

// =============================================================================
// install_host_services — cdylib entry point
// =============================================================================

/// Wire the host's services into this plugin. Called by a plugin
/// cdylib's `STREAMLIB_PLUGIN.register` callback via the
/// [`streamlib_plugin_abi::export_plugin!`] macro.
///
/// Validates [`HostServices::abi_layout_version`] against
/// [`HOST_SERVICES_LAYOUT_VERSION`], stores the callback table in
/// [`HOST_CALLBACKS`], installs the cdylib's tracing
/// [`ForwardingSubscriber`] as the per-plugin `GLOBAL_DISPATCH`,
/// installs the cdylib's iceoryx2 `Log` forwarder, and returns a
/// [`RegisterHelper`] the macro uses to register processor types
/// with the host's registry.
///
/// # Returns
///
/// `Some(RegisterHelper)` on success. `None` on layout-version
/// mismatch or null pointer — the macro short-circuits processor
/// registration, and the host's post-call "processor not registered"
/// check surfaces a `Configuration` error.
///
/// # Safety
///
/// `host_services_ptr` must point at a [`HostServices`] value
/// initialized by the host. The host's loader guarantees this.
pub unsafe fn install_host_services(host_services_ptr: *const c_void) -> Option<RegisterHelper> {
    if host_services_ptr.is_null() {
        return None;
    }

    // SAFETY: per the caller's promise. Read `abi_layout_version`
    // before touching any other field — if the layout doesn't match,
    // the rest of the struct's shape may have drifted.
    let services = unsafe { &*(host_services_ptr as *const HostServices) };

    if services.abi_layout_version != HOST_SERVICES_LAYOUT_VERSION {
        // Logging hasn't been wired yet (the forwarder install is
        // below); the host detects the failure via the post-call
        // "processor not registered" check.
        return None;
    }

    // Validate every inner vtable's layout_version before storing the
    // pointers. The outer `abi_layout_version` only covers the wire
    // shape of [`HostServices`] itself; a host that bumped, say, the
    // GpuContextLimitedAccessVTable to v4 but kept HostServices v4
    // would otherwise silently call through mismatched offsets from a
    // v3-built cdylib. Mismatch → refuse the install cleanly; the
    // host's post-call "processor not registered" check surfaces the
    // failure. (Inner vtables are validated only when non-null. The
    // GPU vtable pointer may legitimately be null on hosts that don't
    // ship a GpuContext, per `HOST_SERVICES_LAYOUT_VERSION` v4 docs.)
    use streamlib_plugin_abi::{
        AUDIO_CLOCK_VTABLE_LAYOUT_VERSION, GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION,
        RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION, RUNTIME_OPS_VTABLE_LAYOUT_VERSION,
    };
    if !services.runtime_context_vtable.is_null() {
        // SAFETY: per the wire contract, when non-null this points at
        // a `&'static RuntimeContextVTable` owned by the host. The
        // first u32 in the struct is `layout_version` (pinned at
        // offset 0 by the layout-regression tests).
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
        // SAFETY: same shape as runtime_context_vtable. Null is
        // allowed (host has no GpuContext); only non-null pointers
        // are version-validated.
        let v = unsafe { (*services.gpu_context_limited_access_vtable).layout_version };
        if v != GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.surface_store_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations. Null
        // is allowed (host has no SurfaceStore); only non-null
        // pointers are version-validated.
        let v = unsafe { (*services.surface_store_vtable).layout_version };
        if v != SURFACE_STORE_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.gpu_context_full_access_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations. Null
        // is allowed (host has no GpuContext); only non-null pointers
        // are version-validated.
        let v = unsafe { (*services.gpu_context_full_access_vtable).layout_version };
        if v != GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.texture_ring_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations. Null
        // is allowed (host has no GpuContext); only non-null pointers
        // are version-validated.
        let v = unsafe { (*services.texture_ring_methods_vtable).layout_version };
        if v != streamlib_plugin_abi::TEXTURE_RING_METHODS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.vulkan_compute_kernel_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations. Null
        // is allowed (host has no GpuContext); only non-null pointers
        // are version-validated.
        let v = unsafe { (*services.vulkan_compute_kernel_methods_vtable).layout_version };
        if v != streamlib_plugin_abi::VULKAN_COMPUTE_KERNEL_METHODS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.vulkan_graphics_kernel_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations. Null
        // is allowed (host has no GpuContext); only non-null pointers
        // are version-validated.
        let v = unsafe { (*services.vulkan_graphics_kernel_methods_vtable).layout_version };
        if v != streamlib_plugin_abi::VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.vulkan_ray_tracing_kernel_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations. Null
        // is allowed (host has no GpuContext); only non-null pointers
        // are version-validated.
        let v = unsafe { (*services.vulkan_ray_tracing_kernel_methods_vtable).layout_version };
        if v != streamlib_plugin_abi::VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services
        .vulkan_acceleration_structure_methods_vtable
        .is_null()
    {
        // SAFETY: same shape as the other vtable validations. Null
        // is allowed (host has no GpuContext); only non-null pointers
        // are version-validated.
        let v = unsafe { (*services.vulkan_acceleration_structure_methods_vtable).layout_version };
        if v != streamlib_plugin_abi::VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.rhi_color_converter_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations. Null
        // is allowed (host has no GpuContext); only non-null pointers
        // are version-validated.
        let v = unsafe { (*services.rhi_color_converter_methods_vtable).layout_version };
        if v != streamlib_plugin_abi::RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.rhi_command_recorder_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations. Null
        // is allowed (host has no GpuContext); only non-null pointers
        // are version-validated.
        let v = unsafe { (*services.rhi_command_recorder_methods_vtable).layout_version };
        if v != streamlib_plugin_abi::RHI_COMMAND_RECORDER_METHODS_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.output_writer_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations. Null
        // is allowed (host does not wire iceoryx2 transport); only
        // non-null pointers are version-validated.
        let v = unsafe { (*services.output_writer_vtable).layout_version };
        if v != streamlib_plugin_abi::OUTPUT_WRITER_VTABLE_LAYOUT_VERSION {
            return None;
        }
    }
    if !services.input_mailboxes_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations. Null
        // is allowed (host does not wire iceoryx2 transport); only
        // non-null pointers are version-validated.
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
    // The cdylib's `tracing::*!()` macros now route every event
    // through the host's `tracing_emit` callback.
    crate::core::plugin::forwarding_subscriber::install_for_self();

    // Install the iceoryx2 log forwarder. The cdylib's iceoryx2-log
    // emits route through the host's `iceoryx_log_emit` callback.
    // Also raise the cdylib's iceoryx2-log level to Trace so the
    // host's filter sees every record; the host then decides via
    // its tracing pipeline what to actually emit.
    crate::core::plugin::iceoryx2_log_forwarder::install_for_self();

    Some(RegisterHelper {})
}

/// Helper handed back to the cdylib's `export_plugin!` macro for
/// registering processors with the host's registry. Source-compatible
/// with v1's `helper.register::<P>()` call shape — the implementation
/// now monomorphizes a [`ProcessorVTable`] per processor type and
/// routes through the host's `processor_register` callback instead
/// of dispatching through `&'static ProcessorInstanceFactory`.
pub struct RegisterHelper {}

impl RegisterHelper {
    /// Register a processor type with the host's registry.
    ///
    /// Builds the static per-P [`ProcessorVTable`], serializes
    /// `P::descriptor()` to msgpack, and calls the host's
    /// `processor_register` callback. Source-compatible at the call
    /// site (`helper.register::<P::Processor>()`).
    pub fn register<P>(&self)
    where
        P: crate::core::processors::GeneratedProcessor + 'static,
        P::Config: crate::core::processors::Config,
    {
        // Resolve the host's callback table. In a cdylib this was
        // populated by `install_host_services` above. In the host
        // process (where this code path also runs when a processor
        // is registered inline via `PROCESSOR_REGISTRY.register::<P>()`),
        // `HOST_CALLBACKS` is empty — the host-static path bypasses
        // the plugin ABI and registers directly with the factory.
        if let Some(callbacks) = host_callbacks() {
            register_via_callback::<P>(callbacks);
        } else {
            // Host-static path: same vtable shape, but registered
            // directly with the in-process factory (no plugin ABI hop).
            crate::core::processors::PROCESSOR_REGISTRY.register::<P>();
        }
    }
}

/// Cdylib-side registration: build a vtable + descriptor msgpack and
/// call the host's `processor_register` callback.
fn register_via_callback<P>(callbacks: &HostCallbacks)
where
    P: crate::core::processors::GeneratedProcessor + 'static,
    P::Config: crate::core::processors::Config,
{
    let descriptor = match <P as crate::core::processors::GeneratedProcessor>::descriptor() {
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

    let vtable = crate::core::plugin::processor_vtable::vtable_for::<P>();

    // SAFETY: msgpack bytes and vtable pointer live in this plugin's
    // process address space for the duration of the call. The host's
    // implementation copies any data it needs to retain (the
    // descriptor is decoded into a `ProcessorDescriptor`; the vtable
    // pointer is stored as-is and the cdylib is pinned via
    // `LOADED_PLUGIN_LIBRARIES`).
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
// Host-side callback implementations
// =============================================================================

/// Concrete host-side service table the host's loader plugs into a
/// [`HostServices`] payload via [`runtime_facing::host_services_for_self`].
///
/// Holds the host's iceoryx2 node. Lives behind the
/// [`HostServices::host`] opaque pointer.
pub struct HostServiceImpls {
    pub iceoryx2_node: crate::iceoryx2::Iceoryx2Node,
}

unsafe impl Send for HostServiceImpls {}
unsafe impl Sync for HostServiceImpls {}

// ---------------- Panic safety helpers ----------------
//
// Unwinding through an `extern "C"` boundary is undefined behaviour.
// Every host-side callback below routes its body through
// [`run_host_extern_c`] so a panic in host code is caught and
// converted to a logged error plus a sensible default return value
// at the plugin ABI, instead of corrupting the cdylib's stack.
//
// The default-on-panic value per callback type:
//   - void                  → `()`
//   - bool                  → `false`
//   - u32 / usize           → `0`
//   - isize (used by id_copy with `-1` = None) → `-1`
//   - i32  (status codes; non-zero = error)   → `1`
//   - HostInterest          → `HostInterest::Never`
//   - `*const c_void` / `*mut u8` / `*const ProcessorVTable` → `null` / `null_mut`

/// Run an extern "C" callback body inside [`std::panic::catch_unwind`].
/// Panics are logged and converted to `default_on_panic` so the plugin
/// ABI stays sound. `callback_name` is included in the error
/// log to make the source obvious in mixed-callback traces.
///
/// Uses [`std::panic::AssertUnwindSafe`] internally because callback
/// bodies routinely touch raw pointers and `*mut` outputs that aren't
/// `UnwindSafe` by default — the pointer dereferences are sound under
/// the plugin ABI contract regardless of unwinding.
///
/// Re-export of the canonical panic-safety helper in
/// [`streamlib_adapter_abi::ffi`]. Every extern "C" boundary
/// crossing in the engine — host-side and cdylib-side — must route
/// through this wrapper so all six consumers (engine + five surface
/// adapters) share a single implementation.
pub(crate) use streamlib_adapter_abi::ffi::run_host_extern_c;

unsafe extern "C" fn host_tracing_register_callsite(
    _host: HostHandle,
    _target_ptr: *const u8,
    _target_len: usize,
    _level: HostLogLevel,
) -> HostInterest {
    run_host_extern_c(
        "host_tracing_register_callsite",
        || {
            // The host's `EnvFilter` filters at emit time via
            // `host_tracing_emit` (it calls `tracing::event!` which
            // fires through the host's subscriber chain). Returning
            // `Always` here tells the cdylib's forwarding
            // `Subscriber` to cache "always emit" for the callsite —
            // every event reaches `host_tracing_emit`, where the
            // host's filter actually decides.
            //
            // Trade-off: cdylib pays for the plugin ABI hop even on
            // filtered-out events, plus a string copy of the
            // message. A future refinement could push a (target,
            // level)-keyed pre-filter here; the current ABI shape
            // doesn't constrain that.
            HostInterest::Always
        },
        HostInterest::Never,
    )
}

unsafe extern "C" fn host_tracing_enabled(
    _host: HostHandle,
    _target_ptr: *const u8,
    _target_len: usize,
    _level: HostLogLevel,
) -> bool {
    run_host_extern_c(
        "host_tracing_enabled",
        || {
            // Paired with `host_tracing_register_callsite` returning
            // `Always`: this never fires from the cdylib side. Kept
            // in the ABI so a future register_callsite that returns
            // `Sometimes` has the per-event enable hook available.
            true
        },
        false,
    )
}

unsafe extern "C" fn host_tracing_emit(
    _host: HostHandle,
    target_ptr: *const u8,
    target_len: usize,
    level: HostLogLevel,
    message_ptr: *const u8,
    message_len: usize,
    fields_msgpack_ptr: *const u8,
    fields_msgpack_len: usize,
) {
    run_host_extern_c(
        "host_tracing_emit",
        || {
            let target = unsafe {
                std::str::from_utf8_unchecked(std::slice::from_raw_parts(target_ptr, target_len))
            };
            let message = if message_len == 0 {
                ""
            } else {
                unsafe {
                    std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                        message_ptr,
                        message_len,
                    ))
                }
            };
            let level_val = host_log_level_to_tracing(level);
            let fields_bytes = if fields_msgpack_len == 0 || fields_msgpack_ptr.is_null() {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(fields_msgpack_ptr, fields_msgpack_len) }
            };

            // Decode the structured fields (msgpack map) and replay them
            // through the host's tracing pipeline alongside `message`. The
            // simplest shape that preserves field fidelity is to log via
            // the host's own subscriber using `event!`-style emission with
            // a single `message` field — structured fields go into the
            // event's value set as serde-derived JSON values, captured by
            // `JsonlSinkLayer::Capture::record_*`.
            let fields_map: serde_json::Value =
                rmp_serde::from_slice(fields_bytes).unwrap_or(serde_json::Value::Null);

            emit_via_host_dispatch(target, level_val, message, &fields_map);
        },
        (),
    )
}

unsafe extern "C" fn host_pubsub_publish(
    _host: HostHandle,
    topic_ptr: *const u8,
    topic_len: usize,
    event_msgpack_ptr: *const u8,
    event_msgpack_len: usize,
) {
    run_host_extern_c(
        "host_pubsub_publish",
        || {
            let topic = unsafe {
                std::str::from_utf8_unchecked(std::slice::from_raw_parts(topic_ptr, topic_len))
            };
            let event_bytes =
                unsafe { std::slice::from_raw_parts(event_msgpack_ptr, event_msgpack_len) };
            let event: Event = match rmp_serde::from_slice(event_bytes) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(
                        target: "streamlib::plugin",
                        "host_pubsub_publish: failed to decode event from cdylib: {e}"
                    );
                    return;
                }
            };
            crate::core::pubsub::PUBSUB.publish(topic, &event);
        },
        (),
    )
}

unsafe extern "C" fn host_schema_register(
    _host: HostHandle,
    canonical_id_ptr: *const u8,
    canonical_id_len: usize,
    yaml_ptr: *const u8,
    yaml_len: usize,
) {
    run_host_extern_c(
        "host_schema_register",
        || {
            let canonical_id = unsafe {
                std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                    canonical_id_ptr,
                    canonical_id_len,
                ))
            };
            let yaml = unsafe {
                std::str::from_utf8_unchecked(std::slice::from_raw_parts(yaml_ptr, yaml_len))
            };
            crate::core::embedded_schemas::register_schema(canonical_id.to_string(), yaml);
        },
        (),
    )
}

unsafe extern "C" fn host_schema_lookup(
    _host: HostHandle,
    canonical_id_ptr: *const u8,
    canonical_id_len: usize,
    result_callback: extern "C" fn(*mut c_void, *const u8, usize),
    result_userdata: *mut c_void,
) {
    run_host_extern_c(
        "host_schema_lookup",
        || {
            let canonical_id = unsafe {
                std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                    canonical_id_ptr,
                    canonical_id_len,
                ))
            };
            match crate::core::embedded_schemas::get_embedded_schema_definition(canonical_id) {
                Some(yaml) => {
                    let bytes = yaml.as_bytes();
                    result_callback(result_userdata, bytes.as_ptr(), bytes.len());
                }
                None => {
                    result_callback(result_userdata, std::ptr::null(), 0);
                }
            }
        },
        (),
    )
}

unsafe extern "C" fn host_iceoryx_log_emit(
    _host: HostHandle,
    level: HostLogLevel,
    origin_ptr: *const u8,
    origin_len: usize,
    message_ptr: *const u8,
    message_len: usize,
) {
    run_host_extern_c(
        "host_iceoryx_log_emit",
        || {
            let origin = if origin_len == 0 {
                ""
            } else {
                unsafe {
                    std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                        origin_ptr, origin_len,
                    ))
                }
            };
            let message = if message_len == 0 {
                ""
            } else {
                unsafe {
                    std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                        message_ptr,
                        message_len,
                    ))
                }
            };
            // Forward into the host's tracing pipeline at the appropriate level.
            match level {
                HostLogLevel::Trace => {
                    tracing::trace!(target: "iceoryx2", origin = %origin, "{message}")
                }
                HostLogLevel::Debug => {
                    tracing::debug!(target: "iceoryx2", origin = %origin, "{message}")
                }
                HostLogLevel::Info => {
                    tracing::info!(target: "iceoryx2", origin = %origin, "{message}")
                }
                HostLogLevel::Warn => {
                    tracing::warn!(target: "iceoryx2", origin = %origin, "{message}")
                }
                HostLogLevel::Error => {
                    tracing::error!(target: "iceoryx2", origin = %origin, "{message}")
                }
            }
        },
        (),
    )
}

/// Host-side `processor_register` callback. Decodes the descriptor
/// msgpack and routes to the in-process registry's
/// `register_via_vtable` path. Returns 0 on success, non-zero on
/// descriptor decode failure, vtable layout-version mismatch, or
/// duplicate registration.
unsafe extern "C" fn host_processor_register(
    _host: HostHandle,
    descriptor_msgpack_ptr: *const u8,
    descriptor_msgpack_len: usize,
    vtable: *const ProcessorVTable,
) -> i32 {
    run_host_extern_c(
        "host_processor_register",
        || {
            if vtable.is_null() {
                tracing::warn!("host_processor_register: null vtable pointer");
                return -1;
            }

            let vtable_layout = unsafe { (*vtable).layout_version };
            if vtable_layout != PROCESSOR_VTABLE_LAYOUT_VERSION {
                tracing::warn!(
                    "host_processor_register: vtable layout version mismatch (got {}, expected {})",
                    vtable_layout,
                    PROCESSOR_VTABLE_LAYOUT_VERSION
                );
                return -2;
            }

            let descriptor_bytes = unsafe {
                std::slice::from_raw_parts(descriptor_msgpack_ptr, descriptor_msgpack_len)
            };
            let descriptor: crate::core::descriptors::ProcessorDescriptor =
                match rmp_serde::from_slice(descriptor_bytes) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!(
                            "host_processor_register: failed to decode descriptor msgpack: {e}"
                        );
                        return -3;
                    }
                };

            // SAFETY: `vtable` is `&'static ProcessorVTable` on the cdylib
            // side; the cdylib is pinned via `LOADED_PLUGIN_LIBRARIES`, so
            // the pointer outlives the host's usage.
            let vtable_ref: &'static ProcessorVTable = unsafe { &*vtable };

            // Cdylib-resident registration — the vtable's function
            // pointers target the cdylib's address space, so
            // lifecycle dispatch needs the `with_cdylib_scope` wrap
            // to give the cdylib body a `ScopeToken`-shaped
            // FullAccess that routes through the FullAccess vtable.
            match crate::core::processors::PROCESSOR_REGISTRY
                .register_via_vtable(descriptor, vtable_ref, /* cdylib_resident */ true)
            {
                Ok(()) => 0,
                Err(e) => {
                    tracing::warn!("host_processor_register: register_via_vtable failed: {e}");
                    -4
                }
            }
        },
        // Non-zero on panic = error. Discriminate from the explicit
        // failure codes (-1 .. -4) with a fresh value.
        -5,
    )
}

// =============================================================================
// Plugin ABI conversions
// =============================================================================

pub(crate) fn tracing_level_to_host(level: tracing::Level) -> HostLogLevel {
    match level {
        tracing::Level::TRACE => HostLogLevel::Trace,
        tracing::Level::DEBUG => HostLogLevel::Debug,
        tracing::Level::INFO => HostLogLevel::Info,
        tracing::Level::WARN => HostLogLevel::Warn,
        tracing::Level::ERROR => HostLogLevel::Error,
    }
}

pub(crate) fn host_log_level_to_tracing(level: HostLogLevel) -> tracing::Level {
    match level {
        HostLogLevel::Trace => tracing::Level::TRACE,
        HostLogLevel::Debug => tracing::Level::DEBUG,
        HostLogLevel::Info => tracing::Level::INFO,
        HostLogLevel::Warn => tracing::Level::WARN,
        HostLogLevel::Error => tracing::Level::ERROR,
    }
}

pub(crate) fn host_interest_to_tracing(interest: HostInterest) -> tracing::subscriber::Interest {
    match interest {
        HostInterest::Never => tracing::subscriber::Interest::never(),
        HostInterest::Sometimes => tracing::subscriber::Interest::sometimes(),
        HostInterest::Always => tracing::subscriber::Interest::always(),
    }
}

// =============================================================================
// Emit-via-host-dispatch — used by `host_tracing_emit`
// =============================================================================

/// Replay a cdylib-emitted event into the host's JSONL drain
/// pipeline.
///
/// `tracing::event!` macros can't take a runtime `target:` — they
/// expand into a static `Callsite` whose target is baked at compile
/// time. To support arbitrary cdylib targets we bypass tracing and
/// push a [`LogRecord`] directly into the host's drain worker via
/// the same queue the polyglot subprocess log-relay uses, by way of
/// [`crate::core::logging::push_polyglot_record`].
///
/// Trade-off: host-side `EnvFilter` filtering doesn't apply on this
/// path; cdylib code is responsible for its own level filtering
/// (the cdylib's `ForwardingSubscriber::register_callsite` queries
/// `host_tracing_register_callsite` and caches the result). The
/// drain queue is bounded so an over-emitting plugin still
/// drop-oldests gracefully.
fn emit_via_host_dispatch(
    target: &str,
    level: tracing::Level,
    message: &str,
    fields: &serde_json::Value,
) {
    use crate::core::logging::LogRecord;
    use crate::core::logging::push_polyglot_record;

    let attrs = match fields {
        serde_json::Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => std::collections::BTreeMap::new(),
    };

    let record = LogRecord {
        host_ts: crate::core::logging::now_ns(),
        level: (level).into(),
        target: target.to_string(),
        message: message.to_string(),
        pipeline_id: None,
        processor_id: None,
        rhi_op: None,
        intercepted: false,
        channel: None,
        attrs,
        source: None,
        source_ts: None,
        source_seq: None,
    };

    push_polyglot_record(record);
}

// =============================================================================
// Host-side static vtables (RuntimeContext / AudioClock / RuntimeOps)
// =============================================================================
//
// The host installs these `&'static` vtables into [`HostServices`] at
// `host_services_for_self` time. Every callback derefs the opaque
// `ctx` / `handle` pointer back to a host-owned Rust type and routes
// through that type's normal Rust accessor — `ctx` for the
// RuntimeContext vtable is a `*const RuntimeContext`, `handle` for
// the audio-clock vtable is a `*const SharedAudioClock`, and `handle`
// for the runtime-ops vtable is a `*const Arc<dyn RuntimeOperations>`.
// The cdylib treats them all as opaque, dispatching through fn

// =============================================================================
// runtime_facing — host-side payload builder
// =============================================================================

/// Host-facing helpers used by `Runner::add_module` to assemble a
/// [`HostServices`] payload pointing at this plugin's callback
/// implementations.
pub mod runtime_facing {
    use super::{
        HOST_AUDIO_CLOCK_VTABLE, HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE,
        HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE, HOST_INPUT_MAILBOXES_VTABLE,
        HOST_OUTPUT_WRITER_VTABLE, HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE,
        HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE, HOST_RUNTIME_CONTEXT_VTABLE,
        HOST_RUNTIME_OPS_VTABLE, HOST_SURFACE_STORE_VTABLE, HOST_TEXTURE_RING_METHODS_VTABLE,
        HOST_VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE,
        HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE, HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE,
        HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE, HostServiceImpls, host_iceoryx_log_emit,
        host_processor_register, host_pubsub_publish, host_schema_lookup, host_schema_register,
        host_tracing_emit, host_tracing_enabled, host_tracing_register_callsite,
    };
    use std::ffi::c_void;
    use std::sync::OnceLock;

    use streamlib_plugin_abi::{HOST_SERVICES_LAYOUT_VERSION, HostServices};

    /// Heap-allocated service impl table, leaked once per process.
    /// The `HostServices.host` opaque pointer points at this.
    static HOST_IMPLS: OnceLock<&'static HostServiceImpls> = OnceLock::new();

    fn host_impls_for_self(node: &crate::iceoryx2::Iceoryx2Node) -> &'static HostServiceImpls {
        HOST_IMPLS.get_or_init(|| {
            let impls = HostServiceImpls {
                iceoryx2_node: node.clone(),
            };
            Box::leak(Box::new(impls))
        })
    }

    /// Build a [`HostServices`] payload from this process's host
    /// callback impls. Callable repeatedly; the underlying
    /// [`HostServiceImpls`] is constructed once and reused for the
    /// process lifetime, matching `LOADED_PLUGIN_LIBRARIES`'s pinning
    /// lifetime for loaded cdylibs.
    pub fn host_services_for_self(node: &crate::iceoryx2::Iceoryx2Node) -> HostServices {
        let host_impls = host_impls_for_self(node);
        let host_handle = host_impls as *const HostServiceImpls as *const c_void;

        HostServices {
            abi_layout_version: HOST_SERVICES_LAYOUT_VERSION,
            _reserved_padding: 0,
            host: host_handle,
            tracing_register_callsite: host_tracing_register_callsite,
            tracing_enabled: host_tracing_enabled,
            tracing_emit: host_tracing_emit,
            pubsub_publish: host_pubsub_publish,
            schema_register: host_schema_register,
            schema_lookup: host_schema_lookup,
            iceoryx_log_emit: host_iceoryx_log_emit,
            processor_register: host_processor_register,
            runtime_context_vtable: &HOST_RUNTIME_CONTEXT_VTABLE,
            audio_clock_vtable: &HOST_AUDIO_CLOCK_VTABLE,
            runtime_ops_vtable: &HOST_RUNTIME_OPS_VTABLE,
            gpu_context_limited_access_vtable: &HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE,
            surface_store_vtable: &HOST_SURFACE_STORE_VTABLE,
            gpu_context_full_access_vtable: &HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE,
            texture_ring_methods_vtable: &HOST_TEXTURE_RING_METHODS_VTABLE,
            vulkan_compute_kernel_methods_vtable: &HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE,
            vulkan_graphics_kernel_methods_vtable: &HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE,
            vulkan_ray_tracing_kernel_methods_vtable:
                &HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE,
            vulkan_acceleration_structure_methods_vtable:
                &HOST_VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE,
            rhi_color_converter_methods_vtable: &HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE,
            rhi_command_recorder_methods_vtable: &HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE,
            output_writer_vtable: &HOST_OUTPUT_WRITER_VTABLE,
            input_mailboxes_vtable: &HOST_INPUT_MAILBOXES_VTABLE,
        }
    }
}

// =============================================================================
// FullAccess vtable callback-body tests (tier-1 — no GPU required)
// =============================================================================
//
// Tier-1 host-side wire-format tests for `HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE`.
// Each test invokes a vtable callback directly with a null `gpu_handle`
// (the path that runs before any `gpu.create_*_kernel` call), asserting
// the callback returns error code 1 + writes the expected message into
// the caller's error buffer + leaves the out-handle slot untouched.
//
// The success-path tests (real Arc<GpuContext>, valid descriptor, kernel
// handle minting) require a real Vulkan device and live in
// `tests/` under the `streamlib/hardware-tests` feature. The dlopen
// integration test that exercises the full cdylib → vtable → host chain
// arrives with C3.
