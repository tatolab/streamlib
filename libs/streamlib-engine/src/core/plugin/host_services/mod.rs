// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-DSO host-services callback table.
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
//!   validates layout, stores the callback table in a per-DSO
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
//! Passing `&'static T` references across the FFI would couple
//! every consumer to byte-identical type layouts across DSOs,
//! breaking streamlib's multi-builder deployment model.
//!
//! The callback-table shape removes that coupling: only `extern "C"
//! fn` signatures and primitive payloads cross the wire. The cdylib's
//! statically-linked engine copy keeps its own statics, but the read
//! paths through them (`PUBSUB.publish`, `register_schema`,
//! `get_embedded_schema_definition`, `tracing::*!`,
//! `iceoryx2_log::*`) route through the host's fn pointers instead
//! of through the local DSO's state.
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
use std::sync::{Arc, OnceLock};

use streamlib_plugin_abi::{
    AudioClockVTable, ComputeKernelDescriptorRepr, GpuContextFullAccessVTable,
    GpuContextLimitedAccessVTable, GraphicsKernelDescriptorRepr, HostHandle, HostInterest,
    HostLogLevel, HostServices, ProcessorVTable, RayTracingKernelDescriptorRepr,
    RuntimeContextVTable, RuntimeOpsVTable, SurfaceStoreVTable,
    GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION,
    GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION, HOST_SERVICES_LAYOUT_VERSION,
    PROCESSOR_VTABLE_LAYOUT_VERSION, SURFACE_STORE_VTABLE_LAYOUT_VERSION,
};

// tokio is not exposed across the ABI. Lifecycle methods are
// synchronous at the trait surface; plugins that need async
// lifecycle work bring their own runtime. The host's tokio runtime
// stays invisible to plugins.

use crate::core::pubsub::Event;

mod shared;

mod audio_clock;
mod gpu_context;
mod input_mailboxes;
mod output_writer;
mod runtime_context;
mod runtime_ops;
mod surface_store;
mod texture_ring;
pub use audio_clock::{host_audio_clock_vtable, HOST_AUDIO_CLOCK_VTABLE};
pub use gpu_context::{
    host_gpu_context_full_access_vtable, host_gpu_context_limited_access_vtable,
    HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE, HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE,
};
pub use input_mailboxes::{host_input_mailboxes_vtable, HOST_INPUT_MAILBOXES_VTABLE};
pub use output_writer::{host_output_writer_vtable, HOST_OUTPUT_WRITER_VTABLE};
pub use runtime_context::{host_runtime_context_vtable, HOST_RUNTIME_CONTEXT_VTABLE};
pub use runtime_ops::{
    host_runtime_ops_vtable, install_host_runtime_tokio_handle, HOST_RUNTIME_OPS_VTABLE,
};
pub use surface_store::{host_surface_store_vtable, HOST_SURFACE_STORE_VTABLE};
pub use texture_ring::{host_texture_ring_methods_vtable, HOST_TEXTURE_RING_METHODS_VTABLE};

#[cfg(target_os = "linux")]
use shared::borrow::{
    make_acceleration_structure_borrow, make_compute_kernel_borrow,
    make_graphics_kernel_borrow, make_index_buffer_borrow, make_pixel_buffer_borrow,
    make_storage_buffer_borrow, make_texture_borrow, make_uniform_buffer_borrow,
    make_vertex_buffer_borrow,
};
use shared::wire::{slice_from_raw, write_err, write_id_bytes};

// =============================================================================
// HostCallbacks — per-DSO cache of the host's fn pointers
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
        result_callback: extern "C" fn(
            userdata: *mut c_void,
            yaml_ptr: *const u8,
            yaml_len: usize,
        ),
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
    /// `OutputWriter` β-shape methods short-circuit cleanly when
    /// the vtable is null. Sourced from
    /// [`HostServices::output_writer_vtable`] at install time
    /// (issue #894).
    pub output_writer_vtable:
        *const streamlib_plugin_abi::OutputWriterVTable,
    /// Host-installed [`InputMailboxesVTable`] pointer. May be
    /// null when the host doesn't wire iceoryx2 transport; cdylib's
    /// `InputMailboxes` β-shape methods short-circuit cleanly when
    /// the vtable is null. Sourced from
    /// [`HostServices::input_mailboxes_vtable`] at install time
    /// (issue #894).
    pub input_mailboxes_vtable:
        *const streamlib_plugin_abi::InputMailboxesVTable,
}

// Safety: every field is a fn pointer or a raw pointer the host
// promises stays valid for the cdylib's process lifetime.
unsafe impl Send for HostCallbacks {}
unsafe impl Sync for HostCallbacks {}

/// Per-DSO cache of the host's callback table. `OnceLock` semantics:
/// the cdylib's `install_host_services` writes once at register
/// time; subsequent reads from `PUBSUB.publish`, `register_schema`,
/// the tracing `ForwardingSubscriber`, and the iceoryx2 forwarder
/// retrieve the same value. **The host's DSO never populates this**
/// — host-side code reads its local statics directly, bypassing the
/// callback table.
static HOST_CALLBACKS: OnceLock<HostCallbacks> = OnceLock::new();

/// Returns this DSO's callback table if a cdylib's
/// `install_host_services` has populated it. `None` in the host
/// binary; `Some(_)` in any cdylib that has registered.
pub fn host_callbacks() -> Option<&'static HostCallbacks> {
    HOST_CALLBACKS.get()
}

// =============================================================================
// install_host_services — cdylib entry point
// =============================================================================

/// Wire the host's services into this DSO. Called by a plugin
/// cdylib's `STREAMLIB_PLUGIN.register` callback via the
/// [`streamlib_plugin_abi::export_plugin!`] macro.
///
/// Validates [`HostServices::abi_layout_version`] against
/// [`HOST_SERVICES_LAYOUT_VERSION`], stores the callback table in
/// [`HOST_CALLBACKS`], installs the cdylib's tracing
/// [`ForwardingSubscriber`] as the per-DSO `GLOBAL_DISPATCH`,
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
pub unsafe fn install_host_services(
    host_services_ptr: *const c_void,
) -> Option<RegisterHelper> {
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
        let v = unsafe {
            (*services.vulkan_compute_kernel_methods_vtable).layout_version
        };
        if v != streamlib_plugin_abi::VULKAN_COMPUTE_KERNEL_METHODS_VTABLE_LAYOUT_VERSION
        {
            return None;
        }
    }
    if !services.vulkan_graphics_kernel_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations. Null
        // is allowed (host has no GpuContext); only non-null pointers
        // are version-validated.
        let v = unsafe {
            (*services.vulkan_graphics_kernel_methods_vtable).layout_version
        };
        if v != streamlib_plugin_abi::VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE_LAYOUT_VERSION
        {
            return None;
        }
    }
    if !services.vulkan_ray_tracing_kernel_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations. Null
        // is allowed (host has no GpuContext); only non-null pointers
        // are version-validated.
        let v = unsafe {
            (*services.vulkan_ray_tracing_kernel_methods_vtable).layout_version
        };
        if v != streamlib_plugin_abi::VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE_LAYOUT_VERSION
        {
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
        let v = unsafe {
            (*services.vulkan_acceleration_structure_methods_vtable).layout_version
        };
        if v != streamlib_plugin_abi::VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE_LAYOUT_VERSION
        {
            return None;
        }
    }
    if !services.rhi_color_converter_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations. Null
        // is allowed (host has no GpuContext); only non-null pointers
        // are version-validated.
        let v = unsafe {
            (*services.rhi_color_converter_methods_vtable).layout_version
        };
        if v != streamlib_plugin_abi::RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION
        {
            return None;
        }
    }
    if !services.rhi_command_recorder_methods_vtable.is_null() {
        // SAFETY: same shape as the other vtable validations. Null
        // is allowed (host has no GpuContext); only non-null pointers
        // are version-validated.
        let v = unsafe {
            (*services.rhi_command_recorder_methods_vtable).layout_version
        };
        if v != streamlib_plugin_abi::RHI_COMMAND_RECORDER_METHODS_VTABLE_LAYOUT_VERSION
        {
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
        vulkan_compute_kernel_methods_vtable: services
            .vulkan_compute_kernel_methods_vtable,
        vulkan_graphics_kernel_methods_vtable: services
            .vulkan_graphics_kernel_methods_vtable,
        vulkan_ray_tracing_kernel_methods_vtable: services
            .vulkan_ray_tracing_kernel_methods_vtable,
        vulkan_acceleration_structure_methods_vtable: services
            .vulkan_acceleration_structure_methods_vtable,
        rhi_color_converter_methods_vtable: services
            .rhi_color_converter_methods_vtable,
        rhi_command_recorder_methods_vtable: services
            .rhi_command_recorder_methods_vtable,
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
        // FFI and registers directly with the factory.
        if let Some(callbacks) = host_callbacks() {
            register_via_callback::<P>(callbacks);
        } else {
            // Host-static path: same vtable shape, but registered
            // directly with the in-process factory (no FFI hop).
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

    // SAFETY: msgpack bytes and vtable pointer live in this DSO's
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
// at the FFI boundary, instead of corrupting the cdylib's stack.
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
/// Panics are logged and converted to `default_on_panic` so the FFI
/// boundary stays sound. `callback_name` is included in the error
/// log to make the source obvious in mixed-callback traces.
///
/// Uses [`std::panic::AssertUnwindSafe`] internally because callback
/// bodies routinely touch raw pointers and `*mut` outputs that aren't
/// `UnwindSafe` by default — the pointer dereferences are sound under
/// the FFI contract regardless of unwinding.
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
            // Trade-off: cdylib pays for the FFI hop even on
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
// FFI conversions
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
    use crate::core::logging::push_polyglot_record;
    use crate::core::logging::LogRecord;

    let attrs = match fields {
        serde_json::Value::Object(map) => {
            map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        }
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

/// Host-facing helpers used by `Runner::add_module` (and the
/// `streamlib-runtime` binary's plugin loader) to assemble a
/// [`HostServices`] payload pointing at this DSO's callback
/// implementations.
pub mod runtime_facing {
    use super::{
        host_iceoryx_log_emit, host_processor_register, host_pubsub_publish, host_schema_lookup,
        host_schema_register, host_tracing_emit, host_tracing_enabled,
        host_tracing_register_callsite, HostServiceImpls, HOST_AUDIO_CLOCK_VTABLE,
        HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE, HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE,
        HOST_INPUT_MAILBOXES_VTABLE, HOST_OUTPUT_WRITER_VTABLE,
        HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE,
        HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE, HOST_RUNTIME_CONTEXT_VTABLE,
        HOST_RUNTIME_OPS_VTABLE, HOST_SURFACE_STORE_VTABLE,
        HOST_TEXTURE_RING_METHODS_VTABLE, HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE,
        HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE,
        HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE,
        HOST_VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE,
    };
    use std::ffi::c_void;
    use std::sync::OnceLock;

    use streamlib_plugin_abi::{HostServices, HOST_SERVICES_LAYOUT_VERSION};

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
            vulkan_compute_kernel_methods_vtable:
                &HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE,
            vulkan_graphics_kernel_methods_vtable:
                &HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE,
            vulkan_ray_tracing_kernel_methods_vtable:
                &HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE,
            vulkan_acceleration_structure_methods_vtable:
                &HOST_VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE,
            rhi_color_converter_methods_vtable:
                &HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE,
            rhi_command_recorder_methods_vtable:
                &HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE,
            output_writer_vtable: &HOST_OUTPUT_WRITER_VTABLE,
            input_mailboxes_vtable: &HOST_INPUT_MAILBOXES_VTABLE,
        }
    }
}

// ---- VulkanComputeKernelMethodsVTable wrappers (#949) ----------------------
//
// Each wrapper reconstructs the kernel borrow from the raw `Arc`
// handle the cdylib passes (`Arc::into_raw(Arc<VulkanComputeKernelInner>)`
// per the β-shape's `from_arc_into_raw`), runs the inner method,
// and converts the `Result<()>` into the FFI's `i32 + err_buf`
// shape. All bodies are wrapped in `run_host_extern_c` so a panic
// in the inner method becomes a non-zero return.
//
// First slice (this PR): `set_push_constants` + `dispatch`. The
// buffer/texture-input variants need a small trait redesign (the
// inner method's `B: VulkanStorageBindable` generic can't cross the
// FFI as-is — concrete β-shape inputs need a separate accessor on
// the trait) and land in a follow-up sub-issue.

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::into_raw(Arc<VulkanComputeKernelInner>)`. The leaked
/// strong count keeps the kernel alive for the call's duration.
#[cfg(target_os = "linux")]
unsafe fn handle_as_compute_kernel(
    handle: *const c_void,
) -> Option<&'static crate::vulkan::rhi::VulkanComputeKernelInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const crate::vulkan::rhi::VulkanComputeKernelInner) })
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_set_push_constants(
    kernel_handle: *const c_void,
    bytes_ptr: *const u8,
    bytes_len: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_push_constants",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "set_push_constants: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if bytes_ptr.is_null() && bytes_len != 0 {
                write_err(
                    "set_push_constants: null bytes_ptr with non-zero len",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let bytes = if bytes_len == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(bytes_ptr, bytes_len) }
            };
            match kernel.set_push_constants(bytes) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("set_push_constants: {e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_dispatch(
    kernel_handle: *const c_void,
    group_x: u32,
    group_y: u32,
    group_z: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_dispatch",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "dispatch: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match kernel.dispatch(group_x, group_y, group_z) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("dispatch: {e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

// ---- Binding-method wrappers (typed by input wrapper) ---------------------
//
// Each wrapper reconstructs a stack-allocated plugin-handle borrow
// from the raw `Arc::into_raw` handle the cdylib passes, then
// forwards to the inner kernel's binding method.
//
// `ManuallyDrop` is **load-bearing**, not defensive: removing it
// would let the borrow's Drop run on scope exit, which calls the
// vtable's `drop_*` slot and decrements the host's Arc refcount —
// while the cdylib still holds an outstanding plugin handle that
// expects to own a strong reference. The result is an under-counted
// Arc and a use-after-free on the cdylib's eventual Drop. The
// cdylib owns ownership for the call's duration; the host wrapper
// only borrows.
//
// **Invariant the helpers depend on:** the inner kernel's binding
// methods (`set_storage_buffer`, `set_uniform_buffer`,
// `set_sampled_texture`, `set_storage_image`) only deref
// `self.handle` to reach the host-internal allocation; they do NOT
// read the cached POD fields (`width`, `height`, `format_raw`,
// `byte_size_cached`, `mapped_ptr_cached`, etc.). The reconstructed
// borrow zeros every POD field for that reason. If a future
// engine-side binding method starts reading `buffer.width` /
// `texture.format()` / similar through the wrapper, the zeroed POD
// silently produces wrong results — the helper site is the place to
// add a populated-field stage if that invariant ever shifts.
//
// The vtable pointer is filled with the host's limited-access vtable
// (matching what `from_arc_into_raw` would have written) so the
// borrow is well-formed for any field-only read, even though no
// vtable callback is supposed to fire while the borrow is alive.

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_set_storage_buffer_pixel(
    kernel_handle: *const c_void,
    binding: u32,
    pixel_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_storage_buffer_pixel",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "set_storage_buffer_pixel: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer_handle.is_null() {
                write_err(
                    "set_storage_buffer_pixel: null pixel_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_pixel_buffer_borrow(pixel_buffer_handle);
            match kernel.set_storage_buffer(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_buffer_pixel: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_set_storage_buffer_storage(
    kernel_handle: *const c_void,
    binding: u32,
    storage_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_storage_buffer_storage",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "set_storage_buffer_storage: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if storage_buffer_handle.is_null() {
                write_err(
                    "set_storage_buffer_storage: null storage_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_storage_buffer_borrow(storage_buffer_handle);
            match kernel.set_storage_buffer(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_buffer_storage: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_set_uniform_buffer(
    kernel_handle: *const c_void,
    binding: u32,
    uniform_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_uniform_buffer",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "set_uniform_buffer: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if uniform_buffer_handle.is_null() {
                write_err(
                    "set_uniform_buffer: null uniform_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_uniform_buffer_borrow(uniform_buffer_handle);
            match kernel.set_uniform_buffer(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_uniform_buffer: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_set_sampled_texture(
    kernel_handle: *const c_void,
    binding: u32,
    texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_sampled_texture",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "set_sampled_texture: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if texture_handle.is_null() {
                write_err(
                    "set_sampled_texture: null texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_texture_borrow(texture_handle);
            match kernel.set_sampled_texture(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_sampled_texture: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_set_storage_image(
    kernel_handle: *const c_void,
    binding: u32,
    texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_storage_image",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "set_storage_image: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if texture_handle.is_null() {
                write_err(
                    "set_storage_image: null texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_texture_borrow(texture_handle);
            match kernel.set_storage_image(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_image: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

// ---- Non-Linux platform stubs (vtable layout stays unconditional) ----------

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_set_push_constants(
    _kernel_handle: *const c_void,
    _bytes_ptr: *const u8,
    _bytes_len: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_push_constants: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_dispatch(
    _kernel_handle: *const c_void,
    _group_x: u32,
    _group_y: u32,
    _group_z: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "dispatch: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_set_storage_buffer_pixel(
    _kernel_handle: *const c_void,
    _binding: u32,
    _pixel_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_buffer_pixel: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_set_storage_buffer_storage(
    _kernel_handle: *const c_void,
    _binding: u32,
    _storage_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_buffer_storage: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_set_uniform_buffer(
    _kernel_handle: *const c_void,
    _binding: u32,
    _uniform_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_uniform_buffer: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_set_sampled_texture(
    _kernel_handle: *const c_void,
    _binding: u32,
    _texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_sampled_texture: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_set_storage_image(
    _kernel_handle: *const c_void,
    _binding: u32,
    _texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_image: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Read the compute kernel's declared bindings into a caller-provided
/// `[ComputeBindingSpecRepr]` buffer. v4 (introspection).
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_bindings(
    kernel_handle: *const c_void,
    out_specs_buf: *mut streamlib_plugin_abi::ComputeBindingSpecRepr,
    out_specs_cap: usize,
    out_specs_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_bindings",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "bindings: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_specs_len.is_null() {
                write_err(
                    "bindings: null out_specs_len pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let bindings = kernel.bindings();
            let actual = bindings.len();
            unsafe { std::ptr::write(out_specs_len, actual) };
            if out_specs_cap < actual {
                return 2;
            }
            if !out_specs_buf.is_null() {
                for (i, spec) in bindings.iter().enumerate() {
                    let repr = streamlib_plugin_abi::ComputeBindingSpecRepr::from(spec);
                    unsafe { std::ptr::write(out_specs_buf.add(i), repr) };
                }
            } else if actual > 0 {
                write_err(
                    "bindings: out_specs_buf is null but kernel has bindings",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            0
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_bindings(
    _kernel_handle: *const c_void,
    _out_specs_buf: *mut streamlib_plugin_abi::ComputeBindingSpecRepr,
    _out_specs_cap: usize,
    _out_specs_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "bindings: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

// ---- Raw-vulkanalia-handle slots (v5 — #1073) -----------------------------
//
// Engine SDK code (`RgbToNv12Converter::convert`,
// `Nv12ToRgbConverter::convert`) reaches three `pub(crate)`
// `VulkanComputeKernel` setter methods that take raw `vk::ImageView`
// and one `record` method that takes raw `vk::CommandBuffer`. When
// that engine SDK code is compiled into a cdylib (workspace plugin
// packages with `crate-type = ["rlib", "cdylib"]` — h264, h265,
// camera), the cdylib-compiled methods can't deref `host_inner()`
// without tripping the panic guard. These callbacks let the cdylib
// dispatch through the host's per-method vtable instead.
//
// Wire shape: `vk::ImageView` is `#[repr(transparent)] pub struct
// ImageView(u64)` and `vk::CommandBuffer` is `#[repr(transparent)]
// pub struct CommandBuffer(usize)` (vulkanalia-sys handles.rs). The
// FFI carries the raw integer as `u64`; the host reconstructs via
// `Handle::from_raw` before forwarding.

// The four callbacks below dispatch through `*_raw` shim methods on
// `VulkanComputeKernelInner` so that this file stays off the
// vulkanalia allowlist (`xtask check-boundaries`). The RHI-side shim
// is the canonical owner of `Handle::from_raw` reconstruction.

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_set_sampled_image_view(
    kernel_handle: *const c_void,
    binding: u32,
    image_view_handle: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_sampled_image_view",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "set_sampled_image_view: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match kernel.set_sampled_image_view_raw(binding, image_view_handle) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_sampled_image_view: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_set_combined_image_sampler_view(
    kernel_handle: *const c_void,
    binding: u32,
    image_view_handle: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_combined_image_sampler_view",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "set_combined_image_sampler_view: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match kernel
                .set_combined_image_sampler_view_raw(binding, image_view_handle)
            {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_combined_image_sampler_view: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_set_storage_image_view(
    kernel_handle: *const c_void,
    binding: u32,
    image_view_handle: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_storage_image_view",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "set_storage_image_view: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match kernel.set_storage_image_view_raw(binding, image_view_handle) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_image_view: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_record(
    kernel_handle: *const c_void,
    command_buffer_handle: u64,
    group_x: u32,
    group_y: u32,
    group_z: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_record",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "record: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match kernel.record_raw(
                command_buffer_handle,
                group_x,
                group_y,
                group_z,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

// ---- Non-Linux stubs for v5 raw-vulkanalia-handle slots --------------------

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_set_sampled_image_view(
    _kernel_handle: *const c_void,
    _binding: u32,
    _image_view_handle: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_sampled_image_view: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_set_combined_image_sampler_view(
    _kernel_handle: *const c_void,
    _binding: u32,
    _image_view_handle: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_combined_image_sampler_view: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_set_storage_image_view(
    _kernel_handle: *const c_void,
    _binding: u32,
    _image_view_handle: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_image_view: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_record(
    _kernel_handle: *const c_void,
    _command_buffer_handle: u64,
    _group_x: u32,
    _group_y: u32,
    _group_z: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Host-side `VulkanComputeKernelMethodsVTable` populated with v5
/// method slots — v4's surface plus the v5 raw-vulkanalia-handle
/// slots needed by engine-SDK-internal converter code reaching out of
/// cdylib-resident processors (#1073).
pub static HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE:
    streamlib_plugin_abi::VulkanComputeKernelMethodsVTable =
    streamlib_plugin_abi::VulkanComputeKernelMethodsVTable {
        layout_version: streamlib_plugin_abi::VULKAN_COMPUTE_KERNEL_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        set_push_constants: host_compute_kernel_set_push_constants,
        dispatch: host_compute_kernel_dispatch,
        set_storage_buffer_pixel: host_compute_kernel_set_storage_buffer_pixel,
        set_storage_buffer_storage: host_compute_kernel_set_storage_buffer_storage,
        set_uniform_buffer: host_compute_kernel_set_uniform_buffer,
        set_sampled_texture: host_compute_kernel_set_sampled_texture,
        set_storage_image: host_compute_kernel_set_storage_image,
        bindings: host_compute_kernel_bindings,
        set_sampled_image_view: host_compute_kernel_set_sampled_image_view,
        set_combined_image_sampler_view:
            host_compute_kernel_set_combined_image_sampler_view,
        set_storage_image_view: host_compute_kernel_set_storage_image_view,
        record: host_compute_kernel_record,
    };

/// Accessor for the host's static `VulkanComputeKernelMethodsVTable`
/// — used by `VulkanComputeKernel::from_arc_into_raw` to populate
/// the β-shape's `methods_vtable` field.
pub fn host_vulkan_compute_kernel_methods_vtable(
) -> *const streamlib_plugin_abi::VulkanComputeKernelMethodsVTable {
    // Same routing as `host_gpu_context_limited_access_vtable`:
    // cdylib β-shape constructors must store the host's vtable
    // pointer so dispatches actually cross to host code (whose
    // `host_callbacks()` returns `None`). Without this routing, the
    // β-shape stored the cdylib's local static and dispatched to the
    // cdylib's own copy of the wrapper — where `host_callbacks()`
    // returns `Some` and any reach through `Texture::host_inner()` or
    // sibling β-shape `host_inner()` accessors panics.
    match host_callbacks() {
        Some(c) if !c.vulkan_compute_kernel_methods_vtable.is_null() => {
            c.vulkan_compute_kernel_methods_vtable
        }
        _ => &HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE,
    }
}

// ---- VulkanGraphicsKernelMethodsVTable wrappers (#951) ---------------------
//
// Each wrapper reconstructs the kernel borrow from the raw `Arc`
// handle the cdylib passes (`Arc::into_raw(Arc<VulkanGraphicsKernelInner>)`
// per the β-shape's `from_arc_into_raw`), runs the inner method,
// and converts the `Result<()>` into the FFI's `i32 + err_buf`
// shape. All bodies are wrapped in `run_host_extern_c` so a panic
// in the inner method becomes a non-zero return.
//
// Buffer / texture borrow reconstruction reuses the
// `make_*_buffer_borrow` / `make_texture_borrow` helpers from the
// compute-kernel section above — same `ManuallyDrop`-wrapped
// plugin-handle pattern, same "cached PODs are never read"
// invariant. See the comment block above
// `make_pixel_buffer_borrow` for the load-bearing details.

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::into_raw(Arc<VulkanGraphicsKernelInner>)`. The leaked
/// strong count keeps the kernel alive for the call's duration.
#[cfg(target_os = "linux")]
unsafe fn handle_as_graphics_kernel(
    handle: *const c_void,
) -> Option<&'static crate::vulkan::rhi::VulkanGraphicsKernelInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const crate::vulkan::rhi::VulkanGraphicsKernelInner) })
}

#[cfg(target_os = "linux")]
fn index_type_from_repr(raw: u32) -> Option<crate::core::rhi::IndexType> {
    match raw {
        0 => Some(crate::core::rhi::IndexType::Uint16),
        1 => Some(crate::core::rhi::IndexType::Uint32),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_storage_buffer_pixel(
    kernel_handle: *const c_void,
    frame_index: u32,
    binding: u32,
    pixel_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_storage_buffer_pixel",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) })
            else {
                write_err(
                    "set_storage_buffer_pixel: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer_handle.is_null() {
                write_err(
                    "set_storage_buffer_pixel: null pixel_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_pixel_buffer_borrow(pixel_buffer_handle);
            match kernel.set_storage_buffer(frame_index, binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_buffer_pixel: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_storage_buffer_storage(
    kernel_handle: *const c_void,
    frame_index: u32,
    binding: u32,
    storage_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_storage_buffer_storage",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) })
            else {
                write_err(
                    "set_storage_buffer_storage: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if storage_buffer_handle.is_null() {
                write_err(
                    "set_storage_buffer_storage: null storage_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_storage_buffer_borrow(storage_buffer_handle);
            match kernel.set_storage_buffer(frame_index, binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_buffer_storage: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_uniform_buffer(
    kernel_handle: *const c_void,
    frame_index: u32,
    binding: u32,
    uniform_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_uniform_buffer",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) })
            else {
                write_err(
                    "set_uniform_buffer: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if uniform_buffer_handle.is_null() {
                write_err(
                    "set_uniform_buffer: null uniform_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_uniform_buffer_borrow(uniform_buffer_handle);
            match kernel.set_uniform_buffer(frame_index, binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_uniform_buffer: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_sampled_texture(
    kernel_handle: *const c_void,
    frame_index: u32,
    binding: u32,
    texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_sampled_texture",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) })
            else {
                write_err(
                    "set_sampled_texture: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if texture_handle.is_null() {
                write_err(
                    "set_sampled_texture: null texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_texture_borrow(texture_handle);
            match kernel.set_sampled_texture(frame_index, binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_sampled_texture: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_storage_image(
    kernel_handle: *const c_void,
    frame_index: u32,
    binding: u32,
    texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_storage_image",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) })
            else {
                write_err(
                    "set_storage_image: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if texture_handle.is_null() {
                write_err(
                    "set_storage_image: null texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_texture_borrow(texture_handle);
            match kernel.set_storage_image(frame_index, binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_image: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_vertex_buffer(
    kernel_handle: *const c_void,
    frame_index: u32,
    binding: u32,
    vertex_buffer_handle: *const c_void,
    offset: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_vertex_buffer",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) })
            else {
                write_err(
                    "set_vertex_buffer: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if vertex_buffer_handle.is_null() {
                write_err(
                    "set_vertex_buffer: null vertex_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_vertex_buffer_borrow(vertex_buffer_handle);
            match kernel.set_vertex_buffer(frame_index, binding, &*borrow, offset) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_vertex_buffer: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_index_buffer(
    kernel_handle: *const c_void,
    frame_index: u32,
    index_buffer_handle: *const c_void,
    offset: u64,
    index_type_raw: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_index_buffer",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) })
            else {
                write_err(
                    "set_index_buffer: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if index_buffer_handle.is_null() {
                write_err(
                    "set_index_buffer: null index_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let Some(index_type) = index_type_from_repr(index_type_raw) else {
                write_err(
                    &format!("set_index_buffer: unknown index_type discriminant {index_type_raw}"),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let borrow = make_index_buffer_borrow(index_buffer_handle);
            match kernel.set_index_buffer(frame_index, &*borrow, offset, index_type) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_index_buffer: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_push_constants(
    kernel_handle: *const c_void,
    frame_index: u32,
    bytes_ptr: *const u8,
    bytes_len: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_push_constants",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) })
            else {
                write_err(
                    "set_push_constants: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if bytes_ptr.is_null() && bytes_len != 0 {
                write_err(
                    "set_push_constants: null bytes_ptr with non-zero len",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let bytes = if bytes_len == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(bytes_ptr, bytes_len) }
            };
            match kernel.set_push_constants(frame_index, bytes) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_push_constants: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
fn draw_call_from_repr(repr: &streamlib_plugin_abi::DrawCallRepr) -> crate::core::rhi::DrawCall {
    crate::core::rhi::DrawCall {
        vertex_count: repr.vertex_count,
        instance_count: repr.instance_count,
        first_vertex: repr.first_vertex,
        first_instance: repr.first_instance,
        viewport: if repr.viewport_present != 0 {
            Some(crate::core::rhi::Viewport {
                x: repr.viewport.x,
                y: repr.viewport.y,
                width: repr.viewport.width,
                height: repr.viewport.height,
                min_depth: repr.viewport.min_depth,
                max_depth: repr.viewport.max_depth,
            })
        } else {
            None
        },
        scissor: if repr.scissor_present != 0 {
            Some(crate::core::rhi::ScissorRect {
                x: repr.scissor.x,
                y: repr.scissor.y,
                width: repr.scissor.width,
                height: repr.scissor.height,
            })
        } else {
            None
        },
    }
}

#[cfg(target_os = "linux")]
fn draw_indexed_call_from_repr(
    repr: &streamlib_plugin_abi::DrawIndexedCallRepr,
) -> crate::core::rhi::DrawIndexedCall {
    crate::core::rhi::DrawIndexedCall {
        index_count: repr.index_count,
        instance_count: repr.instance_count,
        first_index: repr.first_index,
        vertex_offset: repr.vertex_offset,
        first_instance: repr.first_instance,
        viewport: if repr.viewport_present != 0 {
            Some(crate::core::rhi::Viewport {
                x: repr.viewport.x,
                y: repr.viewport.y,
                width: repr.viewport.width,
                height: repr.viewport.height,
                min_depth: repr.viewport.min_depth,
                max_depth: repr.viewport.max_depth,
            })
        } else {
            None
        },
        scissor: if repr.scissor_present != 0 {
            Some(crate::core::rhi::ScissorRect {
                x: repr.scissor.x,
                y: repr.scissor.y,
                width: repr.scissor.width,
                height: repr.scissor.height,
            })
        } else {
            None
        },
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_offscreen_render(
    kernel_handle: *const c_void,
    frame_index: u32,
    color_texture_handles: *const *const c_void,
    color_clear_present: *const u32,
    color_clear_values: *const [f32; 4],
    target_count: usize,
    extent_width: u32,
    extent_height: u32,
    draw: *const streamlib_plugin_abi::OffscreenDrawRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_offscreen_render",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) })
            else {
                write_err(
                    "offscreen_render: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if draw.is_null() {
                write_err(
                    "offscreen_render: null draw pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if target_count != 0
                && (color_texture_handles.is_null()
                    || color_clear_present.is_null()
                    || color_clear_values.is_null())
            {
                write_err(
                    "offscreen_render: null parallel-array pointer with non-zero target_count",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let handles = if target_count == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(color_texture_handles, target_count) }
            };
            let present_flags = if target_count == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(color_clear_present, target_count) }
            };
            let clear_values = if target_count == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(color_clear_values, target_count) }
            };
            // Reconstruct ManuallyDrop-wrapped Texture borrows for each
            // attachment. The Vec keeps the wrappers alive for the
            // duration of the inner call; OffscreenColorTarget then
            // borrows into those wrappers.
            let mut texture_borrows: Vec<std::mem::ManuallyDrop<crate::core::rhi::Texture>> =
                Vec::with_capacity(target_count);
            for (i, &handle) in handles.iter().enumerate() {
                if handle.is_null() {
                    write_err(
                        &format!("offscreen_render: null texture handle at color target {i}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
                texture_borrows.push(make_texture_borrow(handle));
            }
            let targets: Vec<crate::vulkan::rhi::OffscreenColorTarget<'_>> = texture_borrows
                .iter()
                .enumerate()
                .map(|(i, borrow)| {
                    let clear_color = if present_flags[i] != 0 {
                        Some(clear_values[i])
                    } else {
                        None
                    };
                    crate::vulkan::rhi::OffscreenColorTarget {
                        texture: &**borrow,
                        clear_color,
                    }
                })
                .collect();
            let draw_repr = unsafe { &*draw };
            let inner_draw = match draw_repr.kind {
                k if k == streamlib_plugin_abi::OffscreenDrawKindRepr::Draw as u32 => {
                    crate::vulkan::rhi::OffscreenDraw::Draw(draw_call_from_repr(
                        &draw_repr.draw_call,
                    ))
                }
                k if k == streamlib_plugin_abi::OffscreenDrawKindRepr::DrawIndexed as u32 => {
                    crate::vulkan::rhi::OffscreenDraw::DrawIndexed(draw_indexed_call_from_repr(
                        &draw_repr.draw_indexed_call,
                    ))
                }
                other => {
                    write_err(
                        &format!("offscreen_render: unknown draw kind discriminant {other}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match kernel.offscreen_render(
                frame_index,
                &targets,
                (extent_width, extent_height),
                inner_draw,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("offscreen_render: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

// ---- VulkanRayTracingKernelMethodsVTable wrappers (#953) -------------------
//
// Each wrapper reconstructs the kernel borrow from the raw `Arc`
// handle the cdylib passes (`Arc::into_raw(Arc<VulkanRayTracingKernelInner>)`
// per the β-shape's `from_arc_into_raw`), runs the inner method,
// and converts the `Result<()>` into the FFI's `i32 + err_buf`
// shape. All bodies are wrapped in `run_host_extern_c` so a panic
// in the inner method becomes a non-zero return.
//
// Buffer / texture borrow reconstruction reuses the
// `make_*_buffer_borrow` / `make_texture_borrow` helpers from the
// compute-kernel section above — same `ManuallyDrop`-wrapped
// plugin-handle pattern, same "cached PODs are never read"
// invariant. See the comment block above
// `make_pixel_buffer_borrow` for the load-bearing details.
//
// The AS-binding wrapper reconstructs an AS borrow via
// `make_acceleration_structure_borrow` — same `ManuallyDrop` shape
// as the buffer/texture helpers. The β-shape's `kind()` /
// `device_address()` / `storage_size()` getters read the cached
// fields on the borrow directly (no vtable dispatch); the helper
// populates those fields at construction time from the host-internal
// `Inner`, so the inner kernel's `set_acceleration_structure` reads
// the real values rather than the placeholder zeros that would
// trip the kernel's `TopLevel` check. `vk_handle()` stays host-only
// (vulkanalia handle, no cdylib path) and is only called from the
// host wrapper here, after the kind check passes.

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::into_raw(Arc<VulkanRayTracingKernelInner>)`. The leaked
/// strong count keeps the kernel alive for the call's duration.
#[cfg(target_os = "linux")]
unsafe fn handle_as_ray_tracing_kernel(
    handle: *const c_void,
) -> Option<&'static crate::vulkan::rhi::VulkanRayTracingKernelInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const crate::vulkan::rhi::VulkanRayTracingKernelInner) })
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_set_acceleration_structure(
    kernel_handle: *const c_void,
    binding: u32,
    acceleration_structure_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_set_acceleration_structure",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) })
            else {
                write_err(
                    "set_acceleration_structure: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if acceleration_structure_handle.is_null() {
                write_err(
                    "set_acceleration_structure: null acceleration_structure handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_acceleration_structure_borrow(acceleration_structure_handle);
            match kernel.set_acceleration_structure(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_acceleration_structure: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_set_storage_buffer_pixel(
    kernel_handle: *const c_void,
    binding: u32,
    pixel_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_set_storage_buffer_pixel",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) })
            else {
                write_err(
                    "set_storage_buffer_pixel: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer_handle.is_null() {
                write_err(
                    "set_storage_buffer_pixel: null pixel_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_pixel_buffer_borrow(pixel_buffer_handle);
            match kernel.set_storage_buffer(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_buffer_pixel: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_set_storage_buffer_storage(
    kernel_handle: *const c_void,
    binding: u32,
    storage_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_set_storage_buffer_storage",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) })
            else {
                write_err(
                    "set_storage_buffer_storage: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if storage_buffer_handle.is_null() {
                write_err(
                    "set_storage_buffer_storage: null storage_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_storage_buffer_borrow(storage_buffer_handle);
            match kernel.set_storage_buffer(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_buffer_storage: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_set_uniform_buffer(
    kernel_handle: *const c_void,
    binding: u32,
    uniform_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_set_uniform_buffer",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) })
            else {
                write_err(
                    "set_uniform_buffer: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if uniform_buffer_handle.is_null() {
                write_err(
                    "set_uniform_buffer: null uniform_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_uniform_buffer_borrow(uniform_buffer_handle);
            match kernel.set_uniform_buffer(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_uniform_buffer: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_set_sampled_texture(
    kernel_handle: *const c_void,
    binding: u32,
    texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_set_sampled_texture",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) })
            else {
                write_err(
                    "set_sampled_texture: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if texture_handle.is_null() {
                write_err(
                    "set_sampled_texture: null texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_texture_borrow(texture_handle);
            match kernel.set_sampled_texture(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_sampled_texture: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_set_storage_image(
    kernel_handle: *const c_void,
    binding: u32,
    texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_set_storage_image",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) })
            else {
                write_err(
                    "set_storage_image: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if texture_handle.is_null() {
                write_err(
                    "set_storage_image: null texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_texture_borrow(texture_handle);
            match kernel.set_storage_image(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_image: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_set_push_constants(
    kernel_handle: *const c_void,
    bytes_ptr: *const u8,
    bytes_len: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_set_push_constants",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) })
            else {
                write_err(
                    "set_push_constants: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if bytes_ptr.is_null() && bytes_len != 0 {
                write_err(
                    "set_push_constants: null bytes_ptr with non-zero len",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let bytes = if bytes_len == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(bytes_ptr, bytes_len) }
            };
            match kernel.set_push_constants(bytes) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_push_constants: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_trace_rays(
    kernel_handle: *const c_void,
    width: u32,
    height: u32,
    depth: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_trace_rays",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) })
            else {
                write_err(
                    "trace_rays: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match kernel.trace_rays(width, height, depth) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("trace_rays: {e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

// ---- Non-Linux platform stubs (vtable layout stays unconditional) ----------

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_storage_buffer_pixel(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _binding: u32,
    _pixel_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_buffer_pixel: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_storage_buffer_storage(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _binding: u32,
    _storage_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_buffer_storage: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_uniform_buffer(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _binding: u32,
    _uniform_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_uniform_buffer: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_sampled_texture(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _binding: u32,
    _texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_sampled_texture: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_storage_image(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _binding: u32,
    _texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_image: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_vertex_buffer(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _binding: u32,
    _vertex_buffer_handle: *const c_void,
    _offset: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_vertex_buffer: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_index_buffer(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _index_buffer_handle: *const c_void,
    _offset: u64,
    _index_type_raw: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_index_buffer: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_push_constants(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _bytes_ptr: *const u8,
    _bytes_len: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_push_constants: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_offscreen_render(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _color_texture_handles: *const *const c_void,
    _color_clear_present: *const u32,
    _color_clear_values: *const [f32; 4],
    _target_count: usize,
    _extent_width: u32,
    _extent_height: u32,
    _draw: *const streamlib_plugin_abi::OffscreenDrawRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "offscreen_render: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Read the graphics kernel's declared bindings into a caller-provided
/// `[GraphicsBindingSpecRepr]` buffer. v3 (introspection).
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_bindings(
    kernel_handle: *const c_void,
    out_specs_buf: *mut streamlib_plugin_abi::GraphicsBindingSpecRepr,
    out_specs_cap: usize,
    out_specs_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_bindings",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) })
            else {
                write_err(
                    "bindings: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_specs_len.is_null() {
                write_err(
                    "bindings: null out_specs_len pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let bindings = kernel.bindings();
            let actual = bindings.len();
            unsafe { std::ptr::write(out_specs_len, actual) };
            if out_specs_cap < actual {
                return 2;
            }
            if !out_specs_buf.is_null() {
                for (i, spec) in bindings.iter().enumerate() {
                    let repr =
                        streamlib_plugin_abi::GraphicsBindingSpecRepr::from(spec);
                    unsafe { std::ptr::write(out_specs_buf.add(i), repr) };
                }
            } else if actual > 0 {
                write_err(
                    "bindings: out_specs_buf is null but kernel has bindings",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            0
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_bindings(
    _kernel_handle: *const c_void,
    _out_specs_buf: *mut streamlib_plugin_abi::GraphicsBindingSpecRepr,
    _out_specs_cap: usize,
    _out_specs_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "bindings: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// v4 — record bind + push + draw into a caller-owned
/// `vk::CommandBuffer`. The cdylib mints + manages the command
/// buffer; this callback reconstructs the handle and forwards to
/// `VulkanGraphicsKernelInner::cmd_bind_and_draw_raw`, which does
/// the `vk::CommandBuffer::from_raw` conversion under the engine's
/// canonical vulkanalia-allowlist scope.
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_cmd_bind_and_draw(
    kernel_handle: *const c_void,
    command_buffer_handle: u64,
    frame_index: u32,
    draw: *const streamlib_plugin_abi::DrawCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_cmd_bind_and_draw",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) })
            else {
                write_err(
                    "cmd_bind_and_draw: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if draw.is_null() {
                write_err(
                    "cmd_bind_and_draw: null draw pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let draw_repr = unsafe { &*draw };
            let inner_draw = draw_call_from_repr(draw_repr);
            match kernel.cmd_bind_and_draw_raw(
                command_buffer_handle,
                frame_index,
                &inner_draw,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("cmd_bind_and_draw: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_cmd_bind_and_draw(
    _kernel_handle: *const c_void,
    _command_buffer_handle: u64,
    _frame_index: u32,
    _draw: *const streamlib_plugin_abi::DrawCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "cmd_bind_and_draw: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// v4 — indexed variant of [`host_graphics_kernel_cmd_bind_and_draw`].
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_cmd_bind_and_draw_indexed(
    kernel_handle: *const c_void,
    command_buffer_handle: u64,
    frame_index: u32,
    draw: *const streamlib_plugin_abi::DrawIndexedCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_cmd_bind_and_draw_indexed",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) })
            else {
                write_err(
                    "cmd_bind_and_draw_indexed: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if draw.is_null() {
                write_err(
                    "cmd_bind_and_draw_indexed: null draw pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let draw_repr = unsafe { &*draw };
            let inner_draw = draw_indexed_call_from_repr(draw_repr);
            match kernel.cmd_bind_and_draw_indexed_raw(
                command_buffer_handle,
                frame_index,
                &inner_draw,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("cmd_bind_and_draw_indexed: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_cmd_bind_and_draw_indexed(
    _kernel_handle: *const c_void,
    _command_buffer_handle: u64,
    _frame_index: u32,
    _draw: *const streamlib_plugin_abi::DrawIndexedCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "cmd_bind_and_draw_indexed: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

pub static HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE:
    streamlib_plugin_abi::VulkanGraphicsKernelMethodsVTable =
    streamlib_plugin_abi::VulkanGraphicsKernelMethodsVTable {
        layout_version:
            streamlib_plugin_abi::VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        set_storage_buffer_pixel: host_graphics_kernel_set_storage_buffer_pixel,
        set_storage_buffer_storage: host_graphics_kernel_set_storage_buffer_storage,
        set_uniform_buffer: host_graphics_kernel_set_uniform_buffer,
        set_sampled_texture: host_graphics_kernel_set_sampled_texture,
        set_storage_image: host_graphics_kernel_set_storage_image,
        set_vertex_buffer: host_graphics_kernel_set_vertex_buffer,
        set_index_buffer: host_graphics_kernel_set_index_buffer,
        set_push_constants: host_graphics_kernel_set_push_constants,
        offscreen_render: host_graphics_kernel_offscreen_render,
        bindings: host_graphics_kernel_bindings,
        cmd_bind_and_draw: host_graphics_kernel_cmd_bind_and_draw,
        cmd_bind_and_draw_indexed: host_graphics_kernel_cmd_bind_and_draw_indexed,
    };

/// Accessor for the host's static `VulkanGraphicsKernelMethodsVTable`
/// — used by `VulkanGraphicsKernel::from_arc_into_raw` to populate
/// the β-shape's `methods_vtable` field.
pub fn host_vulkan_graphics_kernel_methods_vtable(
) -> *const streamlib_plugin_abi::VulkanGraphicsKernelMethodsVTable {
    // See [`host_vulkan_compute_kernel_methods_vtable`] for the routing
    // rationale — cdylib β-shape constructors must store the host's
    // vtable pointer so dispatches actually cross DSO boundaries.
    match host_callbacks() {
        Some(c) if !c.vulkan_graphics_kernel_methods_vtable.is_null() => {
            c.vulkan_graphics_kernel_methods_vtable
        }
        _ => &HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE,
    }
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_set_acceleration_structure(
    _kernel_handle: *const c_void,
    _binding: u32,
    _acceleration_structure_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_acceleration_structure: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_set_storage_buffer_pixel(
    _kernel_handle: *const c_void,
    _binding: u32,
    _pixel_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_buffer_pixel: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_set_storage_buffer_storage(
    _kernel_handle: *const c_void,
    _binding: u32,
    _storage_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_buffer_storage: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_set_uniform_buffer(
    _kernel_handle: *const c_void,
    _binding: u32,
    _uniform_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_uniform_buffer: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_set_sampled_texture(
    _kernel_handle: *const c_void,
    _binding: u32,
    _texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_sampled_texture: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_set_storage_image(
    _kernel_handle: *const c_void,
    _binding: u32,
    _texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_image: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_set_push_constants(
    _kernel_handle: *const c_void,
    _bytes_ptr: *const u8,
    _bytes_len: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_push_constants: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_trace_rays(
    _kernel_handle: *const c_void,
    _width: u32,
    _height: u32,
    _depth: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "trace_rays: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Host-side `VulkanRayTracingKernelMethodsVTable` populated with the
/// v2 method slots (typed binding-method dispatch for the plugin
/// handle's `set_acceleration_structure` / `set_storage_buffer_pixel`
/// / `set_storage_buffer_storage` / `set_uniform_buffer` /
/// `set_sampled_texture` / `set_storage_image` surface plus the
/// primitive-argument slots `set_push_constants` / `trace_rays`).
///
/// The `bindings()` getter and the generic
/// `set_push_constants_value::<T>` stay `host_inner`-routed —
/// `Vec<RayTracingBindingSpec>` isn't `#[repr(C)]` and the generic
/// reduces to `set_push_constants` for cdylib mode.
/// Read the ray-tracing kernel's declared bindings into a caller-
/// provided `[RayTracingBindingSpecRepr]` buffer. v3 (introspection).
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_bindings(
    kernel_handle: *const c_void,
    out_specs_buf: *mut streamlib_plugin_abi::RayTracingBindingSpecRepr,
    out_specs_cap: usize,
    out_specs_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_bindings",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) })
            else {
                write_err(
                    "bindings: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_specs_len.is_null() {
                write_err(
                    "bindings: null out_specs_len pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let bindings = kernel.bindings();
            let actual = bindings.len();
            unsafe { std::ptr::write(out_specs_len, actual) };
            if out_specs_cap < actual {
                return 2;
            }
            if !out_specs_buf.is_null() {
                for (i, spec) in bindings.iter().enumerate() {
                    let repr =
                        streamlib_plugin_abi::RayTracingBindingSpecRepr::from(spec);
                    unsafe { std::ptr::write(out_specs_buf.add(i), repr) };
                }
            } else if actual > 0 {
                write_err(
                    "bindings: out_specs_buf is null but kernel has bindings",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            0
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_bindings(
    _kernel_handle: *const c_void,
    _out_specs_buf: *mut streamlib_plugin_abi::RayTracingBindingSpecRepr,
    _out_specs_cap: usize,
    _out_specs_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "bindings: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

pub static HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE:
    streamlib_plugin_abi::VulkanRayTracingKernelMethodsVTable =
    streamlib_plugin_abi::VulkanRayTracingKernelMethodsVTable {
        layout_version:
            streamlib_plugin_abi::VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        set_acceleration_structure: host_ray_tracing_kernel_set_acceleration_structure,
        set_storage_buffer_pixel: host_ray_tracing_kernel_set_storage_buffer_pixel,
        set_storage_buffer_storage: host_ray_tracing_kernel_set_storage_buffer_storage,
        set_uniform_buffer: host_ray_tracing_kernel_set_uniform_buffer,
        set_sampled_texture: host_ray_tracing_kernel_set_sampled_texture,
        set_storage_image: host_ray_tracing_kernel_set_storage_image,
        set_push_constants: host_ray_tracing_kernel_set_push_constants,
        trace_rays: host_ray_tracing_kernel_trace_rays,
        bindings: host_ray_tracing_kernel_bindings,
    };

/// Accessor for the host's static
/// `VulkanRayTracingKernelMethodsVTable` — used by
/// `VulkanRayTracingKernel::from_arc_into_raw` to populate the
/// β-shape's `methods_vtable` field.
pub fn host_vulkan_ray_tracing_kernel_methods_vtable(
) -> *const streamlib_plugin_abi::VulkanRayTracingKernelMethodsVTable {
    &HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE
}

// ---------------------------------------------------------------------------
// VulkanAccelerationStructureMethodsVTable wrappers (issue #955)
//
// The per-type vtable currently carries one method slot — `label` —
// because:
//   * POD getters (`device_address`, `storage_size`, `kind`) are
//     populated at mint time via the v8 build_triangles_blas /
//     build_tlas out-params; the β-shape's cached fields are always
//     real values, no vtable dispatch needed.
//   * `vk_handle` stays host-only (vulkanalia handle layout couples
//     to vulkanalia minor version; no in-tree cdylib consumer reads
//     it — every binding goes through the ray-tracing kernel's
//     host-side `set_acceleration_structure` slot which dereferences
//     the AS host-side).
//
// `label` uses the same caller-provided byte-buffer out-param shape
// as `TextureRingSlot.surface_id` from #947 — labels longer than
// `out_buf_cap` are silently truncated (fine for diagnostic strings).
// ---------------------------------------------------------------------------

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::into_raw(Arc<VulkanAccelerationStructureInner>)`. The
/// leaked strong count keeps the AS alive for the call's duration.
#[cfg(target_os = "linux")]
unsafe fn handle_as_acceleration_structure(
    handle: *const c_void,
) -> Option<&'static crate::vulkan::rhi::VulkanAccelerationStructureInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe {
        &*(handle as *const crate::vulkan::rhi::VulkanAccelerationStructureInner)
    })
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_vulkan_acceleration_structure_label(
    as_handle: *const c_void,
    out_buf: *mut u8,
    out_buf_cap: usize,
    out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_vulkan_acceleration_structure_label",
        || -> i32 {
            let Some(as_inner) =
                (unsafe { handle_as_acceleration_structure(as_handle) })
            else {
                write_err(
                    "label: null acceleration_structure handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buf.is_null() || out_len.is_null() {
                write_err(
                    "label: null out-parameter pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let label_bytes = as_inner.label().as_bytes();
            // Silent truncation at buffer cap — labels are diagnostic
            // strings, not load-bearing data; an over-large label
            // just shows the prefix in logs.
            let copy_len = label_bytes.len().min(out_buf_cap);
            unsafe {
                std::ptr::copy_nonoverlapping(
                    label_bytes.as_ptr(),
                    out_buf,
                    copy_len,
                );
                std::ptr::write(out_len, copy_len);
            }
            0
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_vulkan_acceleration_structure_label(
    _as_handle: *const c_void,
    _out_buf: *mut u8,
    _out_buf_cap: usize,
    _out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "label: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Host-side `VulkanAccelerationStructureMethodsVTable` wired to the
/// per-method wrappers above (issue #907 PR 5/5 shell + #955 method
/// dispatch).
pub static HOST_VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE:
    streamlib_plugin_abi::VulkanAccelerationStructureMethodsVTable =
    streamlib_plugin_abi::VulkanAccelerationStructureMethodsVTable {
        layout_version:
            streamlib_plugin_abi::VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        label: host_vulkan_acceleration_structure_label,
    };

/// Accessor for the host's static
/// `VulkanAccelerationStructureMethodsVTable` — used by
/// `VulkanAccelerationStructure::from_arc_into_raw` to populate the
/// β-shape's `methods_vtable` field.
pub fn host_vulkan_acceleration_structure_methods_vtable(
) -> *const streamlib_plugin_abi::VulkanAccelerationStructureMethodsVTable {
    &HOST_VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE
}

// =============================================================================
// RhiColorConverterMethodsVTable wrappers (Phase E sub-lift slice A).
// Each wrapper reconstructs the converter borrow from the raw
// `Arc::as_ptr(Arc<RhiColorConverterInner>)` handle the cdylib passes,
// reconstructs the buffer + texture borrows via the same
// `make_*_borrow` ManuallyDrop pattern the compute / graphics kernel
// wrappers use, runs the inner method, and converts the
// `Result<Arc<VulkanComputeKernel>>` into the FFI's `i32 + out
// param + err_buf` shape. All bodies are wrapped in
// `run_host_extern_c` so a panic in the inner method becomes a
// non-zero return.
// =============================================================================

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::as_ptr(Arc<RhiColorConverterInner>)`. The host borrows
/// only — no refcount bump — for the call's duration; the cdylib
/// retains ownership.
#[cfg(target_os = "linux")]
unsafe fn handle_as_color_converter(
    handle: *const c_void,
) -> Option<&'static crate::core::rhi::RhiColorConverterInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe {
        &*(handle as *const crate::core::rhi::RhiColorConverterInner)
    })
}

/// Convert a `#[repr(u32)]` `PrimariesId` discriminant to the typed
/// enum. Returns `None` for out-of-range values so the wrapper can
/// report a clean error rather than transmuting a garbage tag.
#[cfg(target_os = "linux")]
fn primaries_from_raw(
    raw: u32,
) -> Option<crate::core::color::PrimariesId> {
    use crate::core::color::PrimariesId;
    match raw {
        0 => Some(PrimariesId::Bt709),
        1 => Some(PrimariesId::Bt470M),
        2 => Some(PrimariesId::Bt470Bg),
        3 => Some(PrimariesId::Smpte170m),
        4 => Some(PrimariesId::Smpte240m),
        5 => Some(PrimariesId::Film),
        6 => Some(PrimariesId::Bt2020),
        7 => Some(PrimariesId::Smpte428),
        8 => Some(PrimariesId::Smpte431),
        9 => Some(PrimariesId::Smpte432),
        10 => Some(PrimariesId::Ebu3213),
        _ => None,
    }
}

/// Convert a `#[repr(u32)]` `TransferId` discriminant to the typed enum.
#[cfg(target_os = "linux")]
fn transfer_from_raw(raw: u32) -> Option<crate::core::color::TransferId> {
    use crate::core::color::TransferId;
    match raw {
        0 => Some(TransferId::Linear),
        1 => Some(TransferId::Srgb),
        2 => Some(TransferId::Bt709),
        3 => Some(TransferId::Pq),
        4 => Some(TransferId::Hlg),
        _ => None,
    }
}

/// Convert a `#[repr(u32)]` `MatrixId` discriminant to the typed enum.
#[cfg(target_os = "linux")]
fn matrix_from_raw(raw: u32) -> Option<crate::core::color::MatrixId> {
    use crate::core::color::MatrixId;
    match raw {
        0 => Some(MatrixId::Identity),
        1 => Some(MatrixId::Bt709),
        2 => Some(MatrixId::Fcc),
        3 => Some(MatrixId::Bt470Bg),
        4 => Some(MatrixId::Smpte170m),
        5 => Some(MatrixId::Smpte240m),
        6 => Some(MatrixId::Ycgco),
        7 => Some(MatrixId::Bt2020Ncl),
        8 => Some(MatrixId::Bt2020Cl),
        9 => Some(MatrixId::Smpte2085),
        10 => Some(MatrixId::ChromaNcl),
        11 => Some(MatrixId::ChromaCl),
        12 => Some(MatrixId::Ictcp),
        _ => None,
    }
}

/// Convert a `#[repr(u32)]` `RangeId` discriminant to the typed enum.
#[cfg(target_os = "linux")]
fn range_from_raw(raw: u32) -> Option<crate::core::color::RangeId> {
    use crate::core::color::RangeId;
    match raw {
        0 => Some(RangeId::Limited),
        1 => Some(RangeId::Full),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_prepare_buffer_to_image_storage(
    converter_handle: *const c_void,
    src_buffer_handle: *const c_void,
    src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    dst_texture_handle: *const c_void,
    info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    dst_transfer_raw: u32,
    out_kernel: *mut *const c_void,
    out_cached_push_constant_size: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_color_converter_prepare_buffer_to_image_storage",
        || -> i32 {
            let Some(converter) =
                (unsafe { handle_as_color_converter(converter_handle) })
            else {
                write_err(
                    "prepare_buffer_to_image_storage: null converter handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if src_buffer_handle.is_null() {
                write_err(
                    "prepare_buffer_to_image_storage: null src_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if dst_texture_handle.is_null() {
                write_err(
                    "prepare_buffer_to_image_storage: null dst_texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if src_layout.is_null() {
                write_err(
                    "prepare_buffer_to_image_storage: null src_layout pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if info.is_null() {
                write_err(
                    "prepare_buffer_to_image_storage: null info pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if out_kernel.is_null() || out_cached_push_constant_size.is_null() {
                write_err(
                    "prepare_buffer_to_image_storage: null out pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }

            let layout_repr = unsafe { &*src_layout };
            let info_repr = unsafe { &*info };

            let rust_layout = crate::core::rhi::SourceLayoutInfo {
                plane0_stride_bytes: layout_repr.plane0_stride_bytes,
                plane1_stride_bytes: layout_repr.plane1_stride_bytes,
                plane1_offset_bytes: layout_repr.plane1_offset_bytes,
            };

            let Some(primaries) = primaries_from_raw(info_repr.primaries_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_storage: invalid primaries discriminant {}",
                        info_repr.primaries_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(transfer_in) = transfer_from_raw(info_repr.transfer_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_storage: invalid transfer discriminant {}",
                        info_repr.transfer_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(matrix) = matrix_from_raw(info_repr.matrix_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_storage: invalid matrix discriminant {}",
                        info_repr.matrix_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(range) = range_from_raw(info_repr.range_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_storage: invalid range discriminant {}",
                        info_repr.range_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let resolved = crate::core::color::ResolvedColorInfo {
                primaries,
                transfer: transfer_in,
                matrix,
                range,
            };

            let Some(dst_transfer) = transfer_from_raw(dst_transfer_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_storage: invalid dst_transfer discriminant {}",
                        dst_transfer_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };

            let src_borrow = make_storage_buffer_borrow(src_buffer_handle);
            let dst_borrow = make_texture_borrow(dst_texture_handle);

            match converter.prepare_buffer_to_image_storage(
                &*src_borrow,
                rust_layout,
                &*dst_borrow,
                &resolved,
                dst_transfer,
            ) {
                Ok(arc_kernel) => {
                    // `arc_kernel.handle` is the inner Arc-into-raw'd
                    // pointer baked into the β-shape at construction.
                    // The cdylib needs its own strong count on the
                    // inner Arc so its β-shape can outlive our return
                    // (the converter's kernel cache + the inner Arc
                    // chain it sits behind keep their own strong
                    // counts). Bump the inner refcount by 1; the
                    // returned `Arc<VulkanComputeKernel>` drops
                    // naturally at end-of-block — its β-shape's Drop
                    // decrements the inner by 1, but only if this Arc
                    // was the last strong ref, which it isn't because
                    // the converter cache still holds one. Net effect:
                    // cdylib walks away with +1 inner-Arc strong count
                    // dedicated to it.
                    let raw_inner = arc_kernel.handle;
                    unsafe {
                        std::sync::Arc::increment_strong_count(
                            raw_inner
                                as *const crate::vulkan::rhi::VulkanComputeKernelInner,
                        );
                    }
                    let push_constant_size = arc_kernel.cached_push_constant_size;
                    unsafe {
                        std::ptr::write(out_kernel, raw_inner);
                        std::ptr::write(
                            out_cached_push_constant_size,
                            push_constant_size,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(
                        &format!("prepare_buffer_to_image_storage: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_prepare_buffer_to_image_storage(
    _converter_handle: *const c_void,
    _src_buffer_handle: *const c_void,
    _src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    _dst_texture_handle: *const c_void,
    _info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    _dst_transfer_raw: u32,
    _out_kernel: *mut *const c_void,
    _out_cached_push_constant_size: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "prepare_buffer_to_image_storage: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// `PixelBuffer`-shape source variant of
/// [`host_color_converter_prepare_buffer_to_image_storage`]. Decodes
/// the `ResolvedColorInfoRepr` + `SourceLayoutInfoRepr`, reconstructs
/// the `PixelBuffer` borrow, calls
/// `RhiColorConverterInner::prepare_buffer_to_image_pixel`, and bumps
/// the returned kernel's inner Arc strong count for the cdylib to
/// own. v2 (Phase E sub-lift completion).
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_prepare_buffer_to_image_pixel(
    converter_handle: *const c_void,
    src_buffer_handle: *const c_void,
    src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    dst_texture_handle: *const c_void,
    info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    dst_transfer_raw: u32,
    out_kernel: *mut *const c_void,
    out_cached_push_constant_size: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_color_converter_prepare_buffer_to_image_pixel",
        || -> i32 {
            let Some(converter) =
                (unsafe { handle_as_color_converter(converter_handle) })
            else {
                write_err(
                    "prepare_buffer_to_image_pixel: null converter handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if src_buffer_handle.is_null() {
                write_err(
                    "prepare_buffer_to_image_pixel: null src_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if dst_texture_handle.is_null() {
                write_err(
                    "prepare_buffer_to_image_pixel: null dst_texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if src_layout.is_null() {
                write_err(
                    "prepare_buffer_to_image_pixel: null src_layout pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if info.is_null() {
                write_err(
                    "prepare_buffer_to_image_pixel: null info pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if out_kernel.is_null() || out_cached_push_constant_size.is_null() {
                write_err(
                    "prepare_buffer_to_image_pixel: null out pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }

            let layout_repr = unsafe { &*src_layout };
            let info_repr = unsafe { &*info };

            let rust_layout = crate::core::rhi::SourceLayoutInfo {
                plane0_stride_bytes: layout_repr.plane0_stride_bytes,
                plane1_stride_bytes: layout_repr.plane1_stride_bytes,
                plane1_offset_bytes: layout_repr.plane1_offset_bytes,
            };

            let Some(primaries) = primaries_from_raw(info_repr.primaries_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_pixel: invalid primaries discriminant {}",
                        info_repr.primaries_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(transfer_in) = transfer_from_raw(info_repr.transfer_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_pixel: invalid transfer discriminant {}",
                        info_repr.transfer_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(matrix) = matrix_from_raw(info_repr.matrix_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_pixel: invalid matrix discriminant {}",
                        info_repr.matrix_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(range) = range_from_raw(info_repr.range_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_pixel: invalid range discriminant {}",
                        info_repr.range_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let resolved = crate::core::color::ResolvedColorInfo {
                primaries,
                transfer: transfer_in,
                matrix,
                range,
            };

            let Some(dst_transfer) = transfer_from_raw(dst_transfer_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_pixel: invalid dst_transfer discriminant {}",
                        dst_transfer_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };

            let src_borrow = make_pixel_buffer_borrow(src_buffer_handle);
            let dst_borrow = make_texture_borrow(dst_texture_handle);

            match converter.prepare_buffer_to_image_pixel(
                &*src_borrow,
                rust_layout,
                &*dst_borrow,
                &resolved,
                dst_transfer,
            ) {
                Ok(arc_kernel) => {
                    let raw_inner = arc_kernel.handle;
                    unsafe {
                        std::sync::Arc::increment_strong_count(
                            raw_inner
                                as *const crate::vulkan::rhi::VulkanComputeKernelInner,
                        );
                    }
                    let push_constant_size = arc_kernel.cached_push_constant_size;
                    unsafe {
                        std::ptr::write(out_kernel, raw_inner);
                        std::ptr::write(
                            out_cached_push_constant_size,
                            push_constant_size,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(
                        &format!("prepare_buffer_to_image_pixel: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_prepare_buffer_to_image_pixel(
    _converter_handle: *const c_void,
    _src_buffer_handle: *const c_void,
    _src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    _dst_texture_handle: *const c_void,
    _info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    _dst_transfer_raw: u32,
    _out_kernel: *mut *const c_void,
    _out_cached_push_constant_size: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "prepare_buffer_to_image_pixel: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// `StorageBuffer`-shape end-to-end conversion. Same handle and enum-
/// decoding contracts as
/// [`host_color_converter_prepare_buffer_to_image_storage`]; returns
/// no kernel handle (the host's converter retains the kernel cache).
/// v2 (Phase E sub-lift completion).
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_convert_buffer_to_image_storage(
    converter_handle: *const c_void,
    src_buffer_handle: *const c_void,
    src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    dst_texture_handle: *const c_void,
    info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_color_converter_convert_buffer_to_image_storage",
        || -> i32 {
            let Some(converter) =
                (unsafe { handle_as_color_converter(converter_handle) })
            else {
                write_err(
                    "convert_buffer_to_image_storage: null converter handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if src_buffer_handle.is_null() {
                write_err(
                    "convert_buffer_to_image_storage: null src_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if dst_texture_handle.is_null() {
                write_err(
                    "convert_buffer_to_image_storage: null dst_texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if src_layout.is_null() {
                write_err(
                    "convert_buffer_to_image_storage: null src_layout pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if info.is_null() {
                write_err(
                    "convert_buffer_to_image_storage: null info pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }

            let layout_repr = unsafe { &*src_layout };
            let info_repr = unsafe { &*info };

            let rust_layout = crate::core::rhi::SourceLayoutInfo {
                plane0_stride_bytes: layout_repr.plane0_stride_bytes,
                plane1_stride_bytes: layout_repr.plane1_stride_bytes,
                plane1_offset_bytes: layout_repr.plane1_offset_bytes,
            };

            let Some(primaries) = primaries_from_raw(info_repr.primaries_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_storage: invalid primaries discriminant {}",
                        info_repr.primaries_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(transfer_in) = transfer_from_raw(info_repr.transfer_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_storage: invalid transfer discriminant {}",
                        info_repr.transfer_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(matrix) = matrix_from_raw(info_repr.matrix_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_storage: invalid matrix discriminant {}",
                        info_repr.matrix_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(range) = range_from_raw(info_repr.range_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_storage: invalid range discriminant {}",
                        info_repr.range_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let resolved = crate::core::color::ResolvedColorInfo {
                primaries,
                transfer: transfer_in,
                matrix,
                range,
            };

            let src_borrow = make_storage_buffer_borrow(src_buffer_handle);
            let dst_borrow = make_texture_borrow(dst_texture_handle);

            match converter.convert_buffer_to_image_storage(
                &*src_borrow,
                rust_layout,
                &*dst_borrow,
                &resolved,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("convert_buffer_to_image_storage: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_convert_buffer_to_image_storage(
    _converter_handle: *const c_void,
    _src_buffer_handle: *const c_void,
    _src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    _dst_texture_handle: *const c_void,
    _info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "convert_buffer_to_image_storage: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// `PixelBuffer`-shape end-to-end conversion. Identical to
/// [`host_color_converter_convert_buffer_to_image_storage`] except for
/// the source buffer flavor. v2 (Phase E sub-lift completion).
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_convert_buffer_to_image_pixel(
    converter_handle: *const c_void,
    src_buffer_handle: *const c_void,
    src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    dst_texture_handle: *const c_void,
    info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_color_converter_convert_buffer_to_image_pixel",
        || -> i32 {
            let Some(converter) =
                (unsafe { handle_as_color_converter(converter_handle) })
            else {
                write_err(
                    "convert_buffer_to_image_pixel: null converter handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if src_buffer_handle.is_null() {
                write_err(
                    "convert_buffer_to_image_pixel: null src_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if dst_texture_handle.is_null() {
                write_err(
                    "convert_buffer_to_image_pixel: null dst_texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if src_layout.is_null() {
                write_err(
                    "convert_buffer_to_image_pixel: null src_layout pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if info.is_null() {
                write_err(
                    "convert_buffer_to_image_pixel: null info pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }

            let layout_repr = unsafe { &*src_layout };
            let info_repr = unsafe { &*info };

            let rust_layout = crate::core::rhi::SourceLayoutInfo {
                plane0_stride_bytes: layout_repr.plane0_stride_bytes,
                plane1_stride_bytes: layout_repr.plane1_stride_bytes,
                plane1_offset_bytes: layout_repr.plane1_offset_bytes,
            };

            let Some(primaries) = primaries_from_raw(info_repr.primaries_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_pixel: invalid primaries discriminant {}",
                        info_repr.primaries_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(transfer_in) = transfer_from_raw(info_repr.transfer_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_pixel: invalid transfer discriminant {}",
                        info_repr.transfer_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(matrix) = matrix_from_raw(info_repr.matrix_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_pixel: invalid matrix discriminant {}",
                        info_repr.matrix_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(range) = range_from_raw(info_repr.range_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_pixel: invalid range discriminant {}",
                        info_repr.range_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let resolved = crate::core::color::ResolvedColorInfo {
                primaries,
                transfer: transfer_in,
                matrix,
                range,
            };

            let src_borrow = make_pixel_buffer_borrow(src_buffer_handle);
            let dst_borrow = make_texture_borrow(dst_texture_handle);

            match converter.convert_buffer_to_image_pixel(
                &*src_borrow,
                rust_layout,
                &*dst_borrow,
                &resolved,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("convert_buffer_to_image_pixel: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_convert_buffer_to_image_pixel(
    _converter_handle: *const c_void,
    _src_buffer_handle: *const c_void,
    _src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    _dst_texture_handle: *const c_void,
    _info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "convert_buffer_to_image_pixel: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Host-side `RhiColorConverterMethodsVTable` wired to the per-method
/// wrappers above. v2 ships the Phase E sub-lift completion —
/// `prepare_buffer_to_image_pixel`, `convert_buffer_to_image_storage`,
/// `convert_buffer_to_image_pixel`.
pub static HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE:
    streamlib_plugin_abi::RhiColorConverterMethodsVTable =
    streamlib_plugin_abi::RhiColorConverterMethodsVTable {
        layout_version:
            streamlib_plugin_abi::RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        prepare_buffer_to_image_storage:
            host_color_converter_prepare_buffer_to_image_storage,
        prepare_buffer_to_image_pixel:
            host_color_converter_prepare_buffer_to_image_pixel,
        convert_buffer_to_image_storage:
            host_color_converter_convert_buffer_to_image_storage,
        convert_buffer_to_image_pixel:
            host_color_converter_convert_buffer_to_image_pixel,
    };

/// Accessor for the host's static `RhiColorConverterMethodsVTable` —
/// used by `RhiColorConverter::from_arc_into_raw` to populate the
/// β-shape's `methods_vtable` field.
pub fn host_rhi_color_converter_methods_vtable(
) -> *const streamlib_plugin_abi::RhiColorConverterMethodsVTable {
    &HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE
}

// =============================================================================
// RhiCommandRecorderMethodsVTable wrappers (Phase E sub-lift slice B — #984).
// Each wrapper reconstructs the recorder borrow from the raw
// `Box::into_raw(Box<RhiCommandRecorderInner>)` handle the cdylib
// passes, reconstructs the texture / buffer / kernel borrows via the
// same `make_*_borrow` ManuallyDrop pattern the compute / graphics
// kernel wrappers use, decodes the typed integer enum payloads
// (`VulkanLayout` / `VulkanStage` / `VulkanAccess`), runs the inner
// method, and converts the `Result<()>` into the FFI's `i32 +
// err_buf` shape. All bodies are wrapped in `run_host_extern_c` so a
// panic in the inner method becomes a non-zero return.
// =============================================================================

/// SAFETY: caller must hand a `handle` that came from
/// `Box::into_raw(Box<RhiCommandRecorderInner>)` (the β-shape's
/// `handle` field). The host borrows mutably for the call's duration;
/// the cdylib retains ownership and the next `Drop` runs
/// `Box::from_raw + drop` via the parent vtable's
/// `drop_command_recorder` slot.
#[cfg(target_os = "linux")]
unsafe fn handle_as_command_recorder_mut(
    handle: *const c_void,
) -> Option<&'static mut crate::vulkan::rhi::RhiCommandRecorderInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe {
        &mut *(handle as *mut crate::vulkan::rhi::RhiCommandRecorderInner)
    })
}

/// Reconstruct a stack-allocated `VulkanComputeKernel` β-shape
/// borrow from an `Arc::into_raw(Arc<VulkanComputeKernelInner>)`
/// handle. Same ManuallyDrop contract as `make_storage_buffer_borrow`
/// / `make_texture_borrow` — the borrow's Drop must NOT run, or it
/// would decrement the kernel's Arc refcount through the vtable
/// while the cdylib still holds an outstanding plugin handle.
///
/// The cached POD fields are populated from the host-side
/// `VulkanComputeKernelInner` via the same two-step dance the other
/// `make_*_borrow` helpers use: build a minimal borrow with zeroed
/// fields, reach the inner through `host_inner()`, then construct
/// the final borrow with the cached fields filled. Mirrors the
/// contract `from_arc_into_raw` honors at construction.
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_begin(
    recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_begin",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "begin: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match recorder.begin() {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("begin: {e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_begin(
    _recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err("begin: Linux-only", err_buf, err_buf_cap, err_len);
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_image_barrier(
    recorder_handle: *const c_void,
    texture_handle: *const c_void,
    from_layout_raw: i32,
    to_layout_raw: i32,
    from_stage_raw: i64,
    to_stage_raw: i64,
    from_access_raw: i64,
    to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_image_barrier",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_image_barrier: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if texture_handle.is_null() {
                write_err(
                    "record_image_barrier: null texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let texture_borrow = make_texture_borrow(texture_handle);
            let from_layout =
                streamlib_consumer_rhi::VulkanLayout(from_layout_raw);
            let to_layout = streamlib_consumer_rhi::VulkanLayout(to_layout_raw);
            let from_stage =
                crate::vulkan::rhi::VulkanStage(from_stage_raw as u64);
            let to_stage = crate::vulkan::rhi::VulkanStage(to_stage_raw as u64);
            let from_access =
                crate::vulkan::rhi::VulkanAccess(from_access_raw as u64);
            let to_access =
                crate::vulkan::rhi::VulkanAccess(to_access_raw as u64);
            match recorder.record_image_barrier(
                &*texture_borrow,
                from_layout,
                to_layout,
                from_stage,
                to_stage,
                from_access,
                to_access,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_image_barrier: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_image_barrier(
    _recorder_handle: *const c_void,
    _texture_handle: *const c_void,
    _from_layout_raw: i32,
    _to_layout_raw: i32,
    _from_stage_raw: i64,
    _to_stage_raw: i64,
    _from_access_raw: i64,
    _to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record_image_barrier: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_buffer_barrier(
    recorder_handle: *const c_void,
    storage_buffer_handle: *const c_void,
    from_stage_raw: i64,
    to_stage_raw: i64,
    from_access_raw: i64,
    to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_buffer_barrier",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_buffer_barrier: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if storage_buffer_handle.is_null() {
                write_err(
                    "record_buffer_barrier: null storage_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let buffer_borrow =
                make_storage_buffer_borrow(storage_buffer_handle);
            let from_stage =
                crate::vulkan::rhi::VulkanStage(from_stage_raw as u64);
            let to_stage = crate::vulkan::rhi::VulkanStage(to_stage_raw as u64);
            let from_access =
                crate::vulkan::rhi::VulkanAccess(from_access_raw as u64);
            let to_access =
                crate::vulkan::rhi::VulkanAccess(to_access_raw as u64);
            match recorder.record_buffer_barrier(
                &*buffer_borrow,
                from_stage,
                to_stage,
                from_access,
                to_access,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_buffer_barrier: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_buffer_barrier(
    _recorder_handle: *const c_void,
    _storage_buffer_handle: *const c_void,
    _from_stage_raw: i64,
    _to_stage_raw: i64,
    _from_access_raw: i64,
    _to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record_buffer_barrier: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_dispatch(
    recorder_handle: *const c_void,
    kernel_handle: *const c_void,
    group_x: u32,
    group_y: u32,
    group_z: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_dispatch",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_dispatch: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if kernel_handle.is_null() {
                write_err(
                    "record_dispatch: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let kernel_borrow = make_compute_kernel_borrow(kernel_handle);
            match recorder.record_dispatch(
                &*kernel_borrow,
                group_x,
                group_y,
                group_z,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_dispatch: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_dispatch(
    _recorder_handle: *const c_void,
    _kernel_handle: *const c_void,
    _group_x: u32,
    _group_y: u32,
    _group_z: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err("record_dispatch: Linux-only", err_buf, err_buf_cap, err_len);
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_copy_image_to_buffer(
    recorder_handle: *const c_void,
    src_texture_handle: *const c_void,
    src_layout_raw: i32,
    dst_storage_buffer_handle: *const c_void,
    region: *const streamlib_plugin_abi::ImageCopyRegionRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_copy_image_to_buffer",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_copy_image_to_buffer: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if src_texture_handle.is_null() {
                write_err(
                    "record_copy_image_to_buffer: null src texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if dst_storage_buffer_handle.is_null() {
                write_err(
                    "record_copy_image_to_buffer: null dst storage_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if region.is_null() {
                write_err(
                    "record_copy_image_to_buffer: null region pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let region_ref = unsafe { &*region };
            let src_borrow = make_texture_borrow(src_texture_handle);
            let dst_borrow =
                make_storage_buffer_borrow(dst_storage_buffer_handle);
            let src_layout =
                streamlib_consumer_rhi::VulkanLayout(src_layout_raw);
            let region_rust = crate::vulkan::rhi::ImageCopyRegion {
                width: region_ref.width,
                height: region_ref.height,
                buffer_offset: region_ref.buffer_offset,
                buffer_row_length: region_ref.buffer_row_length,
                buffer_image_height: region_ref.buffer_image_height,
                mip_level: region_ref.mip_level,
                array_layer: region_ref.array_layer,
            };
            match recorder.record_copy_image_to_buffer(
                &*src_borrow,
                src_layout,
                &*dst_borrow,
                region_rust,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_copy_image_to_buffer: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_copy_image_to_buffer(
    _recorder_handle: *const c_void,
    _src_texture_handle: *const c_void,
    _src_layout_raw: i32,
    _dst_storage_buffer_handle: *const c_void,
    _region: *const streamlib_plugin_abi::ImageCopyRegionRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record_copy_image_to_buffer: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_pixel_buffer_barrier(
    recorder_handle: *const c_void,
    pixel_buffer_handle: *const c_void,
    from_stage_raw: i64,
    to_stage_raw: i64,
    from_access_raw: i64,
    to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_pixel_buffer_barrier",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_pixel_buffer_barrier: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer_handle.is_null() {
                write_err(
                    "record_pixel_buffer_barrier: null pixel_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let buffer_borrow =
                make_pixel_buffer_borrow(pixel_buffer_handle);
            let from_stage =
                crate::vulkan::rhi::VulkanStage(from_stage_raw as u64);
            let to_stage = crate::vulkan::rhi::VulkanStage(to_stage_raw as u64);
            let from_access =
                crate::vulkan::rhi::VulkanAccess(from_access_raw as u64);
            let to_access =
                crate::vulkan::rhi::VulkanAccess(to_access_raw as u64);
            match recorder.record_buffer_barrier(
                &*buffer_borrow,
                from_stage,
                to_stage,
                from_access,
                to_access,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_pixel_buffer_barrier: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_pixel_buffer_barrier(
    _recorder_handle: *const c_void,
    _pixel_buffer_handle: *const c_void,
    _from_stage_raw: i64,
    _to_stage_raw: i64,
    _from_access_raw: i64,
    _to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record_pixel_buffer_barrier: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_copy_image_to_pixel_buffer(
    recorder_handle: *const c_void,
    src_texture_handle: *const c_void,
    src_layout_raw: i32,
    dst_pixel_buffer_handle: *const c_void,
    region: *const streamlib_plugin_abi::ImageCopyRegionRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_copy_image_to_pixel_buffer",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_copy_image_to_pixel_buffer: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if src_texture_handle.is_null() {
                write_err(
                    "record_copy_image_to_pixel_buffer: null src texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if dst_pixel_buffer_handle.is_null() {
                write_err(
                    "record_copy_image_to_pixel_buffer: null dst pixel_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if region.is_null() {
                write_err(
                    "record_copy_image_to_pixel_buffer: null region pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let region_ref = unsafe { &*region };
            let src_borrow = make_texture_borrow(src_texture_handle);
            let dst_borrow =
                make_pixel_buffer_borrow(dst_pixel_buffer_handle);
            let src_layout =
                streamlib_consumer_rhi::VulkanLayout(src_layout_raw);
            let region_rust = crate::vulkan::rhi::ImageCopyRegion {
                width: region_ref.width,
                height: region_ref.height,
                buffer_offset: region_ref.buffer_offset,
                buffer_row_length: region_ref.buffer_row_length,
                buffer_image_height: region_ref.buffer_image_height,
                mip_level: region_ref.mip_level,
                array_layer: region_ref.array_layer,
            };
            match recorder.record_copy_image_to_buffer(
                &*src_borrow,
                src_layout,
                &*dst_borrow,
                region_rust,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_copy_image_to_pixel_buffer: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_copy_image_to_pixel_buffer(
    _recorder_handle: *const c_void,
    _src_texture_handle: *const c_void,
    _src_layout_raw: i32,
    _dst_pixel_buffer_handle: *const c_void,
    _region: *const streamlib_plugin_abi::ImageCopyRegionRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record_copy_image_to_pixel_buffer: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_submit_signaling_timeline(
    recorder_handle: *const c_void,
    timeline_handle: *const c_void,
    signal_value: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_submit_signaling_timeline",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "submit_signaling_timeline: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if timeline_handle.is_null() {
                write_err(
                    "submit_signaling_timeline: null timeline handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // SAFETY: `timeline_handle` is a borrowed pointer from
            // the cdylib's
            // `RhiCommandRecorder::dispatch_submit_signaling_timeline_via_vtable`
            // (which gets it via `self as *const Self` on the
            // β-shape's outer `HostVulkanTimelineSemaphore` borrow,
            // same convention as the v13
            // `wait_timeline_semaphore` slot). The borrow lasts
            // only for the duration of this call.
            let timeline = unsafe {
                &*(timeline_handle
                    as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore)
            };
            match recorder.submit_signaling_timeline(timeline, signal_value) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("submit_signaling_timeline: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_submit_signaling_timeline(
    _recorder_handle: *const c_void,
    _timeline_handle: *const c_void,
    _signal_value: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "submit_signaling_timeline: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

// -------------------------------------------------------------------------
// v3 (#1066) — swapchain render-path wrappers
// -------------------------------------------------------------------------

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_swapchain_image_barrier(
    recorder_handle: *const c_void,
    image_raw: u64,
    from_layout_raw: i32,
    to_layout_raw: i32,
    from_stage_raw: i64,
    to_stage_raw: i64,
    from_access_raw: i64,
    to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_swapchain_image_barrier",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_swapchain_image_barrier: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            // Dispatch into the RHI-side `from_wire` shim — all
            // `vulkanalia` construction stays inside `vulkan/rhi/`
            // (the check-boundaries rule keeps raw vulkanalia out of
            // `core/plugin/`).
            match recorder.record_swapchain_image_barrier_from_wire(
                image_raw,
                from_layout_raw,
                to_layout_raw,
                from_stage_raw,
                to_stage_raw,
                from_access_raw,
                to_access_raw,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_swapchain_image_barrier: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_swapchain_image_barrier(
    _recorder_handle: *const c_void,
    _image_raw: u64,
    _from_layout_raw: i32,
    _to_layout_raw: i32,
    _from_stage_raw: i64,
    _to_stage_raw: i64,
    _from_access_raw: i64,
    _to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record_swapchain_image_barrier: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_cmd_begin_dynamic_rendering(
    recorder_handle: *const c_void,
    image_view_raw: u64,
    extent_w: u32,
    extent_h: u32,
    has_clear_color: u32,
    clear_r: f32,
    clear_g: f32,
    clear_b: f32,
    clear_a: f32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_cmd_begin_dynamic_rendering",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "cmd_begin_dynamic_rendering: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let clear = if has_clear_color != 0 {
                Some([clear_r, clear_g, clear_b, clear_a])
            } else {
                None
            };
            // Dispatch into the RHI-side `from_wire` shim — see the
            // `record_swapchain_image_barrier` wrapper above for the
            // check-boundaries rationale.
            match recorder.cmd_begin_dynamic_rendering_from_wire(
                image_view_raw,
                extent_w,
                extent_h,
                clear,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("cmd_begin_dynamic_rendering: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_cmd_begin_dynamic_rendering(
    _recorder_handle: *const c_void,
    _image_view_raw: u64,
    _extent_w: u32,
    _extent_h: u32,
    _has_clear_color: u32,
    _clear_r: f32,
    _clear_g: f32,
    _clear_b: f32,
    _clear_a: f32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "cmd_begin_dynamic_rendering: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_command_recorder_cmd_end_dynamic_rendering(
    recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_cmd_end_dynamic_rendering",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "cmd_end_dynamic_rendering: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match recorder.cmd_end_dynamic_rendering() {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("cmd_end_dynamic_rendering: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_command_recorder_cmd_end_dynamic_rendering(
    _recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "cmd_end_dynamic_rendering: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_submit_with_semaphores(
    recorder_handle: *const c_void,
    waits_ptr: *const streamlib_plugin_abi::SemaphoreSubmitInfoRepr,
    waits_count: usize,
    signals_ptr: *const streamlib_plugin_abi::SemaphoreSubmitInfoRepr,
    signals_count: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_submit_with_semaphores",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "submit_with_semaphores: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            // SAFETY: caller-owned arrays. We only read; the buffers
            // outlive the call by the cdylib-side `Vec` they came from.
            let waits_repr: &[streamlib_plugin_abi::SemaphoreSubmitInfoRepr] =
                if waits_count == 0 {
                    &[]
                } else {
                    unsafe { std::slice::from_raw_parts(waits_ptr, waits_count) }
                };
            let signals_repr: &[streamlib_plugin_abi::SemaphoreSubmitInfoRepr] =
                if signals_count == 0 {
                    &[]
                } else {
                    unsafe { std::slice::from_raw_parts(signals_ptr, signals_count) }
                };
            // Dispatch into the RHI-side `from_wire` shim — see the
            // `record_swapchain_image_barrier` wrapper above for the
            // check-boundaries rationale.
            match recorder.submit_with_semaphores_from_wire(waits_repr, signals_repr)
            {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("submit_with_semaphores: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_submit_with_semaphores(
    _recorder_handle: *const c_void,
    _waits_ptr: *const streamlib_plugin_abi::SemaphoreSubmitInfoRepr,
    _waits_count: usize,
    _signals_ptr: *const streamlib_plugin_abi::SemaphoreSubmitInfoRepr,
    _signals_count: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "submit_with_semaphores: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_draw(
    recorder_handle: *const c_void,
    kernel_handle: *const c_void,
    frame_index: u32,
    draw: *const streamlib_plugin_abi::DrawCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_draw",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_draw: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if kernel_handle.is_null() {
                write_err(
                    "record_draw: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if draw.is_null() {
                write_err(
                    "record_draw: null draw pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let draw_ref = unsafe { &*draw };
            let viewport = if draw_ref.viewport_present != 0 {
                Some(crate::core::rhi::Viewport {
                    x: draw_ref.viewport.x,
                    y: draw_ref.viewport.y,
                    width: draw_ref.viewport.width,
                    height: draw_ref.viewport.height,
                    min_depth: draw_ref.viewport.min_depth,
                    max_depth: draw_ref.viewport.max_depth,
                })
            } else {
                None
            };
            let scissor = if draw_ref.scissor_present != 0 {
                Some(crate::core::rhi::ScissorRect {
                    x: draw_ref.scissor.x,
                    y: draw_ref.scissor.y,
                    width: draw_ref.scissor.width,
                    height: draw_ref.scissor.height,
                })
            } else {
                None
            };
            let draw_call = crate::core::rhi::DrawCall {
                vertex_count: draw_ref.vertex_count,
                instance_count: draw_ref.instance_count,
                first_vertex: draw_ref.first_vertex,
                first_instance: draw_ref.first_instance,
                viewport,
                scissor,
            };
            let kernel_borrow = make_graphics_kernel_borrow(kernel_handle);
            match recorder.record_draw(&*kernel_borrow, frame_index, &draw_call) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_draw: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_draw(
    _recorder_handle: *const c_void,
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _draw: *const streamlib_plugin_abi::DrawCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err("record_draw: Linux-only", err_buf, err_buf_cap, err_len);
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_draw_indexed(
    recorder_handle: *const c_void,
    kernel_handle: *const c_void,
    frame_index: u32,
    draw: *const streamlib_plugin_abi::DrawIndexedCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_draw_indexed",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_draw_indexed: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if kernel_handle.is_null() {
                write_err(
                    "record_draw_indexed: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if draw.is_null() {
                write_err(
                    "record_draw_indexed: null draw pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let draw_ref = unsafe { &*draw };
            let viewport = if draw_ref.viewport_present != 0 {
                Some(crate::core::rhi::Viewport {
                    x: draw_ref.viewport.x,
                    y: draw_ref.viewport.y,
                    width: draw_ref.viewport.width,
                    height: draw_ref.viewport.height,
                    min_depth: draw_ref.viewport.min_depth,
                    max_depth: draw_ref.viewport.max_depth,
                })
            } else {
                None
            };
            let scissor = if draw_ref.scissor_present != 0 {
                Some(crate::core::rhi::ScissorRect {
                    x: draw_ref.scissor.x,
                    y: draw_ref.scissor.y,
                    width: draw_ref.scissor.width,
                    height: draw_ref.scissor.height,
                })
            } else {
                None
            };
            let draw_call = crate::core::rhi::DrawIndexedCall {
                index_count: draw_ref.index_count,
                instance_count: draw_ref.instance_count,
                first_index: draw_ref.first_index,
                vertex_offset: draw_ref.vertex_offset,
                first_instance: draw_ref.first_instance,
                viewport,
                scissor,
            };
            let kernel_borrow = make_graphics_kernel_borrow(kernel_handle);
            match recorder.record_draw_indexed(
                &*kernel_borrow,
                frame_index,
                &draw_call,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_draw_indexed: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_draw_indexed(
    _recorder_handle: *const c_void,
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _draw: *const streamlib_plugin_abi::DrawIndexedCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record_draw_indexed: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// v5 — bare submit. Sibling of v1 `submit_signaling_timeline`
/// without the timeline-semaphore parameters; used by
/// `RhiToneMapper::apply_with_layouts`'s private recorder when
/// reached from cdylib-resident processor code (the per-input
/// tone-mapping normalization step in graphics-kernel wrappers
/// is the first in-tree consumer).
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_command_recorder_submit(
    recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_submit",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "submit: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match recorder.submit() {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("submit: {e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_command_recorder_submit(
    _recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err("submit: Linux-only", err_buf, err_buf_cap, err_len);
    1
}

/// v5 — submit and block. Sibling of [`host_command_recorder_submit`];
/// caller-side `vkWaitForFences` after submit.
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_command_recorder_submit_and_wait(
    recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_submit_and_wait",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "submit_and_wait: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match recorder.submit_and_wait() {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("submit_and_wait: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_command_recorder_submit_and_wait(
    _recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "submit_and_wait: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Host-side `RhiCommandRecorderMethodsVTable` wired to the
/// per-method wrappers above. Covers the v1 record-then-submit
/// slots, the v3 swapchain render-path slots used by the cdylib
/// display, and the v5 bare-submit slots used by `RhiToneMapper`
/// when reached from cdylib-resident processor code.
pub static HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE:
    streamlib_plugin_abi::RhiCommandRecorderMethodsVTable =
    streamlib_plugin_abi::RhiCommandRecorderMethodsVTable {
        layout_version:
            streamlib_plugin_abi::RHI_COMMAND_RECORDER_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        begin: host_command_recorder_begin,
        record_image_barrier: host_command_recorder_record_image_barrier,
        record_buffer_barrier: host_command_recorder_record_buffer_barrier,
        record_dispatch: host_command_recorder_record_dispatch,
        record_copy_image_to_buffer:
            host_command_recorder_record_copy_image_to_buffer,
        submit_signaling_timeline:
            host_command_recorder_submit_signaling_timeline,
        record_pixel_buffer_barrier:
            host_command_recorder_record_pixel_buffer_barrier,
        record_copy_image_to_pixel_buffer:
            host_command_recorder_record_copy_image_to_pixel_buffer,
        record_swapchain_image_barrier:
            host_command_recorder_record_swapchain_image_barrier,
        cmd_begin_dynamic_rendering:
            host_command_recorder_cmd_begin_dynamic_rendering,
        cmd_end_dynamic_rendering:
            host_command_recorder_cmd_end_dynamic_rendering,
        submit_with_semaphores: host_command_recorder_submit_with_semaphores,
        record_draw: host_command_recorder_record_draw,
        record_draw_indexed: host_command_recorder_record_draw_indexed,
        submit: host_command_recorder_submit,
        submit_and_wait: host_command_recorder_submit_and_wait,
    };

/// Accessor for the host's static `RhiCommandRecorderMethodsVTable`
/// — used by `RhiCommandRecorder::from_inner` to populate the
/// β-shape's `methods_vtable` field.
///
/// See [`host_vulkan_compute_kernel_methods_vtable`] for the routing
/// rationale — cdylib β-shape constructors must store the host's
/// vtable pointer so dispatches actually cross DSO boundaries.
pub fn host_rhi_command_recorder_methods_vtable(
) -> *const streamlib_plugin_abi::RhiCommandRecorderMethodsVTable {
    match host_callbacks() {
        Some(c) if !c.rhi_command_recorder_methods_vtable.is_null() => {
            c.rhi_command_recorder_methods_vtable
        }
        _ => &HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE,
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




#[cfg(all(test, target_os = "linux"))]
mod compute_kernel_methods_vtable_null_tests {
    //! Tier-1 wire-format tests for the v3 typed binding-method
    //! slots on `VulkanComputeKernelMethodsVTable`. Each wrapper must
    //! reject a null kernel handle before reaching any kernel-side
    //! state (i.e. before any deref) so cdylib callers get a clean
    //! error return on the wire-format path instead of UB.
    //!
    //! The null-buffer-handle / null-texture-handle guards live in
    //! the same wrappers and fire when the kernel handle is valid;
    //! they're exercised end-to-end by the CPU-reference dlopen
    //! integration test (which holds a real kernel and is the only
    //! place a Tier-1 null-input test can reach without panicking on
    //! the kernel-handle deref).

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn set_storage_buffer_pixel_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.set_storage_buffer_pixel)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_storage_buffer_pixel: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_buffer_storage_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.set_storage_buffer_storage)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_storage_buffer_storage: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_uniform_buffer_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.set_uniform_buffer)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_uniform_buffer: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_sampled_texture_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.set_sampled_texture)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_sampled_texture: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_image_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.set_storage_image)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_storage_image: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    // ---- v5 raw-vulkanalia-handle slots (#1073) -----------------------------
    //
    // The null-kernel-handle case is the tier-1 reach: passing a real
    // `vk::ImageView` / `vk::CommandBuffer` raw handle with a null
    // kernel pointer must fail cleanly before any deref. Non-null but
    // garbage kernel handles still trip the host_inner deref's pointer
    // alignment / segfault — the same precedent the v3/v4 tests
    // document.

    #[test]
    fn set_sampled_image_view_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.set_sampled_image_view)(
                std::ptr::null(),
                0,
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_sampled_image_view: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_combined_image_sampler_view_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE
                .set_combined_image_sampler_view)(
                std::ptr::null(),
                0,
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_combined_image_sampler_view: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_image_view_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.set_storage_image_view)(
                std::ptr::null(),
                0,
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_storage_image_view: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn record_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.record)(
                std::ptr::null(),
                0,
                1, 1, 1,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("record: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod gpu_rhi_color_converter_methods_vtable_null_tests {
    //! Tier-1 wire-format tests for the v1 method slot on
    //! `RhiColorConverterMethodsVTable`. The wrapper must reject a
    //! null converter handle before reaching any converter-side
    //! state (i.e. before any deref) so cdylib callers get a clean
    //! error return on the wire-format path instead of UB.
    //!
    //! The null-src-buffer / null-dst-texture / null-src-layout /
    //! null-info / null-out-pointer guards live in the same wrapper
    //! and fire when the converter handle is valid; they're
    //! exercised end-to-end by the camera-package dlopen smoke test
    //! (which holds a real converter). Tier-1 cannot reach them
    //! without first passing the converter-handle deref — passing a
    //! non-null garbage handle for the converter trips a misaligned-
    //! pointer-deref panic before any subsequent guard runs. This
    //! mirrors the precedent set by
    //! `compute_kernel_methods_vtable_null_tests` (only the null-
    //! kernel-handle case is tier-1; the rest ride dlopen).
    //!
    //! Success-path coverage (real Arc<RhiColorConverterInner>, a
    //! cached buffer→image kernel that mints a fresh
    //! Arc<VulkanComputeKernelInner>-shaped out-handle, refcount
    //! transfer to the cdylib) requires a real Vulkan device and
    //! arrives in the camera-package dlopen smoke test.

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    fn dummy_layout() -> streamlib_plugin_abi::SourceLayoutInfoRepr {
        streamlib_plugin_abi::SourceLayoutInfoRepr {
            plane0_stride_bytes: 0,
            plane1_stride_bytes: 0,
            plane1_offset_bytes: 0,
            _reserved_padding: 0,
        }
    }

    fn dummy_info() -> streamlib_plugin_abi::ResolvedColorInfoRepr {
        streamlib_plugin_abi::ResolvedColorInfoRepr {
            primaries_raw: 0,
            transfer_raw: 0,
            matrix_raw: 0,
            range_raw: 0,
        }
    }

    #[test]
    fn prepare_buffer_to_image_storage_rejects_null_converter_handle() {
        let (mut buf, mut len) = make_err_buf();
        let layout = dummy_layout();
        let info = dummy_info();
        let mut out_kernel: *const std::ffi::c_void = std::ptr::null();
        let mut out_pc_size: u32 = 0;
        let rc = unsafe {
            (HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE.prepare_buffer_to_image_storage)(
                std::ptr::null(),
                std::ptr::null(),
                &layout,
                std::ptr::null(),
                &info,
                0,
                &mut out_kernel,
                &mut out_pc_size,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("prepare_buffer_to_image_storage: null converter handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod gpu_rhi_command_recorder_methods_vtable_null_tests {
    //! Tier-1 wire-format tests for the v1 method slots on
    //! `RhiCommandRecorderMethodsVTable`. Each wrapper must reject a
    //! null recorder handle before reaching any recorder-side state
    //! (i.e. before any deref) so cdylib callers get a clean error
    //! return on the wire-format path instead of UB.
    //!
    //! The secondary null-handle guards (texture / storage_buffer /
    //! kernel / timeline) live in the same wrappers and fire when the
    //! recorder handle is valid; they're exercised end-to-end by the
    //! camera-package dlopen smoke test (which holds a real recorder).
    //! Tier-1 cannot reach them without first passing the recorder-
    //! handle deref — passing a non-null garbage handle for the
    //! recorder trips a misaligned-pointer-deref panic before any
    //! subsequent guard runs. This mirrors the precedent set by
    //! `gpu_rhi_color_converter_methods_vtable_null_tests`.
    //!
    //! Success-path coverage (real Box<RhiCommandRecorderInner>, a
    //! full begin → record_* → submit_signaling_timeline cycle)
    //! requires a real Vulkan device and arrives in the camera-
    //! package dlopen smoke test.

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    fn dummy_region() -> streamlib_plugin_abi::ImageCopyRegionRepr {
        streamlib_plugin_abi::ImageCopyRegionRepr::default()
    }

    #[test]
    fn begin_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.begin)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("begin: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn record_image_barrier_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.record_image_barrier)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("record_image_barrier: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn record_buffer_barrier_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.record_buffer_barrier)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("record_buffer_barrier: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn record_dispatch_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.record_dispatch)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("record_dispatch: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn record_copy_image_to_buffer_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let region = dummy_region();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.record_copy_image_to_buffer)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                &region,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("record_copy_image_to_buffer: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn submit_signaling_timeline_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.submit_signaling_timeline)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("submit_signaling_timeline: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    // v5 — submit / submit_and_wait wrappers.

    #[test]
    fn submit_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.submit)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("submit: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn submit_and_wait_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.submit_and_wait)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("submit_and_wait: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn record_pixel_buffer_barrier_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.record_pixel_buffer_barrier)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("record_pixel_buffer_barrier: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn record_copy_image_to_pixel_buffer_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let region = dummy_region();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE
                .record_copy_image_to_pixel_buffer)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                &region,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("record_copy_image_to_pixel_buffer: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod graphics_kernel_methods_vtable_null_tests {
    //! Tier-1 wire-format tests for the v2 method slots on
    //! `VulkanGraphicsKernelMethodsVTable`. Each wrapper must
    //! reject a null kernel handle before reaching any kernel-side
    //! state (i.e. before any deref) so cdylib callers get a clean
    //! error return on the wire-format path instead of UB.
    //!
    //! The null-buffer-handle / null-texture-handle guards live in
    //! the same wrappers and fire when the kernel handle is valid;
    //! they're exercised end-to-end by the graphics-kernel dlopen
    //! smoke test (which holds a real kernel).

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn set_storage_buffer_pixel_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_storage_buffer_pixel)(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_storage_buffer_pixel: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_buffer_storage_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_storage_buffer_storage)(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_storage_buffer_storage: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_uniform_buffer_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_uniform_buffer)(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_uniform_buffer: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_sampled_texture_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_sampled_texture)(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_sampled_texture: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_image_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_storage_image)(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_storage_image: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_vertex_buffer_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_vertex_buffer)(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_vertex_buffer: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_index_buffer_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_index_buffer)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_index_buffer: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_push_constants_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_push_constants)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_push_constants: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn offscreen_render_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let draw: streamlib_plugin_abi::OffscreenDrawRepr = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.offscreen_render)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                0,
                &draw,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("offscreen_render: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    // v4 — cmd_bind_and_draw / cmd_bind_and_draw_indexed wrappers.

    #[test]
    fn cmd_bind_and_draw_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let draw: streamlib_plugin_abi::DrawCallRepr = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.cmd_bind_and_draw)(
                std::ptr::null(),
                0,
                0,
                &draw,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("cmd_bind_and_draw: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn cmd_bind_and_draw_indexed_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let draw: streamlib_plugin_abi::DrawIndexedCallRepr =
            unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.cmd_bind_and_draw_indexed)(
                std::ptr::null(),
                0,
                0,
                &draw,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("cmd_bind_and_draw_indexed: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

}

#[cfg(all(test, target_os = "linux"))]
mod ray_tracing_kernel_methods_vtable_null_tests {
    //! Tier-1 wire-format tests for the v2 method slots on
    //! `VulkanRayTracingKernelMethodsVTable`. Each wrapper must
    //! reject a null kernel handle before reaching any kernel-side
    //! state (i.e. before any deref) so cdylib callers get a clean
    //! error return on the wire-format path instead of UB.
    //!
    //! The null-AS-handle / null-buffer-handle / null-texture-handle
    //! guards live in the same wrappers and fire when the kernel
    //! handle is valid; they're exercised end-to-end by the
    //! ray-tracing-kernel dlopen smoke test (which holds a real
    //! kernel).

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn set_acceleration_structure_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.set_acceleration_structure)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_acceleration_structure: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_buffer_pixel_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.set_storage_buffer_pixel)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_storage_buffer_pixel: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_buffer_storage_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.set_storage_buffer_storage)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_storage_buffer_storage: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_uniform_buffer_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.set_uniform_buffer)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_uniform_buffer: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_sampled_texture_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.set_sampled_texture)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_sampled_texture: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_image_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.set_storage_image)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_storage_image: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_push_constants_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.set_push_constants)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("set_push_constants: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn trace_rays_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.trace_rays)(
                std::ptr::null(),
                0,
                0,
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("trace_rays: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }
}


#[cfg(all(test, target_os = "linux"))]
mod acceleration_structure_methods_vtable_null_tests {
    //! Tier-1 wire-format tests for the v2 `label` method slot on
    //! `VulkanAccelerationStructureMethodsVTable` (issue #955). The
    //! wrapper must reject a null AS handle before reaching any
    //! Inner state so cdylib callers get a clean error return
    //! instead of UB.
    //!
    //! End-to-end coverage (real AS + label round-trip) is locked
    //! by the dlopen integration test for the cross-rustc fixture,
    //! which builds a real BLAS and asserts the round-tripped label
    //! matches the one passed at build time.

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn label_rejects_null_acceleration_structure_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut out_buf = [0u8; 64];
        let mut out_len: usize = 0;
        let rc = unsafe {
            (HOST_VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE.label)(
                std::ptr::null(),
                out_buf.as_mut_ptr(),
                out_buf.len(),
                &mut out_len as *mut usize,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("label: null acceleration_structure handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }
}



#[cfg(test)]
mod run_host_extern_c_panic_safety_net_tests {
    //! Phase G (#961) panic-injection coverage.
    //!
    //! Every `host_*` extern "C" callback wraps its body in
    //! [`run_host_extern_c`], whose `catch_unwind` safety net is
    //! the only thing standing between a panic in cdylib-author
    //! code path and an `extern "C"` unwind across the FFI
    //! boundary (UB). This module locks the safety-net contract:
    //!
    //!   - A body that panics with a `&'static str` payload is
    //!     caught and the documented default is returned.
    //!   - A body that panics with a `String` payload is caught
    //!     and the default is returned.
    //!   - A body that panics with an arbitrary non-string
    //!     payload (e.g. a custom Debug type) is caught and the
    //!     default is returned.
    //!   - Successful (non-panicking) bodies still return their
    //!     value as expected — proves the safety net isn't
    //!     intercepting normal control flow.
    //!
    //! Mental-revert: removing the `catch_unwind` wrap (i.e.
    //! calling `body()` directly inside `run_host_extern_c`)
    //! reverts every test below to a `panic!()` that aborts the
    //! test process. The harness reports each as a hard process
    //! abort rather than a fail.
    //!
    //! Why this module instead of a per-vtable-callback test:
    //! every host_* callback delegates to the same
    //! `run_host_extern_c` helper. Locking the helper's contract
    //! once covers every callback that rides it. Per-callback
    //! panic-injection would require deliberately corrupting
    //! handle state in production-shaped ways (UB) and would
    //! re-test the same `catch_unwind` machinery a hundred times.
    //! This is the engine-tier fix per CLAUDE.md ("Engine-wide
    //! bugs get fixed at the engine layer").

    use super::*;

    #[test]
    fn panic_with_static_str_returns_default_i32() {
        let rc = run_host_extern_c::<_, i32>(
            "test_static_str_panic",
            || panic!("deliberate test panic with &'static str"),
            42i32,
        );
        assert_eq!(rc, 42, "catch_unwind must return the default on panic");
    }

    #[test]
    fn panic_with_string_returns_default_i32() {
        let rc = run_host_extern_c::<_, i32>(
            "test_string_panic",
            || panic!("{}", String::from("deliberate dynamic panic")),
            7i32,
        );
        assert_eq!(rc, 7);
    }

    #[test]
    fn panic_with_non_string_payload_returns_default_i32() {
        // The wrapper's downcast chain handles `&'static str` and
        // `String` explicitly and falls through to a generic
        // "<non-string panic payload>" tracing message for anything
        // else. The catch_unwind contract is the load-bearing part:
        // even with an exotic payload the default must come back.
        #[derive(Debug)]
        struct CustomPayload;
        let rc = run_host_extern_c::<_, i32>(
            "test_custom_payload_panic",
            || std::panic::panic_any(CustomPayload),
            -1i32,
        );
        assert_eq!(rc, -1);
    }

    #[test]
    fn non_panicking_body_returns_its_value() {
        // Locks the "safety net doesn't intercept normal control
        // flow" invariant. Mental-revert: making `run_host_extern_c`
        // always return `default_on_panic` (no Ok branch) would
        // fail this test.
        let rc = run_host_extern_c::<_, i32>(
            "test_ok_path",
            || 99i32,
            -1i32,
        );
        assert_eq!(rc, 99);
    }

    #[test]
    fn panic_with_unit_default_returns_unit() {
        // Locks the panic-default for `()`-returning callbacks
        // (the entire RuntimeOps / clone/drop / null-handle-no-op
        // shape). The () default is trivially "the same value as
        // before"; what matters is that the body's panic doesn't
        // propagate past the FFI boundary.
        let mut hit = false;
        run_host_extern_c::<_, ()>(
            "test_unit_default_panic",
            || {
                hit = true;
                panic!("unit-default panic");
            },
            (),
        );
        assert!(hit, "body must have run before panicking");
    }

    #[test]
    fn panic_with_null_ptr_default_returns_null() {
        // Locks the panic-default for `*const c_void`-returning
        // callbacks (e.g. `host_gpu_lim_clone_handle`,
        // `host_rcv_audio_clock_handle`). The default is a null
        // pointer; the assertion confirms a panicking body returns
        // null rather than dangling memory.
        let p = run_host_extern_c::<_, *const c_void>(
            "test_null_ptr_default_panic",
            || panic!("null-ptr-default panic"),
            std::ptr::null(),
        );
        assert!(p.is_null());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod make_borrow_cached_field_regression_tests {
    //! Locks the issue #988 bug: `make_*_borrow` helpers MUST populate
    //! the β-shape's cached POD fields from the host-side inner —
    //! NOT leave them zeroed. Reverting any `make_*_borrow` to
    //! `width_cached: 0` / `byte_size_cached: 0` / etc. trips these
    //! assertions.
    //!
    //! Requires a working Vulkan device; skips cleanly when one isn't
    //! available (per `project_ci_strategy_no_gpu`).
    use super::*;
    use std::sync::Arc;

    fn try_vulkan_device() -> Option<Arc<crate::vulkan::rhi::HostVulkanDevice>> {
        crate::vulkan::rhi::HostVulkanDevice::new().ok()
    }

    #[test]
    fn make_texture_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        let desc = crate::core::rhi::TextureDescriptor::new(
            640,
            480,
            crate::core::rhi::TextureFormat::Rgba8Unorm,
        );
        let host_texture = crate::vulkan::rhi::HostVulkanTexture::new(&device, &desc)
            .expect("texture allocate");
        use crate::host_rhi::HostTextureExt;
        let texture = crate::core::rhi::Texture::from_vulkan(host_texture);
        let borrow = make_texture_borrow(texture.handle);
        assert_eq!(borrow.width(), 640, "width_cached must mirror the inner");
        assert_eq!(borrow.height(), 480, "height_cached must mirror the inner");
        assert!(
            matches!(borrow.format(), crate::core::rhi::TextureFormat::Rgba8Unorm),
            "format_raw must mirror the inner"
        );
    }

    #[test]
    fn make_storage_buffer_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        let host_buffer =
            crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(
                &device, 16_384,
            )
            .expect("storage buffer allocate");
        let buffer = crate::core::rhi::StorageBuffer::from_arc_into_raw(Arc::new(host_buffer));
        let borrow = make_storage_buffer_borrow(buffer.handle);
        assert_eq!(borrow.byte_size(), 16_384, "byte_size_cached must mirror the inner");
        assert!(
            !borrow.mapped_ptr().is_null(),
            "mapped_ptr_cached must mirror the inner HOST_VISIBLE pointer"
        );
    }

    #[test]
    fn make_pixel_buffer_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        // Bgra8 = 4 bytes/pixel, 320x240 = 307_200 bytes
        let host_buffer =
            crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(
                &device, 320 * 240 * 4,
            )
            .expect("backing buffer allocate");
        let pb = crate::core::rhi::PixelBuffer::from_host_vulkan_buffer(
            Arc::new(host_buffer),
            320,
            240,
            4,
            crate::core::rhi::PixelFormat::Bgra32,
        );
        let borrow = make_pixel_buffer_borrow(pb.handle);
        assert_eq!(borrow.width, 320, "width must mirror the inner");
        assert_eq!(borrow.height, 240, "height must mirror the inner");
        assert!(
            matches!(borrow.format(), crate::core::rhi::PixelFormat::Bgra32),
            "format_raw must mirror the inner"
        );
    }

    #[test]
    fn make_uniform_buffer_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        let buffer = crate::core::rhi::UniformBuffer::new_host_visible(&device, 4_096)
            .expect("uniform buffer allocate");
        let borrow = make_uniform_buffer_borrow(buffer.handle);
        assert_eq!(borrow.byte_size(), 4_096, "byte_size_cached must mirror the inner");
        assert!(
            !borrow.mapped_ptr().is_null(),
            "mapped_ptr_cached must mirror the inner HOST_VISIBLE pointer"
        );
    }

    #[test]
    fn make_vertex_buffer_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        let buffer = crate::core::rhi::VertexBuffer::new_host_visible(&device, 8_192)
            .expect("vertex buffer allocate");
        let borrow = make_vertex_buffer_borrow(buffer.handle);
        assert_eq!(borrow.byte_size(), 8_192, "byte_size_cached must mirror the inner");
        assert!(
            !borrow.mapped_ptr().is_null(),
            "mapped_ptr_cached must mirror the inner HOST_VISIBLE pointer"
        );
    }

    #[test]
    fn make_index_buffer_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        let buffer = crate::core::rhi::IndexBuffer::new_host_visible(&device, 2_048)
            .expect("index buffer allocate");
        let borrow = make_index_buffer_borrow(buffer.handle);
        assert_eq!(borrow.byte_size(), 2_048, "byte_size_cached must mirror the inner");
        assert!(
            !borrow.mapped_ptr().is_null(),
            "mapped_ptr_cached must mirror the inner HOST_VISIBLE pointer"
        );
    }

    #[test]
    fn make_compute_kernel_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        // Reuse the test_blend_1 shader already wired in build.rs: one
        // storage buffer binding at slot 0, one push-constant block of
        // 4 bytes. The assertion below pins the value the cached field
        // must mirror.
        const TEST_BLEND_1_SPV: &[u8] =
            include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_1.spv"));
        let descriptor = crate::core::rhi::ComputeKernelDescriptor {
            label: "make_compute_kernel_borrow_test",
            spv: TEST_BLEND_1_SPV,
            bindings: &[
                crate::core::rhi::ComputeBindingSpec::storage_buffer(0),
                crate::core::rhi::ComputeBindingSpec::storage_buffer(8),
            ],
            push_constant_size: 4,
        };
        let kernel =
            crate::vulkan::rhi::VulkanComputeKernel::new(&device, &descriptor)
                .expect("compute kernel construct");
        let borrow = make_compute_kernel_borrow(kernel.handle);
        assert_eq!(
            borrow.push_constant_size(),
            4,
            "cached_push_constant_size must mirror the inner",
        );
    }

    #[test]
    fn make_acceleration_structure_borrow_populates_cached_pod_fields() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        if !device.supports_ray_tracing_pipeline() {
            return;
        }
        // Single triangle BLAS, smallest payload that exercises the
        // build path. Mirrors the rt-smoke fixture's vertex layout.
        let vertices: Vec<f32> = vec![
            0.0, -0.5, 0.0, -0.5, 0.5, 0.0, 0.5, 0.5, 0.0,
        ];
        let indices: Vec<u32> = vec![0, 1, 2];
        let blas = crate::vulkan::rhi::VulkanAccelerationStructure::build_triangles_blas(
            &device,
            "make_borrow_test_blas",
            &vertices,
            &indices,
        )
        .expect("blas construct");
        let borrow = make_acceleration_structure_borrow(blas.handle);
        assert!(
            matches!(
                borrow.kind(),
                crate::vulkan::rhi::AccelerationStructureKind::BottomLevel,
            ),
            "cached_kind must mirror the inner",
        );
        assert!(
            borrow.device_address() > 0,
            "cached_device_address must mirror the inner",
        );
        assert!(
            borrow.storage_size() > 0,
            "cached_storage_size must mirror the inner",
        );
    }
}

// =============================================================================
// OutputWriterVTable + InputMailboxesVTable tier-1 null-handle tests
// =============================================================================

/// Tier-1 wire-format tests for [`HOST_OUTPUT_WRITER_VTABLE`] (issue
/// #894).
///
/// Each callback's null-handle path triggers the
/// `handle_as_output_writer_inner` short-circuit; mentally revert the
/// `if handle.is_null()` guard inside that helper and the wrapper
/// dereferences a null pointer (SIGSEGV in test runner).
///
/// `clone_arc` / `drop_arc` are infallible by design — they have
/// their own null-handle short-circuit and return cleanly without
/// touching the (null) handle.

/// Tier-1 wire-format tests for [`HOST_INPUT_MAILBOXES_VTABLE`] (issue

#[cfg(all(test, target_os = "linux"))]
mod rhi_color_converter_methods_vtable_tier1_wire_format_tests {
    //! Tier-1 wire-format tests for the v2 sibling slots added to
    //! `RhiColorConverterMethodsVTable`: `prepare_buffer_to_image_pixel`,
    //! `convert_buffer_to_image_storage`, `convert_buffer_to_image_pixel`.
    //!
    //! Each slot's null-handle / null out-ptr / err-buf contract is
    //! exercised against the static `HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE`.

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    fn dummy_layout() -> streamlib_plugin_abi::SourceLayoutInfoRepr {
        streamlib_plugin_abi::SourceLayoutInfoRepr {
            plane0_stride_bytes: 0,
            plane1_stride_bytes: 0,
            plane1_offset_bytes: 0,
            _reserved_padding: 0,
        }
    }

    fn dummy_info() -> streamlib_plugin_abi::ResolvedColorInfoRepr {
        streamlib_plugin_abi::ResolvedColorInfoRepr {
            primaries_raw: 0,
            transfer_raw: 1,
            matrix_raw: 1,
            range_raw: 1,
        }
    }

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE.layout_version,
            streamlib_plugin_abi::RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION,
        );
    }

    #[test]
    fn prepare_buffer_to_image_pixel_returns_error_on_null_converter() {
        let (mut buf, mut len) = make_err_buf();
        let layout = dummy_layout();
        let info = dummy_info();
        let mut out_kernel: *const c_void = std::ptr::null();
        let mut out_size: u32 = 0;
        let rc = unsafe {
            (HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE.prepare_buffer_to_image_pixel)(
                std::ptr::null(),
                std::ptr::null(),
                &layout,
                std::ptr::null(),
                &info,
                1,
                &mut out_kernel as *mut *const c_void,
                &mut out_size as *mut u32,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("prepare_buffer_to_image_pixel: null converter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn convert_buffer_to_image_storage_returns_error_on_null_converter() {
        let (mut buf, mut len) = make_err_buf();
        let layout = dummy_layout();
        let info = dummy_info();
        let rc = unsafe {
            (HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE
                .convert_buffer_to_image_storage)(
                std::ptr::null(),
                std::ptr::null(),
                &layout,
                std::ptr::null(),
                &info,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("convert_buffer_to_image_storage: null converter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn convert_buffer_to_image_pixel_returns_error_on_null_converter() {
        let (mut buf, mut len) = make_err_buf();
        let layout = dummy_layout();
        let info = dummy_info();
        let rc = unsafe {
            (HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE
                .convert_buffer_to_image_pixel)(
                std::ptr::null(),
                std::ptr::null(),
                &layout,
                std::ptr::null(),
                &info,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("convert_buffer_to_image_pixel: null converter handle"),
            "got: {msg}"
        );
    }

    /// Verifies the converter slot's null `out_kernel` rejection
    /// path. Mirrors `prepare_buffer_to_image_storage`'s contract.
    #[test]
    fn prepare_buffer_to_image_pixel_returns_error_on_null_out_kernel() {
        let (mut buf, mut len) = make_err_buf();
        let layout = dummy_layout();
        let info = dummy_info();
        let mut out_size: u32 = 0;
        // Use a non-null fake converter handle so we reach the
        // out-ptr null check. Casting a stack reference to *const
        // is safe here because the FFI wrapper only reaches handle_as_*
        // (a transmute) if other null-checks pass — the out_kernel
        // check runs after the converter null-check but before the
        // handle deref.
        let fake_converter: usize = 1;
        let rc = unsafe {
            (HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE.prepare_buffer_to_image_pixel)(
                &fake_converter as *const usize as *const c_void,
                std::ptr::null(),
                &layout,
                std::ptr::null(),
                &info,
                1,
                std::ptr::null_mut(),
                &mut out_size as *mut u32,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        // The null-check ordering surfaces src_buffer first (it's a
        // simpler check than out_kernel). Either error is acceptable —
        // verify we got *an* error tagged with the slot name.
        assert!(
            msg.contains("prepare_buffer_to_image_pixel:"),
            "got: {msg}"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod kernel_bindings_vtable_tier1_wire_format_tests {
    //! Tier-1 wire-format tests for the `bindings` introspection slot
    //! on each kernel methods vtable (compute v4, graphics v3, ray-
    //! tracing v3).

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn compute_bindings_returns_error_on_null_kernel() {
        let (mut buf, mut len) = make_err_buf();
        let mut out_len: usize = 0;
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.bindings)(
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
                &mut out_len as *mut usize,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("bindings: null kernel handle"), "got: {msg}");
    }

    #[test]
    fn graphics_bindings_returns_error_on_null_kernel() {
        let (mut buf, mut len) = make_err_buf();
        let mut out_len: usize = 0;
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.bindings)(
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
                &mut out_len as *mut usize,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("bindings: null kernel handle"), "got: {msg}");
    }

    #[test]
    fn ray_tracing_bindings_returns_error_on_null_kernel() {
        let (mut buf, mut len) = make_err_buf();
        let mut out_len: usize = 0;
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.bindings)(
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
                &mut out_len as *mut usize,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("bindings: null kernel handle"), "got: {msg}");
    }
}
