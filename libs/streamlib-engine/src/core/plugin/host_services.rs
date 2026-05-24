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
    AUDIO_CLOCK_VTABLE_LAYOUT_VERSION, GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION,
    GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION, HOST_SERVICES_LAYOUT_VERSION,
    PROCESSOR_VTABLE_LAYOUT_VERSION, RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION,
    RUNTIME_OPS_VTABLE_LAYOUT_VERSION, SURFACE_STORE_VTABLE_LAYOUT_VERSION,
};

// tokio is not exposed across the ABI. Lifecycle methods are
// synchronous at the trait surface; plugins that need async
// lifecycle work bring their own runtime. The host's tokio runtime
// stays invisible to plugins.

use crate::core::context::{RuntimeContext, SharedAudioClock};
use crate::core::pubsub::Event;
use crate::core::runtime::RuntimeOperations;

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
/// `pub(crate)` so the cdylib-side trampolines in
/// [`crate::core::context::audio_clock_shim`],
/// [`crate::core::context::runtime_ops_shim`], and the per-processor
/// vtable wrappers in [`crate::core::plugin::processor_vtable`] can
/// route through the same helper. Every extern "C" boundary crossing
/// in the engine — host-side and cdylib-side — must be wrapped.
#[inline]
pub(crate) fn run_host_extern_c<F, T>(
    callback_name: &'static str,
    body: F,
    default_on_panic: T,
) -> T
where
    F: FnOnce() -> T,
{
    use std::panic::{catch_unwind, AssertUnwindSafe};
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "<non-string panic payload>".to_string()
            };
            tracing::error!(
                target: "streamlib::ffi",
                callback = callback_name,
                panic = %msg,
                "host extern \"C\" callback panicked; FFI boundary converted panic to default return"
            );
            default_on_panic
        }
    }
}

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

            match crate::core::processors::PROCESSOR_REGISTRY
                .register_via_vtable(descriptor, vtable_ref)
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
// pointers and reading nothing about layout.

// ---------------- RuntimeContext vtable ----------------

unsafe extern "C" fn host_rcv_runtime_id_copy(
    ctx: *const c_void,
    out_buf: *mut u8,
    out_buf_cap: usize,
    out_len: *mut usize,
) -> usize {
    run_host_extern_c(
        "host_rcv_runtime_id_copy",
        || {
            if ctx.is_null() {
                if !out_len.is_null() {
                    // SAFETY: caller-provided `out_len` is writable.
                    unsafe { *out_len = 0 };
                }
                return 0;
            }
            // SAFETY: host-side construction passes &RuntimeContext as ctx.
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            let id_bytes = rc.runtime_id().as_str().as_bytes();
            write_id_bytes(id_bytes, out_buf, out_buf_cap, out_len)
        },
        0,
    )
}

unsafe extern "C" fn host_rcv_processor_id_copy(
    ctx: *const c_void,
    out_buf: *mut u8,
    out_buf_cap: usize,
    out_len: *mut usize,
) -> isize {
    run_host_extern_c(
        "host_rcv_processor_id_copy",
        || {
            if ctx.is_null() {
                // Mirror the panic-default — `-1` encodes "no processor
                // id" (shared/global ctx), which is the closest defined
                // value to "ctx unavailable". The cdylib treats `-1` as
                // Option::None.
                if !out_len.is_null() {
                    // SAFETY: caller-provided `out_len` is writable.
                    unsafe { *out_len = 0 };
                }
                return -1;
            }
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            match rc.processor_id() {
                Some(pid) => {
                    let bytes = pid.as_str().as_bytes();
                    write_id_bytes(bytes, out_buf, out_buf_cap, out_len) as isize
                }
                None => -1,
            }
        },
        -1,
    )
}

unsafe extern "C" fn host_rcv_is_paused(ctx: *const c_void) -> bool {
    run_host_extern_c(
        "host_rcv_is_paused",
        || {
            if ctx.is_null() {
                // Conservative default — a null ctx means the host's
                // RuntimeContext is unreachable, so the processor
                // should not keep running. Mirrors the panic-default.
                return true;
            }
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            rc.is_paused()
        },
        // Pause-on-panic is the conservative default: a panicking
        // is_paused() callback shouldn't keep a runaway processor
        // running. `true` halts further work until the host clears
        // the panic state.
        true,
    )
}

unsafe extern "C" fn host_rcv_should_process(ctx: *const c_void) -> bool {
    run_host_extern_c(
        "host_rcv_should_process",
        || {
            if ctx.is_null() {
                // Same conservative default — null ctx halts further
                // work until the host clears state.
                return false;
            }
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            rc.should_process()
        },
        // Same conservative default as is_paused — false halts the
        // processor until the host clears state.
        false,
    )
}

unsafe extern "C" fn host_rcv_gpu_full_access(_ctx: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_rcv_gpu_full_access",
        || {
            // FullAccess is engine-only today — the cdylib-facing
            // shim embeds `GpuContextFullAccess` by value alongside
            // its handle/vtable pair, so the cdylib never reaches
            // through this callback. Returns null until a future
            // phase wires cross-DSO FullAccess dispatch.
            std::ptr::null()
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_rcv_gpu_limited_access(_ctx: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_rcv_gpu_limited_access",
        || std::ptr::null(),
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_rcv_audio_clock_handle(ctx: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_rcv_audio_clock_handle",
        || {
            if ctx.is_null() {
                return std::ptr::null();
            }
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            // The shim's audio-clock handle is a `&SharedAudioClock` —
            // the accompanying [`HOST_AUDIO_CLOCK_VTABLE`] callbacks
            // cast it back to that type and invoke the Rust trait
            // methods.
            rc.audio_clock() as *const SharedAudioClock as *const c_void
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_rcv_runtime_ops_handle(ctx: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_rcv_runtime_ops_handle",
        || {
            if ctx.is_null() {
                return std::ptr::null();
            }
            let rc = unsafe { &*(ctx as *const RuntimeContext) };
            // `rc.runtime()` produces an owned `Arc<dyn
            // RuntimeOperations>` each call; the per-RuntimeContext
            // handle we hand the cdylib must outlive the call
            // boundary. We keep the canonical handle as
            // `&Arc<dyn RuntimeOperations>` borrowed out of the
            // RuntimeContext's internal storage, which lives as long
            // as the RuntimeContext itself.
            rc.runtime_operations_ref() as *const Arc<dyn RuntimeOperations> as *const c_void
        },
        std::ptr::null(),
    )
}

/// Static [`RuntimeContextVTable`] installed once per process and
/// reused for every cdylib's `RuntimeContext*Access` shim
/// construction. The host-side `RuntimeContextFullAccess::new` /
/// `RuntimeContextLimitedAccess::new` constructors capture
/// `&HOST_RUNTIME_CONTEXT_VTABLE` directly.
pub static HOST_RUNTIME_CONTEXT_VTABLE: RuntimeContextVTable = RuntimeContextVTable {
    layout_version: RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION,
    _reserved_padding: 0,
    runtime_id_copy: host_rcv_runtime_id_copy,
    processor_id_copy: host_rcv_processor_id_copy,
    is_paused: host_rcv_is_paused,
    should_process: host_rcv_should_process,
    gpu_full_access: host_rcv_gpu_full_access,
    gpu_limited_access: host_rcv_gpu_limited_access,
    audio_clock_handle: host_rcv_audio_clock_handle,
    runtime_ops_handle: host_rcv_runtime_ops_handle,
};

/// Pointer to the [`RuntimeContextVTable`] this DSO should dispatch
/// through. In the host process this returns the host's local
/// `&HOST_RUNTIME_CONTEXT_VTABLE` static (the canonical vtable). In
/// a cdylib `install_host_services` has populated the cached pointer
/// from `HostServices`, so this returns the HOST'S vtable — meaning
/// every callback invocation lands in host-resident extern "C"
/// functions, not in the cdylib's local copy of those functions.
/// That distinction is load-bearing: the host's functions read
/// host-owned Rust types (`RuntimeContext`) with the host's compiled
/// layout, while the cdylib's local copies would re-interpret the
/// same memory through the cdylib's compiled layout.
pub fn host_runtime_context_vtable() -> *const RuntimeContextVTable {
    match host_callbacks() {
        Some(c) if !c.runtime_context_vtable.is_null() => c.runtime_context_vtable,
        _ => &HOST_RUNTIME_CONTEXT_VTABLE,
    }
}

// ---------------- AudioClock vtable ----------------

unsafe extern "C" fn host_acv_sample_rate(handle: *const c_void) -> u32 {
    run_host_extern_c(
        "host_acv_sample_rate",
        || {
            if handle.is_null() {
                return 0;
            }
            let clock = unsafe { &*(handle as *const SharedAudioClock) };
            clock.sample_rate()
        },
        0,
    )
}

unsafe extern "C" fn host_acv_buffer_size(handle: *const c_void) -> usize {
    run_host_extern_c(
        "host_acv_buffer_size",
        || {
            if handle.is_null() {
                return 0;
            }
            let clock = unsafe { &*(handle as *const SharedAudioClock) };
            clock.buffer_size()
        },
        0,
    )
}

unsafe extern "C" fn host_acv_on_tick(
    handle: *const c_void,
    callback: unsafe extern "C" fn(*mut c_void, streamlib_plugin_abi::AudioTickContextRepr),
    user_data: *mut c_void,
    drop_user_data: unsafe extern "C" fn(*mut c_void),
) {
    run_host_extern_c(
        "host_acv_on_tick",
        || {
            // [`OnTickBridge`] owns the (callback, user_data,
            // drop_user_data) trio. Its `Drop` impl fires
            // `drop_user_data` exactly once, no matter where the
            // bridge ends up: stored on the clock (success path —
            // drop fires at clock teardown), dropped immediately
            // (null-handle path or panic before move), or dropped on
            // the unwind path between move and `clock.on_tick`
            // returning (panic-recovery path — the bridge moved into
            // `cb` drops when `cb` unwinds).
            //
            // This shape is the sole owner of the cleanup: the
            // wrapper's third argument to `run_host_extern_c` MUST
            // stay `()` (no `drop_user_data` call) — Rust evaluates
            // function arguments eagerly, so a third-arg side effect
            // would fire `drop_user_data` unconditionally before the
            // body even runs, double-firing it on every success path.
            let bridge = OnTickBridge {
                callback,
                user_data,
                drop_user_data,
            };
            if handle.is_null() {
                // Bridge drops here → drop_user_data fires once. The
                // explicit `drop(bridge)` is for clarity; lexical
                // scope alone would fire it on the same line.
                drop(bridge);
                return;
            }
            let clock = unsafe { &*(handle as *const SharedAudioClock) };

            // Bridge moves into the boxed closure. If
            // `clock.on_tick(cb)` panics before storing `cb`, the
            // unwind drops the Box → closure → bridge →
            // drop_user_data fires exactly once. If `clock.on_tick`
            // stores `cb` successfully, the bridge lives until the
            // clock tears down; drop_user_data fires then.
            let cb: Box<dyn Fn(crate::core::context::AudioTickContext) + Send + Sync> =
                Box::new(move |ctx_local| bridge.fire(ctx_local));
            clock.on_tick(cb);
        },
        // Intentional `()`: the cleanup contract is held entirely by
        // `OnTickBridge::Drop`. See body comment.
        (),
    )
}

/// Holder for the cdylib's `(callback, user_data, drop_user_data)`
/// trio. Owns the user-data pointer for the lifetime of the on-tick
/// registration; the deleter fires when the registration drops.
struct OnTickBridge {
    callback: unsafe extern "C" fn(*mut c_void, streamlib_plugin_abi::AudioTickContextRepr),
    user_data: *mut c_void,
    drop_user_data: unsafe extern "C" fn(*mut c_void),
}

// SAFETY: cdylib's ABI contract requires the callback + drop pair to be
// thread-safe. The on-tick callback may fire from any thread the host's
// audio clock chooses (today, the audio-clock thread).
unsafe impl Send for OnTickBridge {}
unsafe impl Sync for OnTickBridge {}

impl OnTickBridge {
    fn fire(&self, ctx: crate::core::context::AudioTickContext) {
        let repr = streamlib_plugin_abi::AudioTickContextRepr {
            timestamp_ns: ctx.timestamp_ns,
            samples_needed: ctx.samples_needed as u64,
            sample_rate: ctx.sample_rate,
            _reserved_padding: 0,
            tick_number: ctx.tick_number,
        };
        // SAFETY: callback + user_data come from the cdylib's ABI
        // promise; valid for the lifetime of this bridge.
        unsafe { (self.callback)(self.user_data, repr) };
    }
}

impl Drop for OnTickBridge {
    fn drop(&mut self) {
        // SAFETY: drop_user_data is part of the cdylib's ABI contract
        // and is called exactly once when this bridge is released.
        unsafe { (self.drop_user_data)(self.user_data) };
    }
}

/// Static [`AudioClockVTable`] installed once per process. Paired
/// with the per-RuntimeContext audio-clock handle returned by
/// [`HOST_RUNTIME_CONTEXT_VTABLE`]`::audio_clock_handle`.
pub static HOST_AUDIO_CLOCK_VTABLE: AudioClockVTable = AudioClockVTable {
    layout_version: AUDIO_CLOCK_VTABLE_LAYOUT_VERSION,
    _reserved_padding: 0,
    sample_rate: host_acv_sample_rate,
    buffer_size: host_acv_buffer_size,
    on_tick: host_acv_on_tick,
};

/// Pointer to the [`AudioClockVTable`] this DSO should dispatch
/// through. Same DSO-routing rule as
/// [`host_runtime_context_vtable`]: cdylib reads the host's pointer
/// from the cache populated by `install_host_services`; host falls
/// back to its local static.
pub fn host_audio_clock_vtable() -> *const AudioClockVTable {
    match host_callbacks() {
        Some(c) if !c.audio_clock_vtable.is_null() => c.audio_clock_vtable,
        _ => &HOST_AUDIO_CLOCK_VTABLE,
    }
}

// ---------------- RuntimeOps vtable ----------------
//
// The cdylib-side `RuntimeOpsShim` wraps each submit-with-completion
// callback in a `tokio::sync::oneshot` whose Sender is boxed and
// shipped across the FFI as the `user_data` pointer. The host's
// callback impl spawns on the host's tokio runtime (held in
// `HOST_RUNTIME_TOKIO_HANDLE`), awaits the real
// `RuntimeOperations::*_async` method, encodes the response payload,
// and fires the completion callback.

/// Set by the host once at startup before any cdylib registers. The
/// runtime-ops vtable's callbacks block on this handle to run the
/// real `*_async` methods on the host's tokio runtime, completely
/// invisible to the cdylib (which sees only a `oneshot` it polls on
/// its own runtime).
static HOST_RUNTIME_TOKIO_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();

/// Install the host's tokio handle so the [`HOST_RUNTIME_OPS_VTABLE`]
/// callbacks can spawn `*_async` futures against it. The host's
/// `Runner::start` calls this once before any cdylib is loaded.
/// Idempotent: subsequent calls with a different handle are silently
/// ignored.
pub fn install_host_runtime_tokio_handle(handle: tokio::runtime::Handle) {
    let _ = HOST_RUNTIME_TOKIO_HANDLE.set(handle);
}

fn host_tokio_handle() -> Option<&'static tokio::runtime::Handle> {
    HOST_RUNTIME_TOKIO_HANDLE.get()
}

unsafe fn invoke_completion(
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
    status: i32,
    bytes: &[u8],
) {
    // SAFETY: cdylib promises completion is safe to invoke with the
    // user_data pointer; payload bytes are valid for the call.
    unsafe { completion(user_data, status, bytes.as_ptr(), bytes.len()) };
}

/// RAII guard around the cdylib's submit-with-completion contract.
/// The ABI promises the host fires `completion(user_data, ...)`
/// exactly once. Without this guard a panic inside the spawned
/// `async` body (or a runtime shutdown that drops the future before
/// it awaits) would leak the cdylib's boxed `oneshot::Sender` and
/// hang the cdylib's `rx.await` forever. With the guard, the Drop
/// impl fires an aborted-task error completion if the explicit fire
/// path didn't run.
///
/// Holds `user_data` as a `usize` so the guard is `Send + Sync` (raw
/// pointers aren't). The completion fn pointer is naturally Send.
struct CompletionGuard {
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data_addr: usize,
    fired: bool,
}

impl CompletionGuard {
    fn new(
        completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
        user_data: *mut c_void,
    ) -> Self {
        Self {
            completion,
            user_data_addr: user_data as usize,
            fired: false,
        }
    }

    fn fire_with_result<T: serde::Serialize>(mut self, result: crate::core::Result<T>) {
        self.fired = true;
        let user_data_ptr = self.user_data_addr as *mut c_void;
        match result {
            Ok(value) => match rmp_serde::to_vec_named(&value) {
                Ok(bytes) => unsafe {
                    invoke_completion(self.completion, user_data_ptr, 0, &bytes)
                },
                Err(e) => {
                    let msg = format!("response msgpack encode failed: {e}");
                    unsafe {
                        invoke_completion(self.completion, user_data_ptr, -1, msg.as_bytes())
                    };
                }
            },
            Err(e) => {
                let msg = e.to_string();
                unsafe { invoke_completion(self.completion, user_data_ptr, -1, msg.as_bytes()) };
            }
        }
    }

    fn fire_err_msg(mut self, msg: &[u8]) {
        self.fired = true;
        let user_data_ptr = self.user_data_addr as *mut c_void;
        unsafe { invoke_completion(self.completion, user_data_ptr, -1, msg) };
    }
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        if !self.fired {
            // SAFETY: contract promise — completion is always fired
            // exactly once. A drop without a fire signals the host's
            // tokio task aborted (panic or runtime shutdown before
            // the future completed). The cdylib's completion
            // trampoline reclaims its boxed `Sender` either way.
            let user_data_ptr = self.user_data_addr as *mut c_void;
            let msg = b"runtime-ops host task aborted before completion";
            unsafe {
                invoke_completion(self.completion, user_data_ptr, -1, msg);
            }
        }
    }
}

// SAFETY: completion fn pointer is naturally Send; user_data is held
// as a `usize` so the guard can cross `.await` boundaries inside
// tokio task bodies.
unsafe impl Send for CompletionGuard {}
unsafe impl Sync for CompletionGuard {}

unsafe extern "C" fn host_rov_add_processor(
    handle: *const c_void,
    spec_msgpack_ptr: *const u8,
    spec_msgpack_len: usize,
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
) {
    run_host_extern_c(
        "host_rov_add_processor",
        || {
            if handle.is_null() {
                CompletionGuard::new(completion, user_data)
                    .fire_err_msg(b"add_processor: null handle");
                return;
            }
            let ops = unsafe { Arc::clone(&*(handle as *const Arc<dyn RuntimeOperations>)) };
            let guard = CompletionGuard::new(completion, user_data);
            let Some(rt) = host_tokio_handle() else {
                guard.fire_err_msg(b"host tokio handle not installed");
                return;
            };
            let spec_bytes = if spec_msgpack_len == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(spec_msgpack_ptr, spec_msgpack_len) }.to_vec()
            };
            rt.spawn(async move {
                let result = match rmp_serde::from_slice::<crate::core::processors::ProcessorSpec>(
                    &spec_bytes,
                ) {
                    Ok(spec) => ops.add_processor_async(spec).await,
                    Err(e) => Err(crate::core::Error::Config(format!(
                        "add_processor: spec msgpack decode failed: {e}"
                    ))),
                };
                guard.fire_with_result(result);
            });
        },
        // Sync-body panic: CompletionGuard's Drop fires the abort
        // completion if `guard` was constructed before the panic;
        // otherwise the cdylib's `rx.await` hangs. The cdylib's
        // RAII-on-Drop trampoline reclaims its boxed Sender either
        // way.
        (),
    )
}

unsafe extern "C" fn host_rov_remove_processor(
    handle: *const c_void,
    processor_id_msgpack_ptr: *const u8,
    processor_id_msgpack_len: usize,
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
) {
    run_host_extern_c(
        "host_rov_remove_processor",
        || {
            if handle.is_null() {
                CompletionGuard::new(completion, user_data)
                    .fire_err_msg(b"remove_processor: null handle");
                return;
            }
            let ops = unsafe { Arc::clone(&*(handle as *const Arc<dyn RuntimeOperations>)) };
            let guard = CompletionGuard::new(completion, user_data);
            let Some(rt) = host_tokio_handle() else {
                guard.fire_err_msg(b"host tokio handle not installed");
                return;
            };
            let id_bytes = if processor_id_msgpack_len == 0 {
                Vec::new()
            } else {
                unsafe {
                    std::slice::from_raw_parts(processor_id_msgpack_ptr, processor_id_msgpack_len)
                }
                .to_vec()
            };
            rt.spawn(async move {
                let result = match rmp_serde::from_slice::<crate::core::graph::ProcessorUniqueId>(
                    &id_bytes,
                ) {
                    Ok(pid) => ops.remove_processor_async(pid).await,
                    Err(e) => Err(crate::core::Error::Config(format!(
                        "remove_processor: processor_id msgpack decode failed: {e}"
                    ))),
                };
                guard.fire_with_result(result);
            });
        },
        (),
    )
}

unsafe extern "C" fn host_rov_connect(
    handle: *const c_void,
    from_msgpack_ptr: *const u8,
    from_msgpack_len: usize,
    to_msgpack_ptr: *const u8,
    to_msgpack_len: usize,
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
) {
    run_host_extern_c(
        "host_rov_connect",
        || {
            if handle.is_null() {
                CompletionGuard::new(completion, user_data)
                    .fire_err_msg(b"connect: null handle");
                return;
            }
            let ops = unsafe { Arc::clone(&*(handle as *const Arc<dyn RuntimeOperations>)) };
            let guard = CompletionGuard::new(completion, user_data);
            let Some(rt) = host_tokio_handle() else {
                guard.fire_err_msg(b"host tokio handle not installed");
                return;
            };
            let from_bytes = if from_msgpack_len == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(from_msgpack_ptr, from_msgpack_len) }.to_vec()
            };
            let to_bytes = if to_msgpack_len == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(to_msgpack_ptr, to_msgpack_len) }.to_vec()
            };
            rt.spawn(async move {
                let from: crate::core::OutputLinkPortRef =
                    match rmp_serde::from_slice(&from_bytes) {
                        Ok(v) => v,
                        Err(e) => {
                            let result: crate::core::Result<crate::core::graph::LinkUniqueId> =
                                Err(crate::core::Error::Config(format!(
                                    "connect: from-port msgpack decode failed: {e}"
                                )));
                            guard.fire_with_result(result);
                            return;
                        }
                    };
                let to: crate::core::InputLinkPortRef = match rmp_serde::from_slice(&to_bytes) {
                    Ok(v) => v,
                    Err(e) => {
                        let result: crate::core::Result<crate::core::graph::LinkUniqueId> =
                            Err(crate::core::Error::Config(format!(
                                "connect: to-port msgpack decode failed: {e}"
                            )));
                        guard.fire_with_result(result);
                        return;
                    }
                };
                let result = ops.connect_async(from, to).await;
                guard.fire_with_result(result);
            });
        },
        (),
    )
}

unsafe extern "C" fn host_rov_disconnect(
    handle: *const c_void,
    link_id_msgpack_ptr: *const u8,
    link_id_msgpack_len: usize,
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
) {
    run_host_extern_c(
        "host_rov_disconnect",
        || {
            if handle.is_null() {
                CompletionGuard::new(completion, user_data)
                    .fire_err_msg(b"disconnect: null handle");
                return;
            }
            let ops = unsafe { Arc::clone(&*(handle as *const Arc<dyn RuntimeOperations>)) };
            let guard = CompletionGuard::new(completion, user_data);
            let Some(rt) = host_tokio_handle() else {
                guard.fire_err_msg(b"host tokio handle not installed");
                return;
            };
            let bytes = if link_id_msgpack_len == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(link_id_msgpack_ptr, link_id_msgpack_len) }
                    .to_vec()
            };
            rt.spawn(async move {
                let result =
                    match rmp_serde::from_slice::<crate::core::graph::LinkUniqueId>(&bytes) {
                        Ok(link_id) => ops.disconnect_async(link_id).await,
                        Err(e) => Err(crate::core::Error::Config(format!(
                            "disconnect: link_id msgpack decode failed: {e}"
                        ))),
                    };
                guard.fire_with_result(result);
            });
        },
        (),
    )
}

unsafe extern "C" fn host_rov_to_json(
    handle: *const c_void,
    completion: streamlib_plugin_abi::RuntimeOpCompletionCallback,
    user_data: *mut c_void,
) {
    run_host_extern_c(
        "host_rov_to_json",
        || {
            if handle.is_null() {
                CompletionGuard::new(completion, user_data)
                    .fire_err_msg(b"to_json: null handle");
                return;
            }
            let ops = unsafe { Arc::clone(&*(handle as *const Arc<dyn RuntimeOperations>)) };
            let guard = CompletionGuard::new(completion, user_data);
            let Some(rt) = host_tokio_handle() else {
                guard.fire_err_msg(b"host tokio handle not installed");
                return;
            };
            rt.spawn(async move {
                let result = ops.to_json_async().await;
                guard.fire_with_result(result);
            });
        },
        (),
    )
}

/// Take a (borrowed) handle returned from
/// [`RuntimeContextVTable::runtime_ops_handle`] (a `*const Arc<dyn
/// RuntimeOperations>` pointing into `RuntimeContext`-owned storage)
/// and return a new owned handle: a `Box<Arc<dyn RuntimeOperations>>`
/// with an Arc refcount bump. The owned handle stays alive even if
/// the originating `RuntimeContext` is dropped, because the inner Arc
/// keeps the underlying `dyn RuntimeOperations` impl alive
/// independently. Cdylib drops it via [`host_rov_drop_handle`].
unsafe extern "C" fn host_rov_clone_handle(borrowed_handle: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_rov_clone_handle",
        || {
            if borrowed_handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: `borrowed_handle` came from `host_rcv_runtime_ops_handle`
            // which cast `&RuntimeContext.runtime_ops` to `*const c_void`.
            let original = unsafe { &*(borrowed_handle as *const Arc<dyn RuntimeOperations>) };
            let cloned: Arc<dyn RuntimeOperations> = Arc::clone(original);
            Box::into_raw(Box::new(cloned)) as *const c_void
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_rov_drop_handle(owned_handle: *const c_void) {
    run_host_extern_c(
        "host_rov_drop_handle",
        || {
            if owned_handle.is_null() {
                return;
            }
            // SAFETY: paired with `host_rov_clone_handle`'s `Box::into_raw`.
            unsafe {
                let _ = Box::from_raw(owned_handle as *mut Arc<dyn RuntimeOperations>);
            }
        },
        (),
    )
}

/// Static [`RuntimeOpsVTable`] installed once per process. Paired
/// with the per-RuntimeContext runtime-ops handle returned by
/// [`HOST_RUNTIME_CONTEXT_VTABLE`]`::runtime_ops_handle`.
pub static HOST_RUNTIME_OPS_VTABLE: RuntimeOpsVTable = RuntimeOpsVTable {
    layout_version: RUNTIME_OPS_VTABLE_LAYOUT_VERSION,
    _reserved_padding: 0,
    add_processor: host_rov_add_processor,
    remove_processor: host_rov_remove_processor,
    connect: host_rov_connect,
    disconnect: host_rov_disconnect,
    to_json: host_rov_to_json,
    clone_handle: host_rov_clone_handle,
    drop_handle: host_rov_drop_handle,
};

/// Pointer to the [`RuntimeOpsVTable`] this DSO should dispatch
/// through. Same DSO-routing rule as
/// [`host_runtime_context_vtable`].
pub fn host_runtime_ops_vtable() -> *const RuntimeOpsVTable {
    match host_callbacks() {
        Some(c) if !c.runtime_ops_vtable.is_null() => c.runtime_ops_vtable,
        _ => &HOST_RUNTIME_OPS_VTABLE,
    }
}

// ---------------- GpuContextLimitedAccess vtable ----------------
//
// Host-side implementations of every callback on the
// [`GpuContextLimitedAccessVTable`]. The static at the bottom of
// this block (`HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE`) wires them
// up; the cdylib-side mirror lives in the cdylib's statically-
// linked engine copy and reads through the host-installed pointer
// on [`HostServices::gpu_context_limited_access_vtable`].

unsafe extern "C" fn host_gpu_lim_clone_handle(borrowed_handle: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_gpu_lim_clone_handle",
        || {
            if borrowed_handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: `borrowed_handle` was produced by
            // `GpuContextLimitedAccess::new` (or a prior
            // `clone_handle`) as
            // `Box::into_raw(Box::new(Arc::new(GpuContext)))`.
            // Reading through `&*` and cloning the Arc bumps the
            // underlying refcount; we re-leak via
            // `Box::into_raw(Box::new(...))` so the caller gets a
            // fresh owned handle that matches `drop_handle`'s
            // expected shape.
            let original =
                unsafe { &*(borrowed_handle as *const std::sync::Arc<crate::core::context::GpuContext>) };
            Box::into_raw(Box::new(original.clone())) as *const c_void
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_handle(owned_handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_handle",
        || {
            if owned_handle.is_null() {
                return;
            }
            // SAFETY: paired with `GpuContextLimitedAccess::new` and
            // `host_gpu_lim_clone_handle` — both produce
            // `Box::into_raw(Box::new(Arc<GpuContext>))`. Reclaiming
            // via `Box::from_raw` drops the Arc, which decrements
            // the host's `Arc<GpuContext>` refcount and frees the
            // underlying `GpuContext` when the count reaches zero.
            unsafe {
                let _ = Box::from_raw(
                    owned_handle as *mut std::sync::Arc<crate::core::context::GpuContext>,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_clone_pixel_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_pixel_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: `handle` is a `*const c_void` cast of
            // `Arc::into_raw(Arc<PixelBufferRef>)` produced by
            // `PixelBuffer::new` (host-side). Re-interpreting it as
            // `*const PixelBufferRef` and bumping the strong count is the
            // documented `Arc::increment_strong_count` contract.
            unsafe {
                Arc::increment_strong_count(handle as *const crate::core::rhi::PixelBufferRef);
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_pixel_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_pixel_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with `host_gpu_lim_clone_pixel_buffer` and
            // `PixelBuffer::new`'s `Arc::into_raw` initial bump.
            // `Arc::decrement_strong_count` decrements; when refcount hits
            // zero the underlying `PixelBufferRef` is dropped along with
            // its platform buffer.
            unsafe {
                Arc::decrement_strong_count(handle as *const crate::core::rhi::PixelBufferRef);
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_strong_count_pixel_buffer(handle: *const c_void) -> usize {
    run_host_extern_c(
        "host_gpu_lim_strong_count_pixel_buffer",
        || {
            if handle.is_null() {
                return 0;
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<PixelBufferRef>)`-shaped
            // (see `PixelBuffer::new`'s `from_arc_into_raw`). We
            // reconstruct the `Arc` temporarily, read the strong count, and
            // immediately re-leak it via `Arc::into_raw` so the strong count
            // returns to its pre-call value — `Arc::strong_count_from_raw`
            // is not part of the public stable API. The reconstruction runs
            // in HOST-COMPILED code regardless of caller DSO, so the cdylib
            // never has to know `PixelBufferRef`'s in-memory layout.
            unsafe {
                let arc =
                    Arc::from_raw(handle as *const crate::core::rhi::PixelBufferRef);
                let count = Arc::strong_count(&arc);
                let _ = Arc::into_raw(arc);
                count
            }
        },
        0,
    )
}

unsafe extern "C" fn host_gpu_lim_plane_base_address_pixel_buffer(
    handle: *const c_void,
    plane_index: u32,
) -> *mut u8 {
    run_host_extern_c(
        "host_gpu_lim_plane_base_address_pixel_buffer",
        || {
            if handle.is_null() {
                return core::ptr::null_mut();
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<PixelBufferRef>)`-shaped;
            // the leaked strong count keeps the `PixelBufferRef` alive for
            // the duration of the call. We borrow `&PixelBufferRef` rather
            // than reconstructing the Arc to avoid touching the refcount.
            unsafe {
                let pb_ref = &*(handle as *const crate::core::rhi::PixelBufferRef);
                pb_ref.plane_base_address(plane_index)
            }
        },
        core::ptr::null_mut(),
    )
}

unsafe extern "C" fn host_gpu_lim_plane_size_pixel_buffer(
    handle: *const c_void,
    plane_index: u32,
) -> u64 {
    run_host_extern_c(
        "host_gpu_lim_plane_size_pixel_buffer",
        || {
            if handle.is_null() {
                return 0;
            }
            // SAFETY: same as `host_gpu_lim_plane_base_address_pixel_buffer`.
            unsafe {
                let pb_ref = &*(handle as *const crate::core::rhi::PixelBufferRef);
                pb_ref.plane_size(plane_index)
            }
        },
        0,
    )
}

// -------------------------------------------------------------------------
// Texture Arc-handle lifecycle
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_clone_texture(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_texture",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: `handle` is a `*const c_void` cast of
            // `Arc::into_raw(Arc<TextureInner>)` produced by host
            // code (see `Texture::from_arc_into_raw`).
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::rhi::texture::TextureInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_texture(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_texture",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with the `Arc::into_raw` in
            // `Texture::from_arc_into_raw` and any prior
            // `clone_texture` bumps.
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::rhi::texture::TextureInner,
                );
            }
        },
        (),
    )
}

// -------------------------------------------------------------------------
// Texture::native_handle DMA-BUF FD export (Phase F, #957)
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_texture_native_dma_buf_fd(
    texture_handle: *const c_void,
) -> i64 {
    run_host_extern_c(
        "host_gpu_lim_texture_native_dma_buf_fd",
        || {
            if texture_handle.is_null() {
                return -1;
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: `texture_handle` is the
                // `Arc::into_raw(Arc<TextureInner>)` pointer carried as the
                // cdylib-side `Texture::handle` field. Borrowing as
                // `&TextureInner` does not touch the refcount — the
                // caller's `Texture` keeps the Arc alive for the duration
                // of this dispatch.
                let inner = unsafe {
                    &*(texture_handle as *const crate::core::rhi::texture::TextureInner)
                };
                match inner.inner.export_dma_buf_fd() {
                    Ok(fd) => i64::from(fd),
                    Err(_) => -1,
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                // DMA-BUF is a Linux concept. macOS / Windows native
                // handles are deferred until those cdylib adapter paths
                // resume (see #908's AI Agent Notes).
                let _ = texture_handle;
                -1
            }
        },
        -1,
    )
}

// -------------------------------------------------------------------------
// Video-source timeline semaphore publish/clear (v12 — #958)
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_set_video_source_timeline_semaphore(
    handle: *const c_void,
    timeline_handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_set_video_source_timeline_semaphore",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            if timeline_handle.is_null() {
                return;
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: `timeline_handle` is a borrowed
                // `Arc::as_ptr(&Arc<HostVulkanTimelineSemaphore>)`
                // produced by the cdylib caller. Bump the refcount so
                // we can take a temporary owned Arc via `Arc::from_raw`;
                // the caller's Arc strong-count is unchanged.
                // Mirrors the `host_gpu_lim_register_texture` pattern
                // for borrowed `Arc<TextureInner>`-shaped handles.
                let ptr = timeline_handle
                    as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore;
                unsafe {
                    Arc::increment_strong_count(ptr);
                }
                let arc = unsafe { Arc::from_raw(ptr) };
                gpu.set_video_source_timeline_semaphore(&arc);
                // `arc` drops here, balancing the `increment_strong_count`
                // above. The slot holds its own `Arc::clone` (taken by
                // `set_video_source_timeline_semaphore` from the
                // borrow).
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = timeline_handle;
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_clear_video_source_timeline_semaphore(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_clear_video_source_timeline_semaphore",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            #[cfg(target_os = "linux")]
            {
                gpu.clear_video_source_timeline_semaphore();
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = gpu;
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_wait_timeline_semaphore(
    _handle: *const c_void,
    timeline_handle: *const c_void,
    value: u64,
    timeout_ns: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_wait_timeline_semaphore",
        || {
            // `gpu_handle` is intentionally ignored — the timeline
            // borrow carries its own `vulkanalia::Device`, so the
            // wait runs against the timeline directly without
            // dereferencing any `GpuContext` instance. The handle
            // stays in the wire format for cross-slot consistency.
            if timeline_handle.is_null() {
                write_err(
                    "wait_timeline_semaphore: null timeline_handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: `timeline_handle` is a borrowed pointer
                // from the cdylib's
                // `HostVulkanTimelineSemaphore::wait_via_vtable`
                // (which gets it via `self as *const Self`). The
                // host borrow lasts only for the duration of the
                // wait call. We call `wait_direct` to bypass the
                // `host_callbacks().is_some()` check on `wait()`
                // itself — otherwise the host would re-dispatch
                // through the vtable into infinite recursion.
                let timeline = unsafe {
                    &*(timeline_handle
                        as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore)
                };
                match timeline.wait_direct(value, timeout_ns) {
                    Ok(()) => 0,
                    Err(e) => {
                        write_err(
                            &format!("wait_timeline_semaphore: {e}"),
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        1
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (timeline_handle, value, timeout_ns);
                write_err(
                    "wait_timeline_semaphore: Linux-only",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                1
            }
        },
        1,
    )
}

// -------------------------------------------------------------------------
// PooledTextureHandle lifecycle — drop-only (v4)
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_drop_pooled_texture_handle(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_pooled_texture_handle",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with `Box::into_raw(Box<...>)` in
            // `PooledTextureHandle::from_parts`. Reclaiming via
            // `Box::from_raw` runs `Drop for PooledTextureHandleInner`
            // which releases the pool slot exactly once.
            unsafe {
                let _ = Box::from_raw(
                    handle as *mut crate::core::context::texture_pool::PooledTextureHandleInner,
                );
            }
        },
        (),
    )
}

// -------------------------------------------------------------------------
// Method dispatch — Texture-related (v4)
// -------------------------------------------------------------------------

/// Borrow a `&Arc<GpuContext>` from a `*const Arc<GpuContext>`-shaped
/// host handle. Caller must guarantee `handle` came from
/// [`crate::core::context::GpuContextLimitedAccess::new`] or
/// [`host_gpu_lim_clone_handle`]; both produce
/// `Box::into_raw(Box::new(Arc::new(...))) as *const c_void`.
unsafe fn handle_as_gpu_context(
    handle: *const c_void,
) -> Option<&'static Arc<crate::core::context::GpuContext>> {
    if handle.is_null() {
        return None;
    }
    // SAFETY: caller-supplied contract; the Box keeps the Arc alive
    // for the duration of the dispatch through the vtable.
    unsafe { Some(&*(handle as *const Arc<crate::core::context::GpuContext>)) }
}

unsafe fn slice_from_raw(ptr: *const u8, len: usize) -> &'static [u8] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: caller-supplied UTF-8 byte slice; the lifetime is
    // bounded by the dispatch (we never store the slice past return).
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

fn write_err(msg: &str, err_buf: *mut u8, err_buf_cap: usize, err_len: *mut usize) {
    let bytes = msg.as_bytes();
    let written = bytes.len().min(err_buf_cap);
    if written > 0 && !err_buf.is_null() {
        // SAFETY: caller-provided `err_buf` is writable for `err_buf_cap`.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), err_buf, written) };
    }
    if !err_len.is_null() {
        // SAFETY: caller-provided `err_len` is writable.
        unsafe { *err_len = written };
    }
}

unsafe extern "C" fn host_gpu_lim_register_texture(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    texture_handle: *const c_void,
    initial_layout_raw: i32,
) {
    run_host_extern_c(
        "host_gpu_lim_register_texture",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            if texture_handle.is_null() {
                return;
            }
            // SAFETY: `texture_handle` is `Arc::into_raw(Arc<TextureInner>)`-shaped.
            // Bump the refcount so we can hand the cache its own owned
            // Arc; the caller's Texture continues to own its own.
            unsafe {
                Arc::increment_strong_count(
                    texture_handle as *const crate::core::rhi::texture::TextureInner,
                );
            }
            // SAFETY: same shape as above; from_raw + the bump above
            // gives us a fresh Arc with the right refcount.
            let texture_arc = unsafe {
                Arc::from_raw(
                    texture_handle as *const crate::core::rhi::texture::TextureInner,
                )
            };
            let inner_ref = &*texture_arc;
            let width = inner_ref.width();
            let height = inner_ref.height();
            let format = inner_ref.format();
            // Re-wrap into a Texture via the host's from_arc_into_raw
            // helper — leaks the Arc back into the texture cache shape.
            let texture =
                crate::core::rhi::texture::Texture::from_arc_into_raw(
                    texture_arc, width, height, format,
                );
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => return,
            };
            #[cfg(target_os = "linux")]
            {
                let layout = streamlib_consumer_rhi::VulkanLayout(initial_layout_raw);
                gpu.register_texture_with_layout(id_str, texture, layout);
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = initial_layout_raw;
                gpu.register_texture(id_str, texture);
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_update_texture_registration_layout(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    layout_raw: i32,
) {
    run_host_extern_c(
        "host_gpu_lim_update_texture_registration_layout",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => return,
            };
            #[cfg(target_os = "linux")]
            {
                let layout = streamlib_consumer_rhi::VulkanLayout(layout_raw);
                gpu.update_texture_registration_layout(id_str, layout);
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (id_str, layout_raw);
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_acquire_texture(
    handle: *const c_void,
    width: u32,
    height: u32,
    format_raw: u32,
    usage_bits: u32,
    out_pooled_handle: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_texture",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err("acquire_texture: null gpu handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if out_pooled_handle.is_null() {
                write_err("acquire_texture: null out_pooled_handle", err_buf, err_buf_cap, err_len);
                return 1;
            }
            let format = match format_raw {
                0 => streamlib_consumer_rhi::TextureFormat::Rgba8Unorm,
                1 => streamlib_consumer_rhi::TextureFormat::Rgba8UnormSrgb,
                2 => streamlib_consumer_rhi::TextureFormat::Bgra8Unorm,
                3 => streamlib_consumer_rhi::TextureFormat::Bgra8UnormSrgb,
                4 => streamlib_consumer_rhi::TextureFormat::Rgba16Float,
                5 => streamlib_consumer_rhi::TextureFormat::Rgba32Float,
                6 => streamlib_consumer_rhi::TextureFormat::Nv12,
                _ => {
                    let msg = format!("acquire_texture: invalid format_raw {}", format_raw);
                    write_err(&msg, err_buf, err_buf_cap, err_len);
                    return 1;
                }
            };
            let usage =
                streamlib_consumer_rhi::TextureUsages::from_bits_truncate(usage_bits);
            let desc = crate::core::context::TexturePoolDescriptor {
                width,
                height,
                format,
                usage,
                label: None,
            };
            match gpu.acquire_texture(&desc) {
                Ok(pooled) => {
                    // Move the host-built PooledTextureHandle into the
                    // caller's out-slot. The caller (cdylib) owns it
                    // after this — its Drop runs `drop_pooled_texture_handle`.
                    unsafe {
                        std::ptr::write(
                            out_pooled_handle
                                as *mut crate::core::context::PooledTextureHandle,
                            pooled,
                        );
                    }
                    0
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    write_err(&msg, err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_gpu_lim_resolve_texture_by_surface_id(
    handle: *const c_void,
    surface_id_ptr: *const u8,
    surface_id_len: usize,
    has_layout: i32,
    layout_raw: i32,
    width: u32,
    height: u32,
    out_texture: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_resolve_texture_by_surface_id",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "resolve_texture_by_surface_id: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_texture.is_null() {
                write_err(
                    "resolve_texture_by_surface_id: null out_texture",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(surface_id_ptr, surface_id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "resolve_texture_by_surface_id: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let texture_layout = if has_layout != 0 {
                Some(layout_raw)
            } else {
                None
            };
            match gpu.resolve_texture_by_surface_id(id_str, texture_layout, width, height) {
                Ok(texture) => {
                    // Hand the texture to the caller's out-slot. The
                    // caller (cdylib) owns it after this — its Drop
                    // runs `drop_texture`.
                    unsafe {
                        std::ptr::write(
                            out_texture as *mut crate::core::rhi::Texture,
                            texture,
                        );
                    }
                    0
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    write_err(&msg, err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_gpu_lim_unregister_texture(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
) {
    run_host_extern_c(
        "host_gpu_lim_unregister_texture",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => return,
            };
            gpu.unregister_texture(id_str);
        },
        (),
    )
}

// -------------------------------------------------------------------------
// Escalate scope transition (Phase C3)
// -------------------------------------------------------------------------

/// Begin an escalate scope on the supplied `gpu_handle`. Mints a
/// unique opaque token via
/// [`crate::core::context::escalate_scope_registry::begin_escalate_scope`]
/// and writes it into `*out_scope_token`. Blocking on the gate is
/// expected — the host's escalate gate serializes against any
/// concurrent escalate scope on the same `GpuContext`.
unsafe extern "C" fn host_gpu_lim_escalate_begin(
    handle: *const c_void,
    out_scope_token: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_escalate_begin",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "escalate_begin: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1i32;
            };
            if out_scope_token.is_null() {
                write_err(
                    "escalate_begin: null out_scope_token",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1i32;
            }
            // begin_escalate_scope clones the Arc into the registry
            // and enters the gate; both operations succeed without
            // returning a fallible value.
            let token = crate::core::context::escalate_scope_registry::begin_escalate_scope(
                Arc::clone(gpu),
            );
            // SAFETY: out_scope_token is non-null per the check above.
            // Token encoding is just the u64 serial reinterpreted as
            // pointer-shaped; cdylib treats it as opaque.
            unsafe { *out_scope_token = token as *const c_void };
            0
        },
        1,
    )
}

/// End an escalate scope. Removes the bound `Arc<GpuContext>` from
/// the registry (releasing the escalate gate), then runs
/// [`GpuContext::wait_device_idle`] to match the host-mode escalate
/// path's scope-end semantics. Idempotent for stale or never-issued
/// tokens.
unsafe extern "C" fn host_gpu_lim_escalate_end(
    _handle: *const c_void,
    scope_token: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_escalate_end",
        || {
            let token = scope_token as u64;
            // Resolve the Arc BEFORE removing it from the registry so
            // we can call wait_device_idle. If the token is stale or
            // never-issued, this returns None — silently no-op (the
            // gate was never acquired by this token, so there's
            // nothing to release).
            let arc_clone = crate::core::context::escalate_scope_registry::with_scope(
                token,
                Arc::clone,
            );
            let removed = crate::core::context::escalate_scope_registry::end_escalate_scope(token);
            if !removed {
                return 0i32;
            }
            match arc_clone.as_ref().map(|arc| arc.wait_device_idle()) {
                Some(Ok(())) | None => 0,
                Some(Err(e)) => {
                    write_err(
                        &format!("escalate_end: wait_device_idle failed: {e}"),
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

// -------------------------------------------------------------------------
// Linux-only buffer Arc-handle lifecycle
// -------------------------------------------------------------------------
//
// All 4 buffer types (`StorageBuffer`, `UniformBuffer`, `VertexBuffer`,
// `IndexBuffer`) wrap `Arc<HostVulkanBuffer>` under the hood. The per-
// type callbacks are individually addressable in the vtable (so future
// per-type divergence doesn't force a re-version) but share the same
// host-side bookkeeping today. On non-Linux hosts the buffer types
// don't exist, so the callbacks compile to no-ops / error returns —
// the vtable slot is unconditional for ABI stability.

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_clone_host_vulkan_buffer_arc(handle: *const c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: `handle` is `Arc::into_raw(Arc<HostVulkanBuffer>)`-shaped
    // (see each buffer type's `from_arc_into_raw` constructor).
    unsafe {
        Arc::increment_strong_count(handle as *const crate::vulkan::rhi::HostVulkanBuffer);
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_drop_host_vulkan_buffer_arc(handle: *const c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: matched with the `Arc::into_raw` in each buffer type's
    // `from_arc_into_raw` constructor.
    unsafe {
        Arc::decrement_strong_count(handle as *const crate::vulkan::rhi::HostVulkanBuffer);
    }
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_clone_host_vulkan_buffer_arc(_handle: *const c_void) {
    // Buffer types only exist on Linux; this callback is unreachable
    // on other platforms. Defensive no-op.
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_drop_host_vulkan_buffer_arc(_handle: *const c_void) {
    // Buffer types only exist on Linux; defensive no-op.
}

// Per-type wrappers. Each just delegates to the shared
// `host_vulkan_buffer_arc` pair today but lives in the vtable as a
// dedicated slot, so a future per-type divergence (e.g. UniformBuffer
// growing a per-type cached field that needs its own clone semantics)
// only edits the wrapper without touching the vtable surface.

unsafe extern "C" fn host_gpu_lim_clone_storage_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_storage_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_storage_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_storage_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_clone_uniform_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_uniform_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_uniform_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_uniform_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_clone_vertex_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_vertex_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_vertex_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_vertex_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_clone_index_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_index_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_index_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_index_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

// -------------------------------------------------------------------------
// Linux-only acquire_*_buffer method dispatch (v5)
// -------------------------------------------------------------------------

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_acquire_storage_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_storage_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_storage_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_storage_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_storage_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(
                            out_buffer as *mut crate::core::rhi::StorageBuffer,
                            buf,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_acquire_uniform_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_uniform_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_uniform_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_uniform_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_uniform_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(
                            out_buffer as *mut crate::core::rhi::UniformBuffer,
                            buf,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_acquire_vertex_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_vertex_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_vertex_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_vertex_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_vertex_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(
                            out_buffer as *mut crate::core::rhi::VertexBuffer,
                            buf,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_acquire_index_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_index_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_index_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_index_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_index_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(
                            out_buffer as *mut crate::core::rhi::IndexBuffer,
                            buf,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_acquire_storage_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_storage_buffer: StorageBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_acquire_uniform_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_uniform_buffer: UniformBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_acquire_vertex_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_vertex_buffer: VertexBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_acquire_index_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_index_buffer: IndexBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

// -------------------------------------------------------------------------
// TextureRegistration Arc-handle lifecycle
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_clone_texture_registration(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_texture_registration",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<TextureRegistrationInner>)`-shaped.
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::context::texture_registration::TextureRegistrationInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_texture_registration(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_texture_registration",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with the `Arc::into_raw` in
            // `TextureRegistration::from_arc_into_raw`.
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::context::texture_registration::TextureRegistrationInner,
                );
            }
        },
        (),
    )
}

// -------------------------------------------------------------------------
// TextureRegistration method dispatch (v6)
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_texture_registration_texture(
    handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "host_gpu_lim_texture_registration_texture",
        || {
            if handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<TextureRegistrationInner>)`-shaped;
            // the Arc's strong count keeps the inner alive. We return
            // a pointer to the inner's `texture` field; the caller
            // (cdylib) deref's it as `*const Texture`. The pointer is
            // alive as long as the caller's `TextureRegistration` is.
            unsafe {
                let inner = &*(handle
                    as *const crate::core::context::texture_registration::TextureRegistrationInner);
                &inner.texture as *const crate::core::rhi::Texture as *const c_void
            }
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_gpu_lim_texture_registration_current_layout(
    handle: *const c_void,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_texture_registration_current_layout",
        || {
            if handle.is_null() {
                return 0; // VK_IMAGE_LAYOUT_UNDEFINED
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: `handle` is `Arc::into_raw(...)`-shaped.
                unsafe {
                    let inner = &*(handle
                        as *const crate::core::context::texture_registration::TextureRegistrationInner);
                    inner
                        .current_layout
                        .load(std::sync::atomic::Ordering::Acquire)
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = handle;
                0
            }
        },
        0,
    )
}

unsafe extern "C" fn host_gpu_lim_texture_registration_update_layout(
    handle: *const c_void,
    layout_raw: i32,
) {
    run_host_extern_c(
        "host_gpu_lim_texture_registration_update_layout",
        || {
            if handle.is_null() {
                return;
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: same shape as
                // `host_gpu_lim_texture_registration_current_layout`.
                unsafe {
                    let inner = &*(handle
                        as *const crate::core::context::texture_registration::TextureRegistrationInner);
                    inner
                        .current_layout
                        .store(layout_raw, std::sync::atomic::Ordering::Release);
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (handle, layout_raw);
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_resolve_texture_registration_by_surface_id(
    handle: *const c_void,
    surface_id_ptr: *const u8,
    surface_id_len: usize,
    has_layout: i32,
    layout_raw: i32,
    width: u32,
    height: u32,
    out_registration: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_resolve_texture_registration_by_surface_id",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "resolve_texture_registration_by_surface_id: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_registration.is_null() {
                write_err(
                    "resolve_texture_registration_by_surface_id: null out_registration",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(surface_id_ptr, surface_id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "resolve_texture_registration_by_surface_id: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let texture_layout = if has_layout != 0 {
                Some(layout_raw)
            } else {
                None
            };
            match gpu.resolve_texture_registration_by_surface_id(id_str, texture_layout, width, height) {
                Ok(reg) => {
                    // SAFETY: out_registration points at caller-allocated
                    // stack storage for a `TextureRegistration` value.
                    unsafe {
                        std::ptr::write(
                            out_registration
                                as *mut crate::core::context::TextureRegistration,
                            reg,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

// -------------------------------------------------------------------------
// RhiCommandQueue Arc-handle lifecycle + create_command_buffer
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_clone_rhi_command_queue(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_rhi_command_queue",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<RhiCommandQueueInner>)`-shaped.
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::rhi::command_queue::RhiCommandQueueInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_rhi_command_queue(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_rhi_command_queue",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with the `Arc::into_raw` in
            // `RhiCommandQueue::from_arc_into_raw`.
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::rhi::command_queue::RhiCommandQueueInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_create_command_buffer_from_queue(
    queue_handle: *const c_void,
    out_cb: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_create_command_buffer_from_queue",
        || -> i32 {
            if queue_handle.is_null() {
                write_err(
                    "create_command_buffer_from_queue: null queue handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if out_cb.is_null() {
                write_err(
                    "create_command_buffer_from_queue: null out_cb",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // SAFETY: `queue_handle` is
            // `Arc::into_raw(Arc<RhiCommandQueueInner>)`-shaped; the
            // Arc's strong count keeps the inner alive for the duration.
            let inner = unsafe {
                &*(queue_handle
                    as *const crate::core::rhi::command_queue::RhiCommandQueueInner)
            };
            let result = inner.inner.create_command_buffer();
            match result {
                Ok(platform_cb) => {
                    let cb_inner =
                        crate::core::rhi::command_buffer::CommandBufferInner {
                            inner: platform_cb,
                        };
                    let cb = crate::core::rhi::CommandBuffer::from_inner(cb_inner);
                    // SAFETY: out_cb points at caller-allocated stack
                    // storage for a CommandBuffer value.
                    unsafe {
                        std::ptr::write(
                            out_cb as *mut crate::core::rhi::CommandBuffer,
                            cb,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

// -------------------------------------------------------------------------
// CommandBuffer lifecycle: drop + consume-semantics commits (v7)
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_drop_command_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_command_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with `Box::into_raw` in
            // `CommandBuffer::from_inner`.
            unsafe {
                let _ = Box::from_raw(
                    handle as *mut crate::core::rhi::command_buffer::CommandBufferInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_commit_command_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_commit_command_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with `Box::into_raw` in
            // `CommandBuffer::from_inner`; the cdylib's commit(self)
            // nulls its local fields after this call so Drop won't
            // double-free. We move-out of the Box so the platform
            // commit can take ownership of the inner by-value.
            let cb_box = unsafe {
                Box::from_raw(
                    handle as *mut crate::core::rhi::command_buffer::CommandBufferInner,
                )
            };
            let cb_inner = *cb_box;
            cb_inner.inner.commit();
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_commit_and_wait_command_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_commit_and_wait_command_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: see `host_gpu_lim_commit_command_buffer`.
            let cb_box = unsafe {
                Box::from_raw(
                    handle as *mut crate::core::rhi::command_buffer::CommandBufferInner,
                )
            };
            let cb_inner = *cb_box;
            cb_inner.inner.commit_and_wait();
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_copy_texture_command_buffer(
    handle: *const c_void,
    src: *const c_void,
    dst: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_copy_texture_command_buffer",
        || {
            if handle.is_null() || src.is_null() || dst.is_null() {
                return;
            }
            // SAFETY: handle is `Box::into_raw(...)`-shaped; `&mut` is
            // sound because the cdylib's `&mut self` guarantees no
            // concurrent reference. src/dst are
            // `*const Texture` (layout locked by `texture_layout` test).
            unsafe {
                let cb_inner = &mut *(handle
                    as *mut crate::core::rhi::command_buffer::CommandBufferInner);
                let src_tex = &*(src as *const crate::core::rhi::Texture);
                let dst_tex = &*(dst as *const crate::core::rhi::Texture);
                // Re-use the existing platform-specific copy_texture
                // surface inside CommandBufferInner's `inner`.
                #[cfg(all(
                    not(feature = "backend-vulkan"),
                    any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
                ))]
                {
                    cb_inner.inner.copy_texture(
                        &src_tex.host_inner().inner,
                        &dst_tex.host_inner().inner,
                    );
                }
                #[cfg(any(
                    feature = "backend-vulkan",
                    all(target_os = "linux", not(feature = "backend-metal"))
                ))]
                {
                    use crate::host_rhi::HostTextureExt;
                    cb_inner
                        .inner
                        .copy_texture(src_tex.vulkan_inner(), dst_tex.vulkan_inner());
                }
                #[cfg(target_os = "windows")]
                {
                    cb_inner.inner.copy_texture(
                        &src_tex.host_inner().inner,
                        &dst_tex.host_inner().inner,
                    );
                }
            }
        },
        (),
    )
}

// -------------------------------------------------------------------------
// GpuContextLimitedAccess command-queue / command-buffer / blit methods
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_command_queue(
    gpu_handle: *const c_void,
    out_queue: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_command_queue",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err("command_queue: null gpu handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if out_queue.is_null() {
                write_err("command_queue: null out_queue", err_buf, err_buf_cap, err_len);
                return 1;
            }
            // `gpu.command_queue()` returns `&RhiCommandQueue` (a borrow
            // from GpuContext's stored field). Clone into a fresh owned
            // β-shape for the caller — the Clone impl runs the host's
            // `clone_rhi_command_queue` callback (Arc refcount bump).
            let owned = gpu.command_queue().clone();
            // SAFETY: out_queue points at caller-allocated stack storage.
            unsafe {
                std::ptr::write(
                    out_queue as *mut crate::core::rhi::RhiCommandQueue,
                    owned,
                );
            }
            0
        },
        1,
    )
}

unsafe extern "C" fn host_gpu_lim_create_command_buffer(
    gpu_handle: *const c_void,
    out_cb: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_create_command_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "create_command_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_cb.is_null() {
                write_err(
                    "create_command_buffer: null out_cb",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.create_command_buffer() {
                Ok(cb) => {
                    // SAFETY: out_cb points at caller-allocated storage.
                    unsafe {
                        std::ptr::write(
                            out_cb as *mut crate::core::rhi::CommandBuffer,
                            cb,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_lim_copy_pixel_buffer_to_texture(
    gpu_handle: *const c_void,
    pixel_buffer: *const c_void,
    texture: *const c_void,
    surface_id_ptr: *const u8,
    surface_id_len: usize,
    width: u32,
    height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_copy_pixel_buffer_to_texture",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "copy_pixel_buffer_to_texture: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer.is_null() || texture.is_null() {
                write_err(
                    "copy_pixel_buffer_to_texture: null pixel_buffer or texture",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // SAFETY: pixel_buffer / texture point at β-shape values
            // whose layouts are locked by per-type regression tests.
            let pb = unsafe { &*(pixel_buffer as *const crate::core::rhi::PixelBuffer) };
            let tex = unsafe { &*(texture as *const crate::core::rhi::Texture) };
            let id_bytes = unsafe { slice_from_raw(surface_id_ptr, surface_id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "copy_pixel_buffer_to_texture: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match gpu.copy_pixel_buffer_to_texture(pb, tex, id_str, width, height) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_lim_copy_pixel_buffer_to_texture(
    _gpu_handle: *const c_void,
    _pixel_buffer: *const c_void,
    _texture: *const c_void,
    _surface_id_ptr: *const u8,
    _surface_id_len: usize,
    _width: u32,
    _height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "copy_pixel_buffer_to_texture: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

unsafe extern "C" fn host_gpu_lim_blit_copy(
    gpu_handle: *const c_void,
    src: *const c_void,
    dst: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_blit_copy",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err("blit_copy: null gpu handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if src.is_null() || dst.is_null() {
                write_err("blit_copy: null src or dst", err_buf, err_buf_cap, err_len);
                return 1;
            }
            // SAFETY: src / dst point at β-shape PixelBuffer values.
            let src_pb = unsafe { &*(src as *const crate::core::rhi::PixelBuffer) };
            let dst_pb = unsafe { &*(dst as *const crate::core::rhi::PixelBuffer) };
            match gpu.blit_copy(src_pb, dst_pb) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn host_gpu_lim_blit_copy_iosurface(
    gpu_handle: *const c_void,
    src_iosurface_ref: *const c_void,
    dst_pixel_buffer: *const c_void,
    width: u32,
    height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_blit_copy_iosurface",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "blit_copy_iosurface: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if dst_pixel_buffer.is_null() {
                write_err(
                    "blit_copy_iosurface: null dst_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let dst_pb = unsafe {
                &*(dst_pixel_buffer as *const crate::core::rhi::PixelBuffer)
            };
            let src_io = src_iosurface_ref as crate::apple::corevideo_ffi::IOSurfaceRef;
            match unsafe { gpu.blit_copy_iosurface(src_io, dst_pb, width, height) } {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "macos"))]
unsafe extern "C" fn host_gpu_lim_blit_copy_iosurface(
    _gpu_handle: *const c_void,
    _src_iosurface_ref: *const c_void,
    _dst_pixel_buffer: *const c_void,
    _width: u32,
    _height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "blit_copy_iosurface: not available on this platform (macOS-only)",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

// -------------------------------------------------------------------------
// GpuContextLimitedAccessVTable — surface_store accessors
// -------------------------------------------------------------------------

unsafe extern "C" fn host_gpu_lim_surface_store(
    gpu_handle: *const c_void,
    out_store: *mut c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_surface_store",
        || {
            // Always-clear: write a null-handle β-shape first so the
            // caller has a defined state even on error paths.
            if !out_store.is_null() {
                unsafe {
                    std::ptr::write(
                        out_store as *mut crate::core::context::SurfaceStore,
                        crate::core::context::SurfaceStore::null(),
                    );
                }
            }
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                return;
            };
            if out_store.is_null() {
                return;
            }
            // `gpu.surface_store()` returns `Option<SurfaceStore>` —
            // a fresh β-shape with Arc refcount already bumped when
            // Some. We write it into the out-param; the caller (cdylib
            // or host) takes ownership.
            if let Some(store) = gpu.surface_store() {
                unsafe {
                    std::ptr::write(
                        out_store as *mut crate::core::context::SurfaceStore,
                        store,
                    );
                }
            }
            // else: out_store already holds the null-handle β-shape.
        },
        (),
    )
}

unsafe extern "C" fn host_gpu_lim_check_out_surface(
    gpu_handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_check_out_surface",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "check_out_surface: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "check_out_surface: null out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "check_out_surface: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match gpu.check_out_surface(id_str) {
                Ok(pb) => {
                    unsafe {
                        std::ptr::write(
                            out_pixel_buffer as *mut crate::core::rhi::PixelBuffer,
                            pb,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

// =========================================================================
// SurfaceStoreVTable — host-side callbacks
// =========================================================================
//
// Every callback derefs `handle` as `&SurfaceStoreInner` and calls
// the inner method directly. The Arc strong count keeps the inner
// alive for the duration of the dispatch.

#[inline]
unsafe fn ss_inner(handle: *const c_void) -> Option<&'static crate::core::context::surface_store::SurfaceStoreInner> {
    if handle.is_null() {
        None
    } else {
        // SAFETY: caller-supplied contract: `handle` is
        // `Arc::into_raw(Arc<SurfaceStoreInner>)`-shaped.
        Some(unsafe {
            &*(handle as *const crate::core::context::surface_store::SurfaceStoreInner)
        })
    }
}

unsafe extern "C" fn host_ss_clone_handle(handle: *const c_void) {
    run_host_extern_c(
        "host_ss_clone_handle",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::context::surface_store::SurfaceStoreInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_ss_drop_handle(handle: *const c_void) {
    run_host_extern_c(
        "host_ss_drop_handle",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::context::surface_store::SurfaceStoreInner,
                );
            }
        },
        (),
    )
}

unsafe extern "C" fn host_ss_connect(
    handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_connect",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err("connect: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            match inner.connect() {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_ss_disconnect(
    handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_disconnect",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err("disconnect: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            match inner.disconnect() {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_ss_check_in(
    handle: *const c_void,
    pixel_buffer: *const c_void,
    out_id_buf: *mut u8,
    out_id_cap: usize,
    out_id_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_check_in",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err("check_in: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if pixel_buffer.is_null() {
                write_err("check_in: null pixel_buffer", err_buf, err_buf_cap, err_len);
                return 1;
            }
            let pb = unsafe { &*(pixel_buffer as *const crate::core::rhi::PixelBuffer) };
            match inner.check_in(pb) {
                Ok(id) => {
                    let bytes = id.as_bytes();
                    write_id_bytes(bytes, out_id_buf, out_id_cap, out_id_len);
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_ss_check_out(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_check_out",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err("check_out: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "check_out: null out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "check_out: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match inner.check_out(id_str) {
                Ok(pb) => {
                    unsafe {
                        std::ptr::write(
                            out_pixel_buffer as *mut crate::core::rhi::PixelBuffer,
                            pb,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_ss_register_buffer(
    handle: *const c_void,
    pool_id_ptr: *const u8,
    pool_id_len: usize,
    pixel_buffer: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_register_buffer",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err(
                    "register_buffer: null handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer.is_null() {
                write_err(
                    "register_buffer: null pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let pb = unsafe { &*(pixel_buffer as *const crate::core::rhi::PixelBuffer) };
            let pool_id_bytes = unsafe { slice_from_raw(pool_id_ptr, pool_id_len) };
            let pool_id = match std::str::from_utf8(pool_id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "register_buffer: pool_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match inner.register_buffer(pool_id, pb) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_ss_lookup_buffer(
    handle: *const c_void,
    pool_id_ptr: *const u8,
    pool_id_len: usize,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_lookup_buffer",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err("lookup_buffer: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "lookup_buffer: null out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let pool_id_bytes = unsafe { slice_from_raw(pool_id_ptr, pool_id_len) };
            let pool_id = match std::str::from_utf8(pool_id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "lookup_buffer: pool_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match inner.lookup_buffer(pool_id) {
                Ok(pb) => {
                    unsafe {
                        std::ptr::write(
                            out_pixel_buffer as *mut crate::core::rhi::PixelBuffer,
                            pb,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_ss_release(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_release",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err("release: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "release: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match inner.release(id_str) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ss_register_texture(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    texture: *const c_void,
    timeline_handle: *const c_void,
    layout_raw: i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_register_texture",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err(
                    "register_texture: null handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if texture.is_null() {
                write_err(
                    "register_texture: null texture",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let tex = unsafe { &*(texture as *const crate::core::rhi::Texture) };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "register_texture: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            // SAFETY: timeline_handle, when non-null, points at the
            // engine-owned `Arc<HostVulkanTimelineSemaphore>` (passed
            // by `&Arc<...>` from engine code through `&*` cast).
            let timeline = unsafe {
                if timeline_handle.is_null() {
                    None
                } else {
                    Some(
                        &*(timeline_handle
                            as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore),
                    )
                }
            };
            let layout = streamlib_consumer_rhi::VulkanLayout(layout_raw);
            match inner.register_texture(id_str, tex, timeline, layout) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ss_register_texture(
    _handle: *const c_void,
    _id_ptr: *const u8,
    _id_len: usize,
    _texture: *const c_void,
    _timeline_handle: *const c_void,
    _layout_raw: i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "register_texture: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ss_register_pixel_buffer_with_timeline(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    pixel_buffer: *const c_void,
    timeline_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_register_pixel_buffer_with_timeline",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err(
                    "register_pixel_buffer_with_timeline: null handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer.is_null() {
                write_err(
                    "register_pixel_buffer_with_timeline: null pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let pb = unsafe { &*(pixel_buffer as *const crate::core::rhi::PixelBuffer) };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "register_pixel_buffer_with_timeline: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let timeline = unsafe {
                if timeline_handle.is_null() {
                    None
                } else {
                    Some(
                        &*(timeline_handle
                            as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore),
                    )
                }
            };
            match inner.register_pixel_buffer_with_timeline(id_str, pb, timeline) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ss_register_pixel_buffer_with_timeline(
    _handle: *const c_void,
    _id_ptr: *const u8,
    _id_len: usize,
    _pixel_buffer: *const c_void,
    _timeline_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "register_pixel_buffer_with_timeline: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ss_lookup_texture(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    out_texture: *mut c_void,
    out_layout_raw: *mut i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_lookup_texture",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err("lookup_texture: null handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if out_texture.is_null() || out_layout_raw.is_null() {
                write_err(
                    "lookup_texture: null out_texture or out_layout_raw",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "lookup_texture: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match inner.lookup_texture(id_str) {
                Ok((tex, layout)) => {
                    unsafe {
                        std::ptr::write(out_texture as *mut crate::core::rhi::Texture, tex);
                        *out_layout_raw = layout.0;
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ss_lookup_texture(
    _handle: *const c_void,
    _id_ptr: *const u8,
    _id_len: usize,
    _out_texture: *mut c_void,
    _out_layout_raw: *mut i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "lookup_texture: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ss_update_image_layout(
    handle: *const c_void,
    id_ptr: *const u8,
    id_len: usize,
    layout_raw: i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ss_update_image_layout",
        || -> i32 {
            let Some(inner) = (unsafe { ss_inner(handle) }) else {
                write_err(
                    "update_image_layout: null handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let id_bytes = unsafe { slice_from_raw(id_ptr, id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "update_image_layout: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let layout = streamlib_consumer_rhi::VulkanLayout(layout_raw);
            match inner.update_image_layout(id_str, layout) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ss_update_image_layout(
    _handle: *const c_void,
    _id_ptr: *const u8,
    _id_len: usize,
    _layout_raw: i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "update_image_layout: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

// -------------------------------------------------------------------------
// PixelBuffer acquire / get / resolve method-dispatch
// -------------------------------------------------------------------------

#[inline]
fn pixel_format_from_raw(raw: u32) -> Option<streamlib_consumer_rhi::PixelFormat> {
    // Mirror of `PixelBuffer::format`'s reverse mapping. Each
    // `#[repr(u32)]` discriminant maps back to its variant; unknown
    // values return None (caller surfaces an error).
    use streamlib_consumer_rhi::PixelFormat;
    match raw {
        0x42475241 => Some(PixelFormat::Bgra32),
        0x52474241 => Some(PixelFormat::Rgba32),
        0x00000020 => Some(PixelFormat::Argb32),
        0x52476841 => Some(PixelFormat::Rgba64),
        0x34323076 => Some(PixelFormat::Nv12VideoRange),
        0x34323066 => Some(PixelFormat::Nv12FullRange),
        0x32767579 => Some(PixelFormat::Uyvy422),
        0x79757673 => Some(PixelFormat::Yuyv422),
        0x4C303038 => Some(PixelFormat::Gray8),
        0x00000000 => Some(PixelFormat::Unknown),
        _ => None,
    }
}

unsafe extern "C" fn host_gpu_lim_acquire_pixel_buffer(
    gpu_handle: *const c_void,
    width: u32,
    height: u32,
    format_raw: u32,
    out_pool_id_buf: *mut u8,
    out_pool_id_cap: usize,
    out_pool_id_len: *mut usize,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_pixel_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "acquire_pixel_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "acquire_pixel_buffer: null out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let format = match pixel_format_from_raw(format_raw) {
                Some(f) => f,
                None => {
                    let msg = format!(
                        "acquire_pixel_buffer: invalid format_raw 0x{:08x}",
                        format_raw
                    );
                    write_err(&msg, err_buf, err_buf_cap, err_len);
                    return 1;
                }
            };
            match gpu.acquire_pixel_buffer(width, height, format) {
                Ok((pool_id, pb)) => {
                    write_id_bytes(
                        pool_id.as_str().as_bytes(),
                        out_pool_id_buf,
                        out_pool_id_cap,
                        out_pool_id_len,
                    );
                    unsafe {
                        std::ptr::write(
                            out_pixel_buffer as *mut crate::core::rhi::PixelBuffer,
                            pb,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_gpu_lim_get_pixel_buffer(
    gpu_handle: *const c_void,
    pool_id_ptr: *const u8,
    pool_id_len: usize,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_get_pixel_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "get_pixel_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "get_pixel_buffer: null out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(pool_id_ptr, pool_id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "get_pixel_buffer: pool_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let pool_id = crate::core::rhi::PixelBufferPoolId::from_str(id_str);
            match gpu.get_pixel_buffer(&pool_id) {
                Ok(pb) => {
                    unsafe {
                        std::ptr::write(
                            out_pixel_buffer as *mut crate::core::rhi::PixelBuffer,
                            pb,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_gpu_lim_resolve_pixel_buffer_by_surface_id(
    gpu_handle: *const c_void,
    surface_id_ptr: *const u8,
    surface_id_len: usize,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_resolve_pixel_buffer_by_surface_id",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "resolve_pixel_buffer_by_surface_id: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_pixel_buffer.is_null() {
                write_err(
                    "resolve_pixel_buffer_by_surface_id: null out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_bytes = unsafe { slice_from_raw(surface_id_ptr, surface_id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "resolve_pixel_buffer_by_surface_id: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match gpu.resolve_pixel_buffer_by_surface_id(id_str) {
                Ok(pb) => {
                    unsafe {
                        std::ptr::write(
                            out_pixel_buffer as *mut crate::core::rhi::PixelBuffer,
                            pb,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

/// Static [`SurfaceStoreVTable`] installed once per process. Paired
/// with the per-SurfaceStore handle returned by
/// [`HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE`]`::surface_store`.
pub static HOST_SURFACE_STORE_VTABLE: SurfaceStoreVTable = SurfaceStoreVTable {
    layout_version: SURFACE_STORE_VTABLE_LAYOUT_VERSION,
    _reserved_padding: 0,
    clone_handle: host_ss_clone_handle,
    drop_handle: host_ss_drop_handle,
    connect: host_ss_connect,
    disconnect: host_ss_disconnect,
    check_in: host_ss_check_in,
    check_out: host_ss_check_out,
    register_buffer: host_ss_register_buffer,
    lookup_buffer: host_ss_lookup_buffer,
    release: host_ss_release,
    register_texture: host_ss_register_texture,
    register_pixel_buffer_with_timeline: host_ss_register_pixel_buffer_with_timeline,
    lookup_texture: host_ss_lookup_texture,
    update_image_layout: host_ss_update_image_layout,
};

/// Pointer to the [`SurfaceStoreVTable`] this DSO should dispatch
/// through. Same DSO-routing rule as
/// [`host_gpu_context_limited_access_vtable`].
pub fn host_surface_store_vtable() -> *const SurfaceStoreVTable {
    match host_callbacks() {
        Some(c) if !c.surface_store_vtable.is_null() => c.surface_store_vtable,
        _ => &HOST_SURFACE_STORE_VTABLE,
    }
}

/// Static [`GpuContextLimitedAccessVTable`] installed once per process.
/// Paired with the per-RuntimeContext gpu-limited handle returned by
/// [`HOST_RUNTIME_CONTEXT_VTABLE`]`::gpu_limited_access`.
// =============================================================================
// HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE — Phase C2
// =============================================================================
//
// FullAccess vtable bodies. Reached from cdylib code via the
// vtable-dispatched path of `GpuContextLimitedAccess::escalate`; the
// `gpu_handle` slot on every method is an opaque scope token issued
// by the LimitedAccess vtable's `escalate_begin` callback (Phase C3).
// Each body resolves the token to its bound `Arc<GpuContext>` via
// `with_full_scope_or_err`; missing tokens return
// `Error::InvalidEscalateScope`. The engine-internal in-process path
// constructs `GpuContextFullAccess` via `Self::new(GpuContext)` and
// reaches the same engine methods through `host_inner` rather than
// the vtable, so these callback bodies don't ever see an
// engine-internal `Box<Arc<GpuContext>>`-shaped handle.
//
// Kernel return handles: `*const VulkanComputeKernel` / etc., shaped
// as `Arc::into_raw(arc)`. Cdylib's `clone_*` / `drop_*` callbacks
// route refcount accounting through host-compiled code.

/// Defensive no-op. `GpuContextFullAccess::Drop` dispatches on the
/// struct's `handle_kind` discriminator directly without routing
/// through this vtable slot — host-mode (Boxed) runs `Box::from_raw`
/// in-process; cdylib-mode (ScopeToken) is a no-op (the cdylib's
/// escalate wrapper releases the gate via the LimitedAccess vtable's
/// `escalate_end` callback). The slot is preserved at the same vtable
/// offset for layout-version stability; calling it has no effect.
unsafe extern "C" fn host_gpu_full_drop_handle(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_handle",
        || {
            let _ = handle;
        },
        (),
    )
}

// ---------------- Kernel Arc-handle lifecycle (Linux-only) ----------------

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_clone_compute_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_compute_kernel",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: handle is `Arc::into_raw(Arc<VulkanComputeKernel>)`-shaped.
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanComputeKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_drop_compute_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_compute_kernel",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: handle is `Arc::into_raw(Arc<VulkanComputeKernel>)`-shaped.
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanComputeKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_clone_graphics_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_graphics_kernel",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanGraphicsKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_drop_graphics_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_graphics_kernel",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanGraphicsKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_clone_ray_tracing_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_ray_tracing_kernel",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanRayTracingKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_drop_ray_tracing_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_ray_tracing_kernel",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanRayTracingKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_clone_texture_ring(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_texture_ring",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::context::TextureRingInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_drop_texture_ring(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_texture_ring",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::context::TextureRingInner,
                );
            }
        },
        (),
    )
}

// β-shape v4 (#917) lifecycle callbacks. The handle is
// `Arc::into_raw(Arc<<Type>Inner>)`-shaped on the host side; cdylib
// code never sees the Inner layout, only the opaque handle paired
// with its β-shape vtable. Increment/decrement runs in host-compiled
// code where the Inner layout is known statically.

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_clone_color_converter(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_color_converter",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::rhi::RhiColorConverterInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_drop_color_converter(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_color_converter",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::rhi::RhiColorConverterInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_clone_acceleration_structure(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_acceleration_structure",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle
                        as *const crate::vulkan::rhi::VulkanAccelerationStructureInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_drop_acceleration_structure(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_acceleration_structure",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle
                        as *const crate::vulkan::rhi::VulkanAccelerationStructureInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_clone_command_recorder(_handle: *const c_void) {
    // RhiCommandRecorder is Box-shaped (single-owner) — deliberately
    // NOT Clone per CommandBuffer precedent. This slot is reserved
    // infrastructure; the type-level absence of `Clone` for
    // `RhiCommandRecorder` ensures the host callback is never invoked
    // from typesafe code. If reached, it's a bug somewhere.
    run_host_extern_c(
        "host_gpu_full_clone_command_recorder",
        || {
            tracing::error!(
                "host_gpu_full_clone_command_recorder invoked — RhiCommandRecorder is \
                 not Clone-able (Box-shaped, single-owner). This is a bug."
            );
        },
        (),
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_drop_command_recorder(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_command_recorder",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: handle is `Box::into_raw(Box<RhiCommandRecorderInner>)`-shaped.
            // Reconstruct the Box and let Drop run.
            unsafe {
                let _ = Box::from_raw(
                    handle as *mut crate::vulkan::rhi::RhiCommandRecorderInner,
                );
            }
        },
        (),
    )
}

// Non-Linux stubs (callbacks must exist for the static layout, but
// the kernel types only ship on Linux).
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_clone_compute_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_drop_compute_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_clone_graphics_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_drop_graphics_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_clone_ray_tracing_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_drop_ray_tracing_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_clone_texture_ring(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_drop_texture_ring(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_clone_color_converter(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_drop_color_converter(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_clone_acceleration_structure(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_drop_acceleration_structure(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_clone_command_recorder(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_drop_command_recorder(_handle: *const c_void) {}

// ---------------- Kernel construction (Linux-only) ----------------

/// Resolve a scope token to its bound `Arc<GpuContext>` and run the
/// closure. On miss (null token, stale token, never-issued token)
/// writes an "invalid escalate scope" message into `err_buf` and
/// returns `None`. FullAccess vtable callback bodies use this in
/// place of [`handle_as_gpu_context_full`] (which derefs a host-mode
/// `Box<Arc<GpuContext>>` directly — never reached from cdylib code
/// post-C3, kept for tier-1 wire-format tests until they migrate).
fn with_full_scope_or_err<F, R>(
    scope_token: *const c_void,
    op: &str,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
    f: F,
) -> Option<R>
where
    F: FnOnce(&Arc<crate::core::context::GpuContext>) -> R,
{
    let token = scope_token as u64;
    match crate::core::context::escalate_scope_registry::with_scope(token, f) {
        Some(r) => Some(r),
        None => {
            write_err(
                &format!(
                    "{op}: invalid escalate scope (token stale, never-issued, \
                     or null)"
                ),
                err_buf,
                err_buf_cap,
                err_len,
            );
            None
        }
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_create_compute_kernel(
    scope_token: *const c_void,
    desc: *const ComputeKernelDescriptorRepr,
    out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_compute_kernel",
        || -> i32 {
            if desc.is_null() || out_kernel.is_null() {
                write_err(
                    "create_compute_kernel: null desc or out pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let repr: &ComputeKernelDescriptorRepr = unsafe { &*desc };
            let result = with_full_scope_or_err(
                scope_token,
                "create_compute_kernel",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| unsafe {
                    crate::core::rhi::plugin_abi_bridge::with_decoded_compute_kernel_descriptor(
                        repr,
                        |rust_desc| gpu.create_compute_kernel(rust_desc),
                    )
                },
            );
            match result {
                Some(Ok(kernel)) => {
                    // `kernel` is the β-shape; its `handle` is the
                    // `Arc::into_raw(Arc<<Type>Inner>)` raw pointer
                    // already. Forget the β-shape so the strong ref
                    // transfers to cdylib; the cdylib reconstructs its
                    // own β-shape from { handle: raw, vtable } and
                    // never sees the `Arc<X>` internal layout.
                    let raw = kernel.handle;
                    std::mem::forget(kernel);
                    unsafe { std::ptr::write(out_kernel, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1, // err_buf populated by helper
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_create_graphics_kernel(
    scope_token: *const c_void,
    desc: *const GraphicsKernelDescriptorRepr,
    out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_graphics_kernel",
        || -> i32 {
            if desc.is_null() || out_kernel.is_null() {
                write_err(
                    "create_graphics_kernel: null desc or out pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let repr: &GraphicsKernelDescriptorRepr = unsafe { &*desc };
            let result = with_full_scope_or_err(
                scope_token,
                "create_graphics_kernel",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| unsafe {
                    crate::core::rhi::plugin_abi_bridge::with_decoded_graphics_kernel_descriptor(
                        repr,
                        |rust_desc| gpu.create_graphics_kernel(rust_desc),
                    )
                },
            );
            match result {
                Some(Ok(kernel)) => {
                    // β-shape: extract the opaque handle (which is
                    // already `Arc::into_raw(Arc<<Type>Inner>)`-shaped)
                    // and `mem::forget` the wrapper so the strong ref
                    // transfers to cdylib. The cdylib reconstructs a
                    // fresh β-shape from { handle, vtable } and never
                    // sees the host's `Arc<X>` allocation header.
                    let raw = kernel.handle;
                    std::mem::forget(kernel);
                    unsafe { std::ptr::write(out_kernel, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_create_ray_tracing_kernel(
    gpu_handle: *const c_void,
    desc: *const RayTracingKernelDescriptorRepr,
    out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_ray_tracing_kernel",
        || -> i32 {
            if desc.is_null() || out_kernel.is_null() {
                write_err(
                    "create_ray_tracing_kernel: null desc or out pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let repr: &RayTracingKernelDescriptorRepr = unsafe { &*desc };
            let result = with_full_scope_or_err(
                gpu_handle,
                "create_ray_tracing_kernel",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| unsafe {
                    crate::core::rhi::plugin_abi_bridge::with_decoded_ray_tracing_kernel_descriptor(
                        repr,
                        |rust_desc| gpu.create_ray_tracing_kernel(rust_desc),
                    )
                },
            );
            match result {
                Some(Ok(kernel)) => {
                    // β-shape: extract the opaque handle (which is
                    // already `Arc::into_raw(Arc<<Type>Inner>)`-shaped)
                    // and `mem::forget` the wrapper so the strong ref
                    // transfers to cdylib. The cdylib reconstructs a
                    // fresh β-shape from { handle, vtable } and never
                    // sees the host's `Arc<X>` allocation header.
                    let raw = kernel.handle;
                    std::mem::forget(kernel);
                    unsafe { std::ptr::write(out_kernel, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_create_texture_ring(
    scope_token: *const c_void,
    width: u32,
    height: u32,
    format_raw: u32,
    usage_bits: u32,
    count: usize,
    out_ring: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_texture_ring",
        || -> i32 {
            if out_ring.is_null() {
                write_err(
                    "create_texture_ring: null out_ring pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let format = match format_raw {
                0 => streamlib_consumer_rhi::TextureFormat::Rgba8Unorm,
                1 => streamlib_consumer_rhi::TextureFormat::Rgba8UnormSrgb,
                2 => streamlib_consumer_rhi::TextureFormat::Bgra8Unorm,
                3 => streamlib_consumer_rhi::TextureFormat::Bgra8UnormSrgb,
                4 => streamlib_consumer_rhi::TextureFormat::Rgba16Float,
                5 => streamlib_consumer_rhi::TextureFormat::Rgba32Float,
                6 => streamlib_consumer_rhi::TextureFormat::Nv12,
                _ => {
                    write_err(
                        &format!("create_texture_ring: invalid format_raw {format_raw}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let usages =
                streamlib_consumer_rhi::TextureUsages::from_bits_truncate(usage_bits);
            let result = with_full_scope_or_err(
                scope_token,
                "create_texture_ring",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.create_texture_ring(width, height, format, usages, count),
            );
            match result {
                Some(Ok(ring)) => {
                    // `ring` is the β-shape; its handle is
                    // `Arc::into_raw(Arc<TextureRingInner>)`-shaped.
                    let raw = ring.handle;
                    std::mem::forget(ring);
                    unsafe { std::ptr::write(out_ring, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

// Non-Linux stubs for the create_* callbacks.
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_create_compute_kernel(
    _gpu_handle: *const c_void,
    _desc: *const ComputeKernelDescriptorRepr,
    _out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_compute_kernel: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_create_graphics_kernel(
    _gpu_handle: *const c_void,
    _desc: *const GraphicsKernelDescriptorRepr,
    _out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_graphics_kernel: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_create_ray_tracing_kernel(
    _gpu_handle: *const c_void,
    _desc: *const RayTracingKernelDescriptorRepr,
    _out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_ray_tracing_kernel: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}
#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_create_texture_ring(
    _gpu_handle: *const c_void,
    _width: u32,
    _height: u32,
    _format_raw: u32,
    _usage_bits: u32,
    _count: usize,
    _out_ring: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_texture_ring: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

// ---------------- Render-target allocation (Phase C3, Linux-only) ---------

/// Allocate a render-target-capable DMA-BUF-backed `VkImage`. Looks
/// up the bound `Arc<GpuContext>` via the scope_token; runs
/// [`crate::core::context::GpuContext::acquire_render_target_dma_buf_image`]
/// (which picks a tiled DRM modifier via the EGL probe and allocates
/// through the privileged RHI path), and writes the resulting
/// `Texture` β-shape into `*out_texture` on success.
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_acquire_render_target_dma_buf_image(
    scope_token: *const c_void,
    width: u32,
    height: u32,
    format_raw: u32,
    out_texture: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_acquire_render_target_dma_buf_image",
        || -> i32 {
            if out_texture.is_null() {
                write_err(
                    "acquire_render_target_dma_buf_image: null out_texture",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let format = match format_raw {
                0 => streamlib_consumer_rhi::TextureFormat::Rgba8Unorm,
                1 => streamlib_consumer_rhi::TextureFormat::Rgba8UnormSrgb,
                2 => streamlib_consumer_rhi::TextureFormat::Bgra8Unorm,
                3 => streamlib_consumer_rhi::TextureFormat::Bgra8UnormSrgb,
                4 => streamlib_consumer_rhi::TextureFormat::Rgba16Float,
                5 => streamlib_consumer_rhi::TextureFormat::Rgba32Float,
                6 => streamlib_consumer_rhi::TextureFormat::Nv12,
                _ => {
                    write_err(
                        &format!(
                            "acquire_render_target_dma_buf_image: invalid \
                             format_raw {}",
                            format_raw
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "acquire_render_target_dma_buf_image",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.acquire_render_target_dma_buf_image(width, height, format),
            );
            match result {
                Some(Ok(texture)) => {
                    unsafe {
                        std::ptr::write(
                            out_texture as *mut crate::core::rhi::Texture,
                            texture,
                        );
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1, // err_buf already populated by helper
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_acquire_render_target_dma_buf_image(
    _scope_token: *const c_void,
    _width: u32,
    _height: u32,
    _format_raw: u32,
    _out_texture: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_render_target_dma_buf_image: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

// ============================================================================
// Phase D (#906) — privileged-only FullAccess host callbacks.
// Each callback validates the `scope_token` via `with_full_scope_or_err`
// (resolving the bound `Arc<GpuContext>` from the escalate-scope registry)
// before dispatching to the resolved context.
// ============================================================================

unsafe extern "C" fn host_gpu_full_wait_device_idle(
    scope_token: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_wait_device_idle",
        || -> i32 {
            let result = with_full_scope_or_err(
                scope_token,
                "wait_device_idle",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.wait_device_idle(),
            );
            match result {
                Some(Ok(())) => 0,
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

unsafe extern "C" fn host_gpu_full_acquire_output_texture(
    scope_token: *const c_void,
    width: u32,
    height: u32,
    format_raw: u32,
    out_id_buf: *mut u8,
    out_id_cap: usize,
    out_id_len: *mut usize,
    out_texture: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_acquire_output_texture",
        || -> i32 {
            if out_texture.is_null() || out_id_buf.is_null() || out_id_len.is_null() {
                write_err(
                    "acquire_output_texture: null out_texture / out_id_buf / out_id_len",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let format = match format_raw {
                0 => streamlib_consumer_rhi::TextureFormat::Rgba8Unorm,
                1 => streamlib_consumer_rhi::TextureFormat::Rgba8UnormSrgb,
                2 => streamlib_consumer_rhi::TextureFormat::Bgra8Unorm,
                3 => streamlib_consumer_rhi::TextureFormat::Bgra8UnormSrgb,
                4 => streamlib_consumer_rhi::TextureFormat::Rgba16Float,
                5 => streamlib_consumer_rhi::TextureFormat::Rgba32Float,
                6 => streamlib_consumer_rhi::TextureFormat::Nv12,
                _ => {
                    write_err(
                        &format!(
                            "acquire_output_texture: invalid format_raw {}",
                            format_raw
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "acquire_output_texture",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.acquire_output_texture(width, height, format),
            );
            match result {
                Some(Ok((id, texture))) => {
                    let id_bytes = id.as_bytes();
                    if id_bytes.len() > out_id_cap {
                        write_err(
                            "acquire_output_texture: surface id buffer too small",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        return 1;
                    }
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            id_bytes.as_ptr(),
                            out_id_buf,
                            id_bytes.len(),
                        );
                        std::ptr::write(out_id_len, id_bytes.len());
                        std::ptr::write(
                            out_texture as *mut crate::core::rhi::Texture,
                            texture,
                        );
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_upload_pixel_buffer_as_texture(
    scope_token: *const c_void,
    surface_id_ptr: *const u8,
    surface_id_len: usize,
    pixel_buffer: *const c_void,
    width: u32,
    height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_upload_pixel_buffer_as_texture",
        || -> i32 {
            if surface_id_ptr.is_null() || pixel_buffer.is_null() {
                write_err(
                    "upload_pixel_buffer_as_texture: null surface_id / pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_slice =
                unsafe { std::slice::from_raw_parts(surface_id_ptr, surface_id_len) };
            let surface_id = match std::str::from_utf8(id_slice) {
                Ok(s) => s,
                Err(e) => {
                    write_err(
                        &format!(
                            "upload_pixel_buffer_as_texture: surface_id not UTF-8: {e}"
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            // SAFETY: pixel_buffer is a borrowed `*const PixelBuffer`
            // pointer from the cdylib; valid for the duration of the call.
            let pb = unsafe { &*(pixel_buffer as *const crate::core::rhi::PixelBuffer) };
            let result = with_full_scope_or_err(
                scope_token,
                "upload_pixel_buffer_as_texture",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.upload_pixel_buffer_as_texture(surface_id, pb, width, height),
            );
            match result {
                Some(Ok(())) => 0,
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_upload_pixel_buffer_as_texture(
    _scope_token: *const c_void,
    _surface_id_ptr: *const u8,
    _surface_id_len: usize,
    _pixel_buffer: *const c_void,
    _width: u32,
    _height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "upload_pixel_buffer_as_texture: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_color_converter(
    scope_token: *const c_void,
    src_format_raw: u32,
    dst_format_raw: u32,
    out_converter: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_color_converter",
        || -> i32 {
            if out_converter.is_null() {
                write_err(
                    "color_converter: null out_converter",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let src = match pixel_format_from_raw(src_format_raw) {
                Some(f) => f,
                None => {
                    write_err(
                        &format!(
                            "color_converter: invalid src_format_raw {}",
                            src_format_raw
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let dst = match pixel_format_from_raw(dst_format_raw) {
                Some(f) => f,
                None => {
                    write_err(
                        &format!(
                            "color_converter: invalid dst_format_raw {}",
                            dst_format_raw
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "color_converter",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.color_converter(src, dst),
            );
            match result {
                Some(Ok(converter)) => {
                    // `converter` is the β-shape; its `handle` is the
                    // `Arc::into_raw(Arc<RhiColorConverterInner>)` pointer.
                    let raw = converter.handle;
                    std::mem::forget(converter);
                    unsafe { std::ptr::write(out_converter, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_color_converter(
    _scope_token: *const c_void,
    _src_format_raw: u32,
    _dst_format_raw: u32,
    _out_converter: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "color_converter: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_create_command_recorder(
    scope_token: *const c_void,
    label_ptr: *const u8,
    label_len: usize,
    out_recorder: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_command_recorder",
        || -> i32 {
            if out_recorder.is_null() || label_ptr.is_null() {
                write_err(
                    "create_command_recorder: null label_ptr / out_recorder",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let label_slice =
                unsafe { std::slice::from_raw_parts(label_ptr, label_len) };
            let label = match std::str::from_utf8(label_slice) {
                Ok(s) => s,
                Err(e) => {
                    write_err(
                        &format!("create_command_recorder: label not UTF-8: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "create_command_recorder",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.create_command_recorder(label),
            );
            match result {
                Some(Ok(recorder)) => {
                    // SAFETY: `recorder` is the β-shape — a
                    // `#[repr(C)] { handle: *const c_void, vtable: *const VTable }`
                    // 16-byte POD. Layout is byte-identical
                    // by `#[repr(C)]` invariant, not by rustc-version
                    // coupling. The cdylib reads the bits via
                    // `MaybeUninit::assume_init`; its `Drop` later
                    // dispatches through the vtable's
                    // `drop_command_recorder` slot which runs
                    // `Box::from_raw + drop` host-side.
                    unsafe {
                        std::ptr::write(
                            out_recorder as *mut crate::vulkan::rhi::RhiCommandRecorder,
                            recorder,
                        );
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_create_command_recorder(
    _scope_token: *const c_void,
    _label_ptr: *const u8,
    _label_len: usize,
    _out_recorder: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_command_recorder: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_build_triangles_blas(
    scope_token: *const c_void,
    label_ptr: *const u8,
    label_len: usize,
    vertices_ptr: *const f32,
    vertices_len: usize,
    indices_ptr: *const u32,
    indices_len: usize,
    out_blas: *mut *const c_void,
    out_device_address: *mut u64,
    out_storage_size: *mut u64,
    out_kind: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_build_triangles_blas",
        || -> i32 {
            if out_blas.is_null()
                || label_ptr.is_null()
                || out_device_address.is_null()
                || out_storage_size.is_null()
                || out_kind.is_null()
            {
                write_err(
                    "build_triangles_blas: null label_ptr / out-parameter pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let label_slice =
                unsafe { std::slice::from_raw_parts(label_ptr, label_len) };
            let label = match std::str::from_utf8(label_slice) {
                Ok(s) => s,
                Err(e) => {
                    write_err(
                        &format!("build_triangles_blas: label not UTF-8: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let vertices: &[f32] = if vertices_len == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(vertices_ptr, vertices_len) }
            };
            let indices: &[u32] = if indices_len == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(indices_ptr, indices_len) }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "build_triangles_blas",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.build_triangles_blas(label, vertices, indices),
            );
            match result {
                Some(Ok(blas)) => {
                    // `blas` is the β-shape — its `handle` is already
                    // `Arc::into_raw(Arc<VulkanAccelerationStructureInner>)`-shaped
                    // and its cached POD fields were populated by
                    // `VulkanAccelerationStructure::from_arc_into_raw`
                    // (host-mode mint path). Write them through the
                    // out-params so the cdylib's β-shape carries the
                    // real values instead of placeholder zeros. Forget
                    // the β-shape to keep the Arc strong count bumped;
                    // cdylib reconstructs its own β-shape from the
                    // handle + vtable + cached PODs.
                    let raw = blas.handle;
                    let device_address = blas.cached_device_address;
                    let storage_size = blas.cached_storage_size;
                    let kind = blas.cached_kind;
                    std::mem::forget(blas);
                    unsafe {
                        std::ptr::write(out_blas, raw);
                        std::ptr::write(out_device_address, device_address);
                        std::ptr::write(out_storage_size, storage_size);
                        std::ptr::write(out_kind, kind);
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_build_triangles_blas(
    _scope_token: *const c_void,
    _label_ptr: *const u8,
    _label_len: usize,
    _vertices_ptr: *const f32,
    _vertices_len: usize,
    _indices_ptr: *const u32,
    _indices_len: usize,
    _out_blas: *mut *const c_void,
    _out_device_address: *mut u64,
    _out_storage_size: *mut u64,
    _out_kind: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "build_triangles_blas: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_build_tlas(
    scope_token: *const c_void,
    label_ptr: *const u8,
    label_len: usize,
    instances_ptr: *const c_void,
    instances_len: usize,
    out_tlas: *mut *const c_void,
    out_device_address: *mut u64,
    out_storage_size: *mut u64,
    out_kind: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_build_tlas",
        || -> i32 {
            if out_tlas.is_null()
                || label_ptr.is_null()
                || out_device_address.is_null()
                || out_storage_size.is_null()
                || out_kind.is_null()
            {
                write_err(
                    "build_tlas: null label_ptr / out-parameter pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let label_slice =
                unsafe { std::slice::from_raw_parts(label_ptr, label_len) };
            let label = match std::str::from_utf8(label_slice) {
                Ok(s) => s,
                Err(e) => {
                    write_err(
                        &format!("build_tlas: label not UTF-8: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let instances: &[crate::vulkan::rhi::TlasInstanceDesc] = if instances_len
                == 0
            {
                &[]
            } else {
                // SAFETY: `instances_ptr` is `*const TlasInstanceDesc`
                // from the cdylib; layout is byte-identical under
                // rustc-version coupling. The slice is borrowed for
                // the call's duration.
                unsafe {
                    std::slice::from_raw_parts(
                        instances_ptr as *const crate::vulkan::rhi::TlasInstanceDesc,
                        instances_len,
                    )
                }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "build_tlas",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.build_tlas(label, instances),
            );
            match result {
                Some(Ok(tlas)) => {
                    // Same shape as `host_gpu_full_build_triangles_blas`:
                    // the β-shape's cached PODs are real (populated by
                    // `from_arc_into_raw` host-side); write them
                    // through the out-params so the cdylib's reassembled
                    // β-shape carries real values.
                    let raw = tlas.handle;
                    let device_address = tlas.cached_device_address;
                    let storage_size = tlas.cached_storage_size;
                    let kind = tlas.cached_kind;
                    std::mem::forget(tlas);
                    unsafe {
                        std::ptr::write(out_tlas, raw);
                        std::ptr::write(out_device_address, device_address);
                        std::ptr::write(out_storage_size, storage_size);
                        std::ptr::write(out_kind, kind);
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_build_tlas(
    _scope_token: *const c_void,
    _label_ptr: *const u8,
    _label_len: usize,
    _instances_ptr: *const c_void,
    _instances_len: usize,
    _out_tlas: *mut *const c_void,
    _out_device_address: *mut u64,
    _out_storage_size: *mut u64,
    _out_kind: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "build_tlas: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_supports_ray_tracing_pipeline(
    scope_token: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_supports_ray_tracing_pipeline",
        || -> i32 {
            let result = with_full_scope_or_err(
                scope_token,
                "supports_ray_tracing_pipeline",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| Ok::<bool, crate::core::Error>(gpu.supports_ray_tracing_pipeline()),
            );
            match result {
                Some(Ok(true)) => 1,
                Some(Ok(false)) => 0,
                Some(Err(_)) | None => -1,
            }
        },
        -1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_supports_ray_tracing_pipeline(
    _scope_token: *const c_void,
    _err_buf: *mut u8,
    _err_buf_cap: usize,
    _err_len: *mut usize,
) -> i32 {
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_gpu_capabilities(
    scope_token: *const c_void,
    out_caps: *mut streamlib_plugin_abi::GpuCapabilitiesRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_gpu_capabilities",
        || -> i32 {
            if out_caps.is_null() {
                write_err(
                    "gpu_capabilities: null out_caps pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let result = with_full_scope_or_err(
                scope_token,
                "gpu_capabilities",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| Ok::<_, crate::core::Error>(gpu.gpu_capabilities()),
            );
            match result {
                Some(Ok(snapshot)) => {
                    let mut repr = streamlib_plugin_abi::GpuCapabilitiesRepr {
                        device_name: [0u8; 256],
                        device_name_len: 0,
                        supports_external_memory: u8::from(
                            snapshot.supports_external_memory,
                        ),
                        supports_cross_device_dma_buf_probe: u8::from(
                            snapshot.supports_cross_device_dma_buf_probe,
                        ),
                        supports_ray_tracing_pipeline: u8::from(
                            snapshot.supports_ray_tracing_pipeline,
                        ),
                        _reserved_padding: 0,
                    };
                    let bytes = snapshot.device_name.as_bytes();
                    let n = bytes.len().min(repr.device_name.len());
                    repr.device_name[..n].copy_from_slice(&bytes[..n]);
                    repr.device_name_len = n as u32;
                    unsafe { std::ptr::write(out_caps, repr) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_gpu_capabilities(
    _scope_token: *const c_void,
    _out_caps: *mut streamlib_plugin_abi::GpuCapabilitiesRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "gpu_capabilities: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_create_timeline_semaphore(
    scope_token: *const c_void,
    initial_value: u64,
    out_handle: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_timeline_semaphore",
        || -> i32 {
            if out_handle.is_null() {
                write_err(
                    "create_timeline_semaphore: null out_handle pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let result = with_full_scope_or_err(
                scope_token,
                "create_timeline_semaphore",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.create_timeline_semaphore(initial_value),
            );
            match result {
                Some(Ok(arc)) => {
                    let raw = Arc::into_raw(arc) as *const c_void;
                    unsafe { std::ptr::write(out_handle, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_create_timeline_semaphore(
    _scope_token: *const c_void,
    _initial_value: u64,
    _out_handle: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_timeline_semaphore: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_gpu_full_import_dma_buf_storage_buffer(
    scope_token: *const c_void,
    fd: i32,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_import_dma_buf_storage_buffer",
        || -> i32 {
            if out_buffer.is_null() {
                write_err(
                    "import_dma_buf_storage_buffer: null out_buffer pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let result = with_full_scope_or_err(
                scope_token,
                "import_dma_buf_storage_buffer",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.import_dma_buf_storage_buffer(fd, byte_size),
            );
            match result {
                Some(Ok(buf)) => {
                    unsafe {
                        std::ptr::write(
                            out_buffer as *mut crate::core::rhi::StorageBuffer,
                            buf,
                        );
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_gpu_full_import_dma_buf_storage_buffer(
    _scope_token: *const c_void,
    _fd: i32,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "import_dma_buf_storage_buffer: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

unsafe extern "C" fn host_gpu_full_check_in_surface(
    scope_token: *const c_void,
    pixel_buffer: *const c_void,
    out_id_buf: *mut u8,
    out_id_cap: usize,
    out_id_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_check_in_surface",
        || -> i32 {
            if pixel_buffer.is_null() || out_id_buf.is_null() || out_id_len.is_null() {
                write_err(
                    "check_in_surface: null pixel_buffer / out_id_buf / out_id_len",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // SAFETY: pixel_buffer is borrowed from the cdylib for the
            // duration of the call.
            let pb = unsafe { &*(pixel_buffer as *const crate::core::rhi::PixelBuffer) };
            let result = with_full_scope_or_err(
                scope_token,
                "check_in_surface",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.check_in_surface(pb),
            );
            match result {
                Some(Ok(id)) => {
                    let id_bytes = id.as_bytes();
                    if id_bytes.len() > out_id_cap {
                        write_err(
                            "check_in_surface: surface id buffer too small",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        return 1;
                    }
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            id_bytes.as_ptr(),
                            out_id_buf,
                            id_bytes.len(),
                        );
                        std::ptr::write(out_id_len, id_bytes.len());
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

pub static HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE: GpuContextFullAccessVTable =
    GpuContextFullAccessVTable {
        layout_version: GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        drop_handle: host_gpu_full_drop_handle,
        clone_compute_kernel: host_gpu_full_clone_compute_kernel,
        drop_compute_kernel: host_gpu_full_drop_compute_kernel,
        clone_graphics_kernel: host_gpu_full_clone_graphics_kernel,
        drop_graphics_kernel: host_gpu_full_drop_graphics_kernel,
        clone_ray_tracing_kernel: host_gpu_full_clone_ray_tracing_kernel,
        drop_ray_tracing_kernel: host_gpu_full_drop_ray_tracing_kernel,
        clone_texture_ring: host_gpu_full_clone_texture_ring,
        drop_texture_ring: host_gpu_full_drop_texture_ring,
        // v4 β-shape lifecycle slots (#917).
        clone_color_converter: host_gpu_full_clone_color_converter,
        drop_color_converter: host_gpu_full_drop_color_converter,
        clone_acceleration_structure: host_gpu_full_clone_acceleration_structure,
        drop_acceleration_structure: host_gpu_full_drop_acceleration_structure,
        clone_command_recorder: host_gpu_full_clone_command_recorder,
        drop_command_recorder: host_gpu_full_drop_command_recorder,
        create_compute_kernel: host_gpu_full_create_compute_kernel,
        create_graphics_kernel: host_gpu_full_create_graphics_kernel,
        create_ray_tracing_kernel: host_gpu_full_create_ray_tracing_kernel,
        create_texture_ring: host_gpu_full_create_texture_ring,
        acquire_render_target_dma_buf_image:
            host_gpu_full_acquire_render_target_dma_buf_image,
        // Phase D (#906) entries.
        wait_device_idle: host_gpu_full_wait_device_idle,
        acquire_output_texture: host_gpu_full_acquire_output_texture,
        upload_pixel_buffer_as_texture: host_gpu_full_upload_pixel_buffer_as_texture,
        color_converter: host_gpu_full_color_converter,
        create_command_recorder: host_gpu_full_create_command_recorder,
        build_triangles_blas: host_gpu_full_build_triangles_blas,
        build_tlas: host_gpu_full_build_tlas,
        supports_ray_tracing_pipeline: host_gpu_full_supports_ray_tracing_pipeline,
        check_in_surface: host_gpu_full_check_in_surface,
        gpu_capabilities: host_gpu_full_gpu_capabilities,
        create_timeline_semaphore: host_gpu_full_create_timeline_semaphore,
        import_dma_buf_storage_buffer: host_gpu_full_import_dma_buf_storage_buffer,
    };

/// Pointer to the [`GpuContextFullAccessVTable`] this DSO should
/// dispatch through. Same DSO-routing rule as
/// [`host_gpu_context_limited_access_vtable`]: host mode resolves to
/// the local `&HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE` static, cdylib
/// mode resolves to the host-installed pointer cached on
/// [`HostServices::gpu_context_full_access_vtable`].
pub fn host_gpu_context_full_access_vtable() -> *const GpuContextFullAccessVTable {
    match host_callbacks() {
        Some(c) if !c.gpu_context_full_access_vtable.is_null() => {
            c.gpu_context_full_access_vtable
        }
        _ => &HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE,
    }
}

pub static HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE: GpuContextLimitedAccessVTable =
    GpuContextLimitedAccessVTable {
        layout_version: GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        clone_handle: host_gpu_lim_clone_handle,
        drop_handle: host_gpu_lim_drop_handle,
        clone_pixel_buffer: host_gpu_lim_clone_pixel_buffer,
        drop_pixel_buffer: host_gpu_lim_drop_pixel_buffer,
        strong_count_pixel_buffer: host_gpu_lim_strong_count_pixel_buffer,
        plane_base_address_pixel_buffer: host_gpu_lim_plane_base_address_pixel_buffer,
        plane_size_pixel_buffer: host_gpu_lim_plane_size_pixel_buffer,
        clone_texture: host_gpu_lim_clone_texture,
        drop_texture: host_gpu_lim_drop_texture,
        drop_pooled_texture_handle: host_gpu_lim_drop_pooled_texture_handle,
        register_texture: host_gpu_lim_register_texture,
        update_texture_registration_layout: host_gpu_lim_update_texture_registration_layout,
        acquire_texture: host_gpu_lim_acquire_texture,
        resolve_texture_by_surface_id: host_gpu_lim_resolve_texture_by_surface_id,
        unregister_texture: host_gpu_lim_unregister_texture,
        clone_storage_buffer: host_gpu_lim_clone_storage_buffer,
        drop_storage_buffer: host_gpu_lim_drop_storage_buffer,
        clone_uniform_buffer: host_gpu_lim_clone_uniform_buffer,
        drop_uniform_buffer: host_gpu_lim_drop_uniform_buffer,
        clone_vertex_buffer: host_gpu_lim_clone_vertex_buffer,
        drop_vertex_buffer: host_gpu_lim_drop_vertex_buffer,
        clone_index_buffer: host_gpu_lim_clone_index_buffer,
        drop_index_buffer: host_gpu_lim_drop_index_buffer,
        acquire_storage_buffer: host_gpu_lim_acquire_storage_buffer,
        acquire_uniform_buffer: host_gpu_lim_acquire_uniform_buffer,
        acquire_vertex_buffer: host_gpu_lim_acquire_vertex_buffer,
        acquire_index_buffer: host_gpu_lim_acquire_index_buffer,
        clone_texture_registration: host_gpu_lim_clone_texture_registration,
        drop_texture_registration: host_gpu_lim_drop_texture_registration,
        texture_registration_texture: host_gpu_lim_texture_registration_texture,
        texture_registration_current_layout: host_gpu_lim_texture_registration_current_layout,
        texture_registration_update_layout: host_gpu_lim_texture_registration_update_layout,
        resolve_texture_registration_by_surface_id:
            host_gpu_lim_resolve_texture_registration_by_surface_id,
        clone_rhi_command_queue: host_gpu_lim_clone_rhi_command_queue,
        drop_rhi_command_queue: host_gpu_lim_drop_rhi_command_queue,
        create_command_buffer_from_queue: host_gpu_lim_create_command_buffer_from_queue,
        drop_command_buffer: host_gpu_lim_drop_command_buffer,
        commit_command_buffer: host_gpu_lim_commit_command_buffer,
        commit_and_wait_command_buffer: host_gpu_lim_commit_and_wait_command_buffer,
        copy_texture_command_buffer: host_gpu_lim_copy_texture_command_buffer,
        command_queue: host_gpu_lim_command_queue,
        create_command_buffer: host_gpu_lim_create_command_buffer,
        copy_pixel_buffer_to_texture: host_gpu_lim_copy_pixel_buffer_to_texture,
        blit_copy: host_gpu_lim_blit_copy,
        blit_copy_iosurface: host_gpu_lim_blit_copy_iosurface,
        surface_store: host_gpu_lim_surface_store,
        check_out_surface: host_gpu_lim_check_out_surface,
        acquire_pixel_buffer: host_gpu_lim_acquire_pixel_buffer,
        get_pixel_buffer: host_gpu_lim_get_pixel_buffer,
        resolve_pixel_buffer_by_surface_id: host_gpu_lim_resolve_pixel_buffer_by_surface_id,
        escalate_begin: host_gpu_lim_escalate_begin,
        escalate_end: host_gpu_lim_escalate_end,
        texture_native_dma_buf_fd: host_gpu_lim_texture_native_dma_buf_fd,
        set_video_source_timeline_semaphore:
            host_gpu_lim_set_video_source_timeline_semaphore,
        clear_video_source_timeline_semaphore:
            host_gpu_lim_clear_video_source_timeline_semaphore,
        wait_timeline_semaphore: host_gpu_lim_wait_timeline_semaphore,
    };

/// Pointer to the [`GpuContextLimitedAccessVTable`] this DSO should
/// dispatch through. Same DSO-routing rule as
/// [`host_runtime_context_vtable`].
pub fn host_gpu_context_limited_access_vtable() -> *const GpuContextLimitedAccessVTable {
    match host_callbacks() {
        Some(c) if !c.gpu_context_limited_access_vtable.is_null() => {
            c.gpu_context_limited_access_vtable
        }
        _ => &HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE,
    }
}

// ---------------- Shared scratch-buffer helper ----------------

fn write_id_bytes(
    bytes: &[u8],
    out_buf: *mut u8,
    out_buf_cap: usize,
    out_len: *mut usize,
) -> usize {
    let required = bytes.len();
    let written = required.min(out_buf_cap);
    if written > 0 && !out_buf.is_null() {
        // SAFETY: caller guarantees `out_buf` is writable for
        // `out_buf_cap` bytes; we only write `written` bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, written) };
    }
    if !out_len.is_null() {
        // SAFETY: caller guarantees `out_len` is writable.
        unsafe { *out_len = written };
    }
    required
}

// =============================================================================
// OutputWriterVTable wrappers (issue #894 — LAST shared-Rust-type
// crossing in the plugin ABI). Each wrapper reconstructs the inner
// borrow from the raw `Arc::into_raw(Arc<OutputWriterInner>)` handle
// the cdylib passes, runs the inner method, and serializes the
// result into the FFI's out-parameter buffers + `i32 + err_buf`
// shape. All bodies wrapped in `run_host_extern_c` so a panic in
// the inner method becomes a non-zero return.
// =============================================================================

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::into_raw(Arc<crate::iceoryx2::OutputWriterInner>)`. The
/// leaked strong count keeps the inner alive for the call's
/// duration.
unsafe fn handle_as_output_writer_inner(
    handle: *const c_void,
) -> Option<&'static crate::iceoryx2::OutputWriterInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const crate::iceoryx2::OutputWriterInner) })
}

unsafe extern "C" fn host_output_writer_write_raw(
    handle: *const c_void,
    port_ptr: *const u8,
    port_len: usize,
    data_ptr: *const u8,
    data_len: usize,
    timestamp_ns: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_output_writer_write_raw",
        || -> i32 {
            let Some(inner) = (unsafe { handle_as_output_writer_inner(handle) }) else {
                write_extern_err(
                    "write_raw: null OutputWriter handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if port_ptr.is_null() || (port_len > 0 && data_ptr.is_null() && data_len > 0) {
                write_extern_err(
                    "write_raw: null port_ptr or data_ptr",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let port_bytes = unsafe { std::slice::from_raw_parts(port_ptr, port_len) };
            let port = match std::str::from_utf8(port_bytes) {
                Ok(s) => s,
                Err(e) => {
                    write_extern_err(
                        &format!("write_raw: port not UTF-8: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let data = if data_len == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(data_ptr, data_len) }
            };
            match inner.write_raw(port, data, timestamp_ns) {
                Ok(()) => 0,
                Err(e) => {
                    write_extern_err(&e.to_string(), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_output_writer_has_port(
    handle: *const c_void,
    port_ptr: *const u8,
    port_len: usize,
) -> bool {
    run_host_extern_c(
        "host_output_writer_has_port",
        || -> bool {
            let Some(inner) = (unsafe { handle_as_output_writer_inner(handle) }) else {
                return false;
            };
            if port_ptr.is_null() {
                return false;
            }
            let port_bytes = unsafe { std::slice::from_raw_parts(port_ptr, port_len) };
            let Ok(port) = std::str::from_utf8(port_bytes) else {
                return false;
            };
            inner.has_port(port)
        },
        false,
    )
}

pub(crate) unsafe extern "C" fn host_output_writer_clone_arc(
    handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "host_output_writer_clone_arc",
        || -> *const c_void {
            if handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: handle came from Arc::into_raw. We need to
            // reconstruct a non-owning &Arc<Inner> view to call
            // Arc::increment_strong_count, but Arc::from_raw +
            // ManuallyDrop is the idiomatic way to do that for
            // refcount accounting without consuming the strong ref.
            unsafe {
                std::sync::Arc::<crate::iceoryx2::OutputWriterInner>::increment_strong_count(
                    handle as *const crate::iceoryx2::OutputWriterInner,
                );
            }
            handle
        },
        std::ptr::null(),
    )
}

pub(crate) unsafe extern "C" fn host_output_writer_drop_arc(handle: *const c_void) {
    run_host_extern_c(
        "host_output_writer_drop_arc",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: handle came from Arc::into_raw; we release
            // exactly the strong reference Arc::into_raw leaked.
            unsafe {
                std::sync::Arc::<crate::iceoryx2::OutputWriterInner>::decrement_strong_count(
                    handle as *const crate::iceoryx2::OutputWriterInner,
                );
            }
        },
        (),
    )
}

/// Per-DSO host-side static OutputWriter dispatch table.
static HOST_OUTPUT_WRITER_VTABLE: streamlib_plugin_abi::OutputWriterVTable =
    streamlib_plugin_abi::OutputWriterVTable {
        layout_version: streamlib_plugin_abi::OUTPUT_WRITER_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        write_raw: host_output_writer_write_raw,
        has_port: host_output_writer_has_port,
        clone_arc: host_output_writer_clone_arc,
        drop_arc: host_output_writer_drop_arc,
    };

/// Pointer to the [`streamlib_plugin_abi::OutputWriterVTable`] this DSO
/// should dispatch through. Host mode resolves to the local static
/// `HOST_OUTPUT_WRITER_VTABLE`; cdylib mode resolves to the
/// host-installed pointer from [`HostServices::output_writer_vtable`].
pub fn host_output_writer_vtable() -> *const streamlib_plugin_abi::OutputWriterVTable {
    match host_callbacks() {
        Some(c) if !c.output_writer_vtable.is_null() => c.output_writer_vtable,
        _ => &HOST_OUTPUT_WRITER_VTABLE,
    }
}

// =============================================================================
// InputMailboxesVTable wrappers (issue #894)
// =============================================================================

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::into_raw(Arc<crate::iceoryx2::InputMailboxesInner>)`. The
/// leaked strong count keeps the inner alive for the call's
/// duration.
unsafe fn handle_as_input_mailboxes_inner(
    handle: *const c_void,
) -> Option<&'static crate::iceoryx2::InputMailboxesInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const crate::iceoryx2::InputMailboxesInner) })
}

unsafe extern "C" fn host_input_mailboxes_read_raw(
    handle: *const c_void,
    port_ptr: *const u8,
    port_len: usize,
    out_buf: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
    out_timestamp: *mut i64,
    has_data: *mut bool,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_input_mailboxes_read_raw",
        || -> i32 {
            if !out_len.is_null() {
                unsafe {
                    *out_len = 0;
                }
            }
            if !has_data.is_null() {
                unsafe {
                    *has_data = false;
                }
            }
            let Some(inner) = (unsafe { handle_as_input_mailboxes_inner(handle) }) else {
                write_extern_err(
                    "read_raw: null InputMailboxes handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if port_ptr.is_null() {
                write_extern_err(
                    "read_raw: null port_ptr",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let port_bytes = unsafe { std::slice::from_raw_parts(port_ptr, port_len) };
            let port = match std::str::from_utf8(port_bytes) {
                Ok(s) => s,
                Err(e) => {
                    write_extern_err(
                        &format!("read_raw: port not UTF-8: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            // The inner's read_raw consumes the frame from the
            // per-port mailbox. We can't peek non-destructively,
            // so when the consumer's `out_buf` is too small we
            // need to either (a) refuse and signal the required
            // size or (b) consume + lose the frame. Choice (a) is
            // safer; today's iceoryx2 max payload is bounded so
            // a 4 KiB initial buffer covers ~all real traffic.
            //
            // The pattern: pop, measure, if it fits copy + signal
            // success; if not, push it back to the head of the
            // mailbox and signal required size. Today's
            // PortMailbox doesn't expose push-to-head — the
            // simplest sound implementation is to drain via
            // `InputMailboxesInner::read_raw` (which already pops
            // and returns the bytes), then if the bytes fit copy
            // them; if not, return the required size and CONSUME
            // the frame (the consumer's retry sees the next frame
            // or empty mailbox). Truncation on overflow is the
            // documented contract; in practice the β-shape's
            // `read_raw` caller starts with a 4 KiB buffer and
            // resizes proactively when the host indicates a
            // larger payload, so the truncation path triggers
            // only on the rare oversized inbound frame.
            match inner.read_raw(port) {
                Ok(Some((bytes, ts))) => {
                    let required = bytes.len();
                    if !has_data.is_null() {
                        unsafe {
                            *has_data = true;
                        }
                    }
                    if !out_timestamp.is_null() {
                        unsafe {
                            *out_timestamp = ts;
                        }
                    }
                    if !out_len.is_null() {
                        unsafe {
                            *out_len = required;
                        }
                    }
                    if required <= out_cap && !out_buf.is_null() {
                        unsafe {
                            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, required);
                        }
                    } else {
                        // Truncation: indicate required size; the
                        // consumer resizes and the next call sees
                        // an empty mailbox (today's pop semantics
                        // already consumed the frame). The
                        // alternative (peek + non-destructive
                        // size measurement) requires reworking
                        // PortMailbox; deferred to a follow-up
                        // when the truncation path is hit in
                        // practice.
                    }
                    0
                }
                Ok(None) => 0, // has_data stays false
                Err(e) => {
                    write_extern_err(&e.to_string(), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_input_mailboxes_has_data(
    handle: *const c_void,
    port_ptr: *const u8,
    port_len: usize,
) -> bool {
    run_host_extern_c(
        "host_input_mailboxes_has_data",
        || -> bool {
            let Some(inner) = (unsafe { handle_as_input_mailboxes_inner(handle) }) else {
                return false;
            };
            if port_ptr.is_null() {
                return false;
            }
            let port_bytes = unsafe { std::slice::from_raw_parts(port_ptr, port_len) };
            let Ok(port) = std::str::from_utf8(port_bytes) else {
                return false;
            };
            inner.has_data(port)
        },
        false,
    )
}

pub(crate) unsafe extern "C" fn host_input_mailboxes_clone_arc(
    handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "host_input_mailboxes_clone_arc",
        || -> *const c_void {
            if handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: handle came from Arc::into_raw.
            unsafe {
                std::sync::Arc::<crate::iceoryx2::InputMailboxesInner>::increment_strong_count(
                    handle as *const crate::iceoryx2::InputMailboxesInner,
                );
            }
            handle
        },
        std::ptr::null(),
    )
}

pub(crate) unsafe extern "C" fn host_input_mailboxes_drop_arc(handle: *const c_void) {
    run_host_extern_c(
        "host_input_mailboxes_drop_arc",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: handle came from Arc::into_raw.
            unsafe {
                std::sync::Arc::<crate::iceoryx2::InputMailboxesInner>::decrement_strong_count(
                    handle as *const crate::iceoryx2::InputMailboxesInner,
                );
            }
        },
        (),
    )
}

/// Per-DSO host-side static InputMailboxes dispatch table.
static HOST_INPUT_MAILBOXES_VTABLE: streamlib_plugin_abi::InputMailboxesVTable =
    streamlib_plugin_abi::InputMailboxesVTable {
        layout_version: streamlib_plugin_abi::INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        read_raw: host_input_mailboxes_read_raw,
        has_data: host_input_mailboxes_has_data,
        clone_arc: host_input_mailboxes_clone_arc,
        drop_arc: host_input_mailboxes_drop_arc,
    };

/// Pointer to the [`streamlib_plugin_abi::InputMailboxesVTable`] this
/// DSO should dispatch through.
pub fn host_input_mailboxes_vtable() -> *const streamlib_plugin_abi::InputMailboxesVTable {
    match host_callbacks() {
        Some(c) if !c.input_mailboxes_vtable.is_null() => c.input_mailboxes_vtable,
        _ => &HOST_INPUT_MAILBOXES_VTABLE,
    }
}

/// Shared extern-C scratch err-buf writer for the OutputWriter +
/// InputMailboxes host wrappers.
fn write_extern_err(msg: &str, err_buf: *mut u8, err_buf_cap: usize, err_len: *mut usize) {
    if err_buf.is_null() || err_len.is_null() {
        return;
    }
    let bytes = msg.as_bytes();
    let n = bytes.len().min(err_buf_cap);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), err_buf, n);
        *err_len = n;
    }
}

// =============================================================================
// runtime_facing — host-side payload builder
// =============================================================================

/// Host-facing helpers used by `Runner::load_project` (and the
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

// =============================================================================
// TextureRingMethodsVTable wrappers (issue #947 — slot β-shape + method
// dispatch). Each wrapper reconstructs the ring borrow from the raw
// `Arc::into_raw(Arc<TextureRingInner>)` handle the cdylib passes,
// runs the inner method, and serializes the result into the FFI's
// out-parameter buffers + `i32 + err_buf` shape. All bodies are
// wrapped in `run_host_extern_c` so a panic in the inner method
// becomes a non-zero return.
// =============================================================================

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::into_raw(Arc<TextureRingInner>)`. The leaked strong count
/// keeps the ring alive for the call's duration.
#[cfg(target_os = "linux")]
unsafe fn handle_as_texture_ring(
    handle: *const c_void,
) -> Option<&'static crate::core::context::TextureRingInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const crate::core::context::TextureRingInner) })
}

/// Write the slot's POD identity bytes into the caller-provided
/// out-parameter buffers. The texture handle is bumped through the
/// host's limited-access `clone_texture` slot so the resulting
/// cdylib-side `Texture` β-shape owns the matching `Drop`-side
/// decrement.
#[cfg(target_os = "linux")]
unsafe fn write_slot_out_params(
    slot: &crate::core::context::TextureRingSlot,
    out_texture_handle: *mut *const c_void,
    out_texture_width: *mut u32,
    out_texture_height: *mut u32,
    out_texture_format_raw: *mut u32,
    out_surface_id_bytes: *mut [u8; crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES],
    out_surface_id_len: *mut u32,
    out_slot_index: *mut u32,
) {
    // Bump the texture's Arc through the parent limited-access
    // vtable's `clone_texture` slot — same contract every cross-DSO
    // Texture-bearing FFI return uses. The cdylib-side `Texture`
    // β-shape's `Drop` will fire `drop_texture` to balance.
    if !slot.texture.handle.is_null() && !slot.texture.vtable.is_null() {
        unsafe {
            ((*slot.texture.vtable).clone_texture)(slot.texture.handle);
        }
    }
    unsafe {
        *out_texture_handle = slot.texture.handle;
        *out_texture_width = slot.texture.width_cached;
        *out_texture_height = slot.texture.height_cached;
        *out_texture_format_raw = slot.texture.format_raw;
        // Copy the slot's full 64-byte surface_id buffer (inline POD).
        // The cdylib reads it back through `TextureRingSlot::surface_id()`
        // which slices to `surface_id_len`.
        std::ptr::copy_nonoverlapping(
            slot.surface_id_bytes.as_ptr(),
            (*out_surface_id_bytes).as_mut_ptr(),
            crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES,
        );
        *out_surface_id_len = slot.surface_id_len;
        *out_slot_index = slot.slot_index;
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_texture_ring_acquire_next(
    ring_handle: *const c_void,
    out_texture_handle: *mut *const c_void,
    out_texture_width: *mut u32,
    out_texture_height: *mut u32,
    out_texture_format_raw: *mut u32,
    out_surface_id_bytes: *mut [u8; crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES],
    out_surface_id_len: *mut u32,
    out_slot_index: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_ring_acquire_next",
        || -> i32 {
            let Some(ring) = (unsafe { handle_as_texture_ring(ring_handle) }) else {
                write_err(
                    "acquire_next: null ring handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_texture_handle.is_null()
                || out_texture_width.is_null()
                || out_texture_height.is_null()
                || out_texture_format_raw.is_null()
                || out_surface_id_bytes.is_null()
                || out_surface_id_len.is_null()
                || out_slot_index.is_null()
            {
                write_err(
                    "acquire_next: null out-parameter pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // `acquire_next` returns an owned slot (cloned from the
            // pre-allocated `self.slots[idx]`). We write its POD
            // identity bytes through the out-params and let the
            // owned slot drop — `Texture::Drop` decrements the
            // Arc strong count `clone_texture` bumped on this side,
            // but `write_slot_out_params` already bumped a SECOND
            // strong count for the cdylib's eventual `Drop`. Net
            // effect: the cdylib's slot owns +1 strong count
            // balanced by its own Drop, exactly as if the cdylib
            // had called `Arc::into_raw(Arc::clone(...))` itself.
            let slot = ring.acquire_next();
            unsafe {
                write_slot_out_params(
                    &slot,
                    out_texture_handle,
                    out_texture_width,
                    out_texture_height,
                    out_texture_format_raw,
                    out_surface_id_bytes,
                    out_surface_id_len,
                    out_slot_index,
                );
            }
            0
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_texture_ring_copy_pixel_buffer_to_slot(
    ring_handle: *const c_void,
    slot_index: u32,
    surface_id_bytes: *const u8,
    surface_id_len: u32,
    pixel_buffer_handle: *const c_void,
    width: u32,
    height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_ring_copy_pixel_buffer_to_slot",
        || -> i32 {
            let Some(ring) = (unsafe { handle_as_texture_ring(ring_handle) }) else {
                write_err(
                    "copy_pixel_buffer_to_slot: null ring handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer_handle.is_null() {
                write_err(
                    "copy_pixel_buffer_to_slot: null pixel_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if surface_id_bytes.is_null() {
                write_err(
                    "copy_pixel_buffer_to_slot: null surface_id_bytes pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_len = (surface_id_len as usize).min(
                crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES,
            );
            let id_bytes = unsafe { std::slice::from_raw_parts(surface_id_bytes, id_len) };
            let Ok(surface_id) = std::str::from_utf8(id_bytes) else {
                write_err(
                    "copy_pixel_buffer_to_slot: surface_id_bytes is not valid UTF-8",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let borrow = make_pixel_buffer_borrow(pixel_buffer_handle);
            match ring.copy_pixel_buffer_to_slot_by_index(
                slot_index,
                surface_id,
                &*borrow,
                width,
                height,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("copy_pixel_buffer_to_slot: {e}"),
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
unsafe extern "C" fn host_texture_ring_slot(
    ring_handle: *const c_void,
    index: usize,
    out_texture_handle: *mut *const c_void,
    out_texture_width: *mut u32,
    out_texture_height: *mut u32,
    out_texture_format_raw: *mut u32,
    out_surface_id_bytes: *mut [u8; crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES],
    out_surface_id_len: *mut u32,
    out_slot_index: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_ring_slot",
        || -> i32 {
            let Some(ring) = (unsafe { handle_as_texture_ring(ring_handle) }) else {
                write_err(
                    "slot: null ring handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_texture_handle.is_null()
                || out_texture_width.is_null()
                || out_texture_height.is_null()
                || out_texture_format_raw.is_null()
                || out_surface_id_bytes.is_null()
                || out_surface_id_len.is_null()
                || out_slot_index.is_null()
            {
                write_err(
                    "slot: null out-parameter pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // -1 signals "index out of range" without an err_buf
            // write; the cdylib dispatch path translates this to
            // `Option::None`. Any other non-zero is a hard error.
            let Some(slot) = ring.slot(index) else {
                return -1;
            };
            unsafe {
                write_slot_out_params(
                    slot,
                    out_texture_handle,
                    out_texture_width,
                    out_texture_height,
                    out_texture_format_raw,
                    out_surface_id_bytes,
                    out_surface_id_len,
                    out_slot_index,
                );
            }
            0
        },
        1,
    )
}

// ---- Non-Linux platform stubs (vtable layout stays unconditional) ----------

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_texture_ring_acquire_next(
    _ring_handle: *const c_void,
    _out_texture_handle: *mut *const c_void,
    _out_texture_width: *mut u32,
    _out_texture_height: *mut u32,
    _out_texture_format_raw: *mut u32,
    _out_surface_id_bytes: *mut [u8; crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES],
    _out_surface_id_len: *mut u32,
    _out_slot_index: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_next: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_texture_ring_copy_pixel_buffer_to_slot(
    _ring_handle: *const c_void,
    _slot_index: u32,
    _surface_id_bytes: *const u8,
    _surface_id_len: u32,
    _pixel_buffer_handle: *const c_void,
    _width: u32,
    _height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "copy_pixel_buffer_to_slot: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_texture_ring_slot(
    _ring_handle: *const c_void,
    _index: usize,
    _out_texture_handle: *mut *const c_void,
    _out_texture_width: *mut u32,
    _out_texture_height: *mut u32,
    _out_texture_format_raw: *mut u32,
    _out_surface_id_bytes: *mut [u8; crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES],
    _out_surface_id_len: *mut u32,
    _out_slot_index: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "slot: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Host-side `TextureRingMethodsVTable` wired to the per-method
/// wrappers above (issue #947 — `TextureRingSlot` β-shape +
/// method dispatch).
pub static HOST_TEXTURE_RING_METHODS_VTABLE: streamlib_plugin_abi::TextureRingMethodsVTable =
    streamlib_plugin_abi::TextureRingMethodsVTable {
        layout_version: streamlib_plugin_abi::TEXTURE_RING_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        acquire_next: host_texture_ring_acquire_next,
        copy_pixel_buffer_to_slot: host_texture_ring_copy_pixel_buffer_to_slot,
        slot: host_texture_ring_slot,
    };

/// Accessor for the host's static `TextureRingMethodsVTable` — used
/// by `TextureRing::from_arc_into_raw` to populate the β-shape's
/// `methods_vtable` field.
pub fn host_texture_ring_methods_vtable() -> *const streamlib_plugin_abi::TextureRingMethodsVTable {
    &HOST_TEXTURE_RING_METHODS_VTABLE
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

// Each `make_*_borrow` populates the cached POD fields on the
// reconstructed β-shape from the host-side inner we hold via
// `handle`. Cdylib β-shapes carry these cached for free-on-deref
// POD getters (`width()`, `height()`, `mapped_ptr()`, etc.); when
// the host reconstructs a borrow inside a vtable callback for code
// that reads those getters host-side, the borrow's cached fields
// MUST hold the real values — not zero. Reading zero from a "borrow"
// of an otherwise-valid resource was the bug behind issue #988
// (camera-as-cdylib color converter received width=0/height=0 in
// push constants → kernel produced zero-filled output).
#[cfg(target_os = "linux")]
fn make_pixel_buffer_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::core::rhi::PixelBuffer> {
    use crate::host_rhi::HostPixelBufferRefExt;
    // Reconstruct a minimal Pixel-buffer borrow whose `buffer_ref()`
    // can read the host-side `PixelBufferRef` we already hold via
    // `handle`.
    let pb_for_inner = std::mem::ManuallyDrop::new(crate::core::rhi::PixelBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        width: 0,
        height: 0,
        format_raw: 0,
        plane_count_cached: 0,
    });
    let pb_ref = pb_for_inner.buffer_ref();
    let hvb = pb_ref.vulkan_inner();
    let format = pb_ref.format();
    std::mem::ManuallyDrop::new(crate::core::rhi::PixelBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        width: pb_ref.width(),
        height: pb_ref.height(),
        format_raw: format as u32,
        plane_count_cached: hvb.plane_count() as u32,
    })
}

#[cfg(target_os = "linux")]
fn make_storage_buffer_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::core::rhi::StorageBuffer> {
    let sb_for_inner = std::mem::ManuallyDrop::new(crate::core::rhi::StorageBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        byte_size_cached: 0,
        mapped_ptr_cached: std::ptr::null_mut(),
    });
    let hvb = sb_for_inner.host_inner();
    std::mem::ManuallyDrop::new(crate::core::rhi::StorageBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        byte_size_cached: hvb.size() as u64,
        mapped_ptr_cached: hvb.mapped_ptr(),
    })
}

#[cfg(target_os = "linux")]
fn make_uniform_buffer_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::core::rhi::UniformBuffer> {
    let ub_for_inner = std::mem::ManuallyDrop::new(crate::core::rhi::UniformBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        byte_size_cached: 0,
        mapped_ptr_cached: std::ptr::null_mut(),
    });
    let hvb = ub_for_inner.host_inner();
    std::mem::ManuallyDrop::new(crate::core::rhi::UniformBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        byte_size_cached: hvb.size() as u64,
        mapped_ptr_cached: hvb.mapped_ptr(),
    })
}

#[cfg(target_os = "linux")]
fn make_texture_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::core::rhi::Texture> {
    // Populate the cached POD fields from the host-side TextureInner
    // we already have via `handle`. Cdylib β-shapes carry these cached
    // for free-on-deref POD getters (`Texture::width()`, etc.); when
    // the host reconstructs a borrow inside a vtable callback for
    // host-side code that reads `Texture::width()` / `height()`, the
    // borrow's cached fields MUST hold the real values — not zero —
    // because that's what those POD getters return. Reading zero from
    // a "borrow" of an otherwise-valid texture caused the camera-as-
    // cdylib color-converter push constants to encode width=0/height=0
    // and the compute kernel produced zero-filled output (issue #988
    // debug).
    use crate::host_rhi::HostTextureExt;
    let tex_for_inner = std::mem::ManuallyDrop::new(crate::core::rhi::Texture {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        width_cached: 0,
        height_cached: 0,
        format_raw: 0,
        _padding: 0,
    });
    let hvt = tex_for_inner.vulkan_inner();
    let width = hvt.width();
    let height = hvt.height();
    let format = hvt.format();
    std::mem::ManuallyDrop::new(crate::core::rhi::Texture {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        width_cached: width,
        height_cached: height,
        format_raw: format as u32,
        _padding: 0,
    })
}

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

/// Host-side `VulkanComputeKernelMethodsVTable` populated with the
/// v3 method slots (typed binding-method dispatch for the plugin
/// handle's `set_storage_buffer_pixel` / `set_storage_buffer_storage`
/// / `set_uniform_buffer` / `set_sampled_texture` /
/// `set_storage_image` surface plus the previously-shipped
/// `set_push_constants` / `dispatch` primitive-only slots).
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
    };

/// Accessor for the host's static `VulkanComputeKernelMethodsVTable`
/// — used by `VulkanComputeKernel::from_arc_into_raw` to populate
/// the β-shape's `methods_vtable` field.
pub fn host_vulkan_compute_kernel_methods_vtable(
) -> *const streamlib_plugin_abi::VulkanComputeKernelMethodsVTable {
    &HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE
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
fn make_vertex_buffer_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::core::rhi::VertexBuffer> {
    std::mem::ManuallyDrop::new(crate::core::rhi::VertexBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        byte_size_cached: 0,
        mapped_ptr_cached: std::ptr::null_mut(),
    })
}

#[cfg(target_os = "linux")]
fn make_index_buffer_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::core::rhi::IndexBuffer> {
    std::mem::ManuallyDrop::new(crate::core::rhi::IndexBuffer {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        byte_size_cached: 0,
        mapped_ptr_cached: std::ptr::null_mut(),
    })
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
fn make_acceleration_structure_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::vulkan::rhi::VulkanAccelerationStructure> {
    // Read the cached POD descriptors directly from the host-internal
    // Inner. With #955 the β-shape's `kind()` / `device_address()` /
    // `storage_size()` getters read the cached fields (no host_inner()
    // fallback), so the borrow MUST carry real values — the
    // ray-tracing kernel's `set_acceleration_structure` check reads
    // `tlas.kind()` and would see BottomLevel for every borrow if the
    // cached field stayed 0.
    let (cached_kind, cached_device_address, cached_storage_size) =
        if handle.is_null() {
            (0u32, 0u64, 0u64)
        } else {
            // SAFETY: caller hands us a `handle` minted by
            // `Arc::into_raw(Arc<VulkanAccelerationStructureInner>)`,
            // so dereferencing through the host-internal Inner is
            // sound on the host side (this helper is host-only;
            // cdylib borrows would never reach this code path).
            let as_inner = unsafe {
                &*(handle as *const crate::vulkan::rhi::VulkanAccelerationStructureInner)
            };
            let kind = match as_inner.kind() {
                crate::vulkan::rhi::AccelerationStructureKind::BottomLevel => 0u32,
                crate::vulkan::rhi::AccelerationStructureKind::TopLevel => 1u32,
            };
            (kind, as_inner.device_address(), as_inner.storage_size())
        };
    std::mem::ManuallyDrop::new(crate::vulkan::rhi::VulkanAccelerationStructure {
        handle,
        vtable: host_gpu_context_full_access_vtable(),
        methods_vtable: host_vulkan_acceleration_structure_methods_vtable(),
        cached_kind,
        _reserved_padding: 0,
        cached_device_address,
        cached_storage_size,
    })
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

/// Host-side `VulkanGraphicsKernelMethodsVTable` populated with the
/// v2 method slots (typed binding-method dispatch for the plugin
/// handle's `set_storage_buffer_pixel` / `set_storage_buffer_storage`
/// / `set_uniform_buffer` / `set_sampled_texture` /
/// `set_storage_image` / `set_vertex_buffer` / `set_index_buffer` /
/// `set_push_constants` / `offscreen_render` surface).
///
/// Engine-only methods that take a raw `vk::CommandBuffer`
/// (`cmd_bind_and_draw` / `cmd_bind_and_draw_indexed`) stay
/// `host_inner`-routed and are NOT on this vtable — minting a
/// `vk::CommandBuffer` from cdylib code requires an
/// `RhiCommandRecorder` β-shape, which is a separate concern.
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
    };

/// Accessor for the host's static `VulkanGraphicsKernelMethodsVTable`
/// — used by `VulkanGraphicsKernel::from_arc_into_raw` to populate
/// the β-shape's `methods_vtable` field.
pub fn host_vulkan_graphics_kernel_methods_vtable(
) -> *const streamlib_plugin_abi::VulkanGraphicsKernelMethodsVTable {
    &HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE
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

/// Host-side `RhiColorConverterMethodsVTable` wired to the per-method
/// wrappers above (Phase E sub-lift slice A).
pub static HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE:
    streamlib_plugin_abi::RhiColorConverterMethodsVTable =
    streamlib_plugin_abi::RhiColorConverterMethodsVTable {
        layout_version:
            streamlib_plugin_abi::RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        prepare_buffer_to_image_storage:
            host_color_converter_prepare_buffer_to_image_storage,
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
/// The cached POD fields (`cached_push_constant_size`,
/// `_reserved_padding`) are filled with zeros. The
/// `RhiCommandRecorderInner::record_dispatch` path only deref's
/// `self.handle` to reach the underlying `VulkanComputeKernelInner`
/// (via the engine-side `VulkanComputeKernel::record` →
/// `host_inner()` chain that runs on host code, NOT through the
/// vtable). If a future record-dispatch path starts reading
/// `kernel.push_constant_size()` through the wrapper, the zeroed POD
/// silently produces wrong results — extend this helper to populate
/// the field at that point.
///
/// The vtable + methods_vtable pointers are filled with the host's
/// own statics (matching what `from_arc_into_raw` would have written
/// in host mode) so the borrow is well-formed for any field-only
/// read even though no vtable callback is supposed to fire while the
/// borrow is alive.
#[cfg(target_os = "linux")]
fn make_compute_kernel_borrow(
    handle: *const c_void,
) -> std::mem::ManuallyDrop<crate::vulkan::rhi::VulkanComputeKernel> {
    std::mem::ManuallyDrop::new(crate::vulkan::rhi::VulkanComputeKernel {
        handle,
        vtable: host_gpu_context_full_access_vtable(),
        methods_vtable: host_vulkan_compute_kernel_methods_vtable(),
        cached_push_constant_size: 0,
        _reserved_padding: 0,
    })
}

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

/// Host-side `RhiCommandRecorderMethodsVTable` wired to the
/// per-method wrappers above (Phase E sub-lift slice B — #984).
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
    };

/// Accessor for the host's static `RhiCommandRecorderMethodsVTable`
/// — used by `RhiCommandRecorder::from_inner` to populate the
/// β-shape's `methods_vtable` field.
pub fn host_rhi_command_recorder_methods_vtable(
) -> *const streamlib_plugin_abi::RhiCommandRecorderMethodsVTable {
    &HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE
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
#[cfg(test)]
mod gpu_full_access_vtable_tests {
    use super::*;
    use streamlib_plugin_abi::{
        ComputeKernelDescriptorRepr, GraphicsKernelDescriptorRepr,
        RayTracingKernelDescriptorRepr,
    };

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn drop_handle_handles_null_no_crash() {
        // Null handle is documented as a no-op; this just exercises
        // the early-return guard.
        unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.drop_handle)(std::ptr::null());
        }
    }

    #[test]
    fn create_compute_kernel_returns_error_on_null_scope_token() {
        // Post-C3: gpu_handle is interpreted as a scope_token; a null
        // pointer corresponds to scope_token = 0, which is reserved as
        // "never issued" — `with_scope` returns None and the callback
        // returns an "invalid escalate scope" error.
        let (mut buf, mut len) = make_err_buf();
        let mut out: *const c_void = std::ptr::null();
        let bindings_buf: [streamlib_plugin_abi::ComputeBindingSpecRepr; 0] = [];
        let repr = ComputeKernelDescriptorRepr {
            label_ptr: "test".as_ptr(),
            label_len: 4,
            spv_ptr: std::ptr::null(),
            spv_len: 0,
            bindings_ptr: bindings_buf.as_ptr(),
            bindings_len: 0,
            push_constant_size: 0,
            _reserved_padding: 0,
        };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_compute_kernel)(
                std::ptr::null(),
                &repr,
                &mut out,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("create_compute_kernel: invalid escalate scope"),
            "got: {msg}"
        );
        assert!(out.is_null(), "out_kernel must not be written on error");
    }

    #[test]
    fn create_graphics_kernel_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut out: *const c_void = std::ptr::null();
        let repr: GraphicsKernelDescriptorRepr = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_graphics_kernel)(
                std::ptr::null(),
                &repr,
                &mut out,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("create_graphics_kernel: invalid escalate scope"),
            "got: {msg}"
        );
        assert!(out.is_null());
    }

    #[test]
    fn create_ray_tracing_kernel_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut out: *const c_void = std::ptr::null();
        let repr: RayTracingKernelDescriptorRepr = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_ray_tracing_kernel)(
                std::ptr::null(),
                &repr,
                &mut out,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("create_ray_tracing_kernel: invalid escalate scope"),
            "got: {msg}"
        );
        assert!(out.is_null());
    }

    #[test]
    fn create_texture_ring_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut out: *const c_void = std::ptr::null();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_texture_ring)(
                std::ptr::null(),
                64,
                64,
                0, // Rgba8Unorm
                0, // no usage bits
                2,
                &mut out,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("create_texture_ring: invalid escalate scope"),
            "got: {msg}"
        );
        assert!(out.is_null());
    }

    #[test]
    fn acquire_render_target_dma_buf_image_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut out: crate::core::rhi::texture::Texture =
            unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE
                .acquire_render_target_dma_buf_image)(
                std::ptr::null(),
                64,
                64,
                0, // Rgba8Unorm
                &mut out as *mut _ as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains(
                "acquire_render_target_dma_buf_image: invalid escalate scope"
            ),
            "got: {msg}"
        );
    }

    #[test]
    fn acquire_render_target_dma_buf_image_returns_error_on_invalid_format() {
        // Even with an invalid format, the null scope-token check would
        // run after the format decode — so feeding a token of 0 (which
        // would later fail scope lookup) but an invalid format ensures
        // the format-validation path fires.
        let (mut buf, mut len) = make_err_buf();
        let mut out: crate::core::rhi::texture::Texture =
            unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE
                .acquire_render_target_dma_buf_image)(
                std::ptr::null(),
                64,
                64,
                99, // invalid format_raw
                &mut out as *mut _ as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains(
                "acquire_render_target_dma_buf_image: invalid format_raw"
            ),
            "got: {msg}"
        );
    }

    // ============================================================================
    // Phase D (#906) — tier-1 wire-format tests for the 9 new FullAccess slots
    // ============================================================================

    #[test]
    fn wait_device_idle_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.wait_device_idle)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("wait_device_idle: invalid escalate scope"),
            "got: {msg}"
        );
    }

    #[test]
    fn acquire_output_texture_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut id_buf = [0u8; 256];
        let mut id_len: usize = 0;
        let mut out: crate::core::rhi::texture::Texture =
            unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.acquire_output_texture)(
                std::ptr::null(),
                64,
                64,
                0,
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                &mut out as *mut _ as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_output_texture: invalid escalate scope"),
            "got: {msg}"
        );
    }

    #[test]
    fn acquire_output_texture_returns_error_on_invalid_format() {
        let (mut buf, mut len) = make_err_buf();
        let mut id_buf = [0u8; 256];
        let mut id_len: usize = 0;
        let mut out: crate::core::rhi::texture::Texture =
            unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.acquire_output_texture)(
                std::ptr::null(),
                64,
                64,
                99,
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                &mut out as *mut _ as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_output_texture: invalid format_raw"),
            "got: {msg}"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn upload_pixel_buffer_as_texture_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        // We pass non-null surface_id + a "borrowed" PixelBuffer placeholder
        // through the null-pointer guard; the scope-token check then fires
        // because the token is null/zero.
        let sid = b"abc";
        let pb: crate::core::rhi::PixelBuffer = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.upload_pixel_buffer_as_texture)(
                std::ptr::null(),
                sid.as_ptr(),
                sid.len(),
                &pb as *const _ as *const c_void,
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        // Leak the zeroed PixelBuffer to avoid running its (cdylib-mode)
        // Drop on a null handle — that would dispatch through a null
        // vtable. The null-handle Drop guard short-circuits, but
        // mem::forget makes the intent explicit.
        std::mem::forget(pb);
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("upload_pixel_buffer_as_texture: invalid escalate scope"),
            "got: {msg}"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn color_converter_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut out: *const c_void = std::ptr::null();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.color_converter)(
                std::ptr::null(),
                0, // src
                0, // dst
                &mut out,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("color_converter: invalid escalate scope"),
            "got: {msg}"
        );
        assert!(out.is_null());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn create_command_recorder_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let label = b"test_recorder";
        let mut out: std::mem::MaybeUninit<crate::vulkan::rhi::RhiCommandRecorder> =
            std::mem::MaybeUninit::uninit();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_command_recorder)(
                std::ptr::null(),
                label.as_ptr(),
                label.len(),
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("create_command_recorder: invalid escalate scope"),
            "got: {msg}"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn build_triangles_blas_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let label = b"test_blas";
        let vertices = [0.0f32, 0.0, 0.0];
        let indices = [0u32, 1, 2];
        let mut out: *const c_void = std::ptr::null();
        let mut out_device_address: u64 = 0;
        let mut out_storage_size: u64 = 0;
        let mut out_kind: u32 = 0;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.build_triangles_blas)(
                std::ptr::null(),
                label.as_ptr(),
                label.len(),
                vertices.as_ptr(),
                vertices.len(),
                indices.as_ptr(),
                indices.len(),
                &mut out,
                &mut out_device_address as *mut u64,
                &mut out_storage_size as *mut u64,
                &mut out_kind as *mut u32,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("build_triangles_blas: invalid escalate scope"),
            "got: {msg}"
        );
        assert!(out.is_null());
        // Out-params untouched on failure.
        assert_eq!(out_device_address, 0);
        assert_eq!(out_storage_size, 0);
        assert_eq!(out_kind, 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn build_tlas_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let label = b"test_tlas";
        let mut out: *const c_void = std::ptr::null();
        let mut out_device_address: u64 = 0;
        let mut out_storage_size: u64 = 0;
        let mut out_kind: u32 = 0;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.build_tlas)(
                std::ptr::null(),
                label.as_ptr(),
                label.len(),
                std::ptr::null(),
                0,
                &mut out,
                &mut out_device_address as *mut u64,
                &mut out_storage_size as *mut u64,
                &mut out_kind as *mut u32,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("build_tlas: invalid escalate scope"),
            "got: {msg}"
        );
        assert!(out.is_null());
        // Out-params untouched on failure.
        assert_eq!(out_device_address, 0);
        assert_eq!(out_storage_size, 0);
        assert_eq!(out_kind, 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn supports_ray_tracing_pipeline_returns_negative_one_on_null_scope_token() {
        // Returns -1 for "invalid scope token" (since 1/0 are valid yes/no
        // bool returns). The error message goes to err_buf.
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.supports_ray_tracing_pipeline)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, -1, "null scope token must return -1, got {rc}");
    }

    #[test]
    fn check_in_surface_returns_error_on_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let pb: crate::core::rhi::PixelBuffer = unsafe { std::mem::zeroed() };
        let mut id_buf = [0u8; 256];
        let mut id_len: usize = 0;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.check_in_surface)(
                std::ptr::null(),
                &pb as *const _ as *const c_void,
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        std::mem::forget(pb);
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("check_in_surface: invalid escalate scope"),
            "got: {msg}"
        );
    }

    #[test]
    fn vtable_layout_version_matches_constant() {
        assert_eq!(
            HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.layout_version,
            streamlib_plugin_abi::GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION
        );
    }

    #[test]
    fn host_services_for_self_wires_full_access_vtable() {
        let node = match crate::iceoryx2::Iceoryx2Node::new() {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    target: "streamlib::tests::gpu_full_access_vtable",
                    error = %e,
                    "skipping host_services_for_self wiring assertion: iceoryx2 init unavailable in this env"
                );
                return;
            }
        };
        let services = runtime_facing::host_services_for_self(&node);
        assert!(
            !services.gpu_context_full_access_vtable.is_null(),
            "host should wire the FullAccess vtable pointer"
        );
        let installed_version =
            unsafe { (*services.gpu_context_full_access_vtable).layout_version };
        assert_eq!(
            installed_version,
            streamlib_plugin_abi::GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION
        );
    }
}

#[cfg(test)]
mod gpu_lim_escalate_vtable_tests {
    //! Tier-1 wire-format + round-trip tests for C3's escalate_begin
    //! and escalate_end vtable entries.
    //!
    //! Tests that construct a real `GpuContext` carry `#[serial]` to
    //! prevent the NVIDIA Linux dual-`VkDevice` SIGSEGV
    //! (`docs/learnings/nvidia-dual-vulkan-device-crash.md`) when run
    //! against other VkDevice-creating tests in the workspace lib
    //! suite.

    use super::*;
    use serial_test::serial;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    /// Build a host-mode gpu_handle (the `Box<Arc<GpuContext>>`-shaped
    /// pointer that `GpuContextLimitedAccess::new` produces) so the
    /// `escalate_begin` callback can run end-to-end against a real
    /// `Arc<GpuContext>`. Skips when no GPU device is available.
    fn make_host_handle() -> Option<(*const c_void, Arc<crate::core::context::GpuContext>)> {
        let gpu = crate::core::context::GpuContext::init_for_platform().ok()?;
        let arc = Arc::new(gpu);
        let boxed: Box<Arc<crate::core::context::GpuContext>> = Box::new(Arc::clone(&arc));
        let handle = Box::into_raw(boxed) as *const c_void;
        Some((handle, arc))
    }

    /// Free a host_handle minted by `make_host_handle` — pairs with
    /// the `Box::into_raw`.
    unsafe fn free_host_handle(handle: *const c_void) {
        let _ = unsafe {
            Box::from_raw(handle as *mut Arc<crate::core::context::GpuContext>)
        };
    }

    #[test]
    fn escalate_begin_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut token: *const c_void = std::ptr::null();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                std::ptr::null(),
                &mut token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("escalate_begin: null gpu handle"), "got: {msg}");
        assert!(token.is_null(), "scope token must not be written on error");
    }

    #[test]
    #[serial]
    fn escalate_begin_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            tracing::warn!(
                target: "streamlib::tests::escalate_vtable",
                "skipping escalate_begin null-out test: no GPU device"
            );
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                handle,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("escalate_begin: null out_scope_token"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    fn escalate_end_is_idempotent_for_stale_token() {
        // escalate_end with a never-issued token is a clean no-op
        // (returns 0; doesn't release any gate). Documented as
        // idempotent in the registry.
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_end)(
                std::ptr::null(),
                u64::MAX as *const c_void, // never-issued token
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 0);
        assert_eq!(len, 0, "no error message expected for stale token");
    }

    #[test]
    #[serial]
    fn round_trip_begin_then_end_releases_gate() {
        let Some((handle, _arc)) = make_host_handle() else {
            tracing::warn!(
                target: "streamlib::tests::escalate_vtable",
                "skipping round-trip test: no GPU device"
            );
            return;
        };

        let (mut buf, mut len) = make_err_buf();
        let mut token: *const c_void = std::ptr::null();
        let begin_rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                handle,
                &mut token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(begin_rc, 0);
        assert!(!token.is_null(), "scope token must be written on success");

        let end_rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_end)(
                handle,
                token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(end_rc, 0);

        // Begin again on the same handle — gate must have been
        // released, so this succeeds without blocking. (If the gate
        // hadn't released, this would deadlock.)
        let mut token2: *const c_void = std::ptr::null();
        let begin2_rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                handle,
                &mut token2,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(begin2_rc, 0);
        assert!(!token2.is_null());
        assert_ne!(token, token2, "tokens must be unique per begin call");

        let _ = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_end)(
                handle,
                token2,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn full_access_callback_with_valid_token_resolves_scope() {
        // End-to-end: begin a scope, get a valid token, invoke a
        // FullAccess vtable callback with the token + a valid
        // descriptor. The callback's scope-token lookup must succeed
        // (no "invalid escalate scope" error). The actual allocation
        // may succeed or fail depending on the Vulkan environment
        // (render-target DMA-BUF availability, EGL DRM modifier
        // probe), but EITHER outcome proves the scope lookup passed:
        // a success returns rc=0 with `out_texture` populated; a
        // failure returns rc=1 with an error message that does NOT
        // contain "invalid escalate scope".
        //
        // (Mentally revert `with_full_scope_or_err` to always return
        // None — this test fails because the error message would
        // then contain "invalid escalate scope".)
        let Some((handle, _arc)) = make_host_handle() else {
            tracing::warn!(
                target: "streamlib::tests::escalate_vtable",
                "skipping valid-token test: no GPU device"
            );
            return;
        };

        let (mut buf, mut len) = make_err_buf();
        let mut token: *const c_void = std::ptr::null();
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                handle,
                &mut token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            );
        }
        assert!(!token.is_null());

        let mut out: crate::core::rhi::texture::Texture =
            unsafe { std::mem::zeroed() };
        let mut buf2 = [0u8; 256];
        let mut len2 = 0usize;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE
                .acquire_render_target_dma_buf_image)(
                token,
                64,
                64,
                0, // Rgba8Unorm — valid format; forces scope lookup to run
                &mut out as *mut _ as *mut c_void,
                buf2.as_mut_ptr(),
                buf2.len(),
                &mut len2,
            )
        };

        if rc != 0 {
            // Allocation failed for an environment reason; assert the
            // failure was NOT a scope-lookup miss.
            let msg = err_buf_as_str(&buf2, len2);
            assert!(
                !msg.contains("invalid escalate scope"),
                "scope-token lookup must succeed inside an active \
                 scope; got: {msg}"
            );
        } else {
            // Allocation succeeded — definitively proves scope lookup
            // worked. The Texture in `out` owns a live handle; its
            // Drop will fire the vtable's drop_texture as the test
            // returns.
            assert!(!out.handle.is_null(), "out_texture handle populated");
            // SAFETY: `out` was overwritten by `ptr::write` from the
            // callback with a valid Texture; let its normal Drop run
            // to release the underlying handle via the vtable.
        }

        // Clean up the scope.
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_end)(
                handle,
                token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            );
        }
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn full_access_callback_fails_after_escalate_end() {
        // Closes the scope-token validation loop: a token used after
        // escalate_end fires returns the InvalidEscalateScope error
        // (matches the "calls after escalate_end return
        // InvalidEscalateScope" exit criterion).
        let Some((handle, _arc)) = make_host_handle() else {
            tracing::warn!(
                target: "streamlib::tests::escalate_vtable",
                "skipping post-end test: no GPU device"
            );
            return;
        };

        let (mut buf, mut len) = make_err_buf();
        let mut token: *const c_void = std::ptr::null();
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_begin)(
                handle,
                &mut token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            );
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.escalate_end)(
                handle,
                token,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            );
        }

        // Token is now stale — using it on any FullAccess callback
        // returns "invalid escalate scope".
        let mut out: crate::core::rhi::texture::Texture =
            unsafe { std::mem::zeroed() };
        let mut buf2 = [0u8; 256];
        let mut len2 = 0usize;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE
                .acquire_render_target_dma_buf_image)(
                token,
                64,
                64,
                0, // valid format
                &mut out as *mut _ as *mut c_void,
                buf2.as_mut_ptr(),
                buf2.len(),
                &mut len2,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf2, len2);
        assert!(
            msg.contains(
                "acquire_render_target_dma_buf_image: invalid escalate scope"
            ),
            "got: {msg}"
        );

        unsafe { free_host_handle(handle) };
    }
}

#[cfg(test)]
mod gpu_lim_texture_native_dma_buf_fd_tests {
    //! Tier-1 wire-format test for the Phase F
    //! `texture_native_dma_buf_fd` slot (#908 / #957). The slot is the
    //! cross-DSO landing for `Texture::native_handle` on Linux and
    //! returns the DMA-BUF FD widened to `i64`; sentinel `-1` encodes
    //! the `Option::None` case. A null texture handle must be a clean
    //! `-1` (no panic, no UB) — the wrapper short-circuits before any
    //! cast through `*const TextureInner`.

    use super::*;

    #[test]
    fn texture_native_dma_buf_fd_returns_minus_one_on_null_handle() {
        // Null texture_handle is the cdylib-shaped "Texture wasn't
        // minted yet / was already dropped" case. The slot returns
        // `-1` (= `Option::None` in the Rust-side wrapper) without
        // panicking and without touching the null pointer.
        let fd = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .texture_native_dma_buf_fd)(std::ptr::null())
        };
        assert_eq!(
            fd, -1,
            "null texture_handle must produce -1 sentinel (None)"
        );
    }
}

#[cfg(test)]
mod gpu_lim_video_source_timeline_semaphore_tests {
    //! Tier-1 wire-format tests for the v12 (#958)
    //! `set_video_source_timeline_semaphore` /
    //! `clear_video_source_timeline_semaphore` slots. Each wrapper
    //! must short-circuit on null gpu_handle (and `set` on null
    //! timeline_handle) without panicking and without dereferencing
    //! the null pointers.
    //!
    //! The non-null-handle path is exercised end-to-end by the
    //! `load_project_dylib_camera_smoke` integration test (which
    //! holds a real `Arc<HostVulkanTimelineSemaphore>` and is the
    //! only place a Tier-1 with-handle test could reach without
    //! constructing a real `GpuContext` here).
    //!
    //! Mental-revert: stub the wrapper bodies to
    //! `unimplemented!()` and these tests trip the underlying
    //! deref / panic — the wire-format claim regresses.
    use super::*;

    #[test]
    fn set_video_source_timeline_is_noop_on_null_gpu_handle() {
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .set_video_source_timeline_semaphore)(
                std::ptr::null(),
                std::ptr::null(),
            );
        }
    }

    // Note: the timeline_handle null guard at host_gpu_lim_set_video_source_timeline_semaphore
    // line 2078 isn't reachable at tier-1: the first guard
    // (handle_as_gpu_context) short-circuits on null gpu_handle, and
    // a non-null garbage gpu_handle would UB-deref before reaching
    // the timeline check. The guard is exercised end-to-end by
    // load_project_dylib_camera_smoke (the cdylib camera passes a
    // valid gpu_handle and a real Arc-borrow timeline_handle).

    #[test]
    fn clear_video_source_timeline_is_noop_on_null_gpu_handle() {
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .clear_video_source_timeline_semaphore)(std::ptr::null());
        }
    }
}

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
mod texture_ring_methods_vtable_null_tests {
    //! Tier-1 wire-format tests for the v2 method slots on
    //! `TextureRingMethodsVTable` (issue #947). Each wrapper must
    //! reject a null ring handle before reaching any ring-side state
    //! so cdylib callers get a clean error return on the wire-format
    //! path instead of UB.
    //!
    //! End-to-end coverage (real ring + valid handles + slot
    //! round-trip) is locked by the dlopen integration test for the
    //! cross-rustc fixture, which exercises `acquire_next` +
    //! `copy_pixel_buffer_to_slot` end-to-end after the v2 wire-up.

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn acquire_next_rejects_null_ring_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut h: *const c_void = std::ptr::null();
        let mut w: u32 = 0;
        let mut hgt: u32 = 0;
        let mut fmt: u32 = 0;
        let mut id_bytes = [0u8;
            crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES];
        let mut id_len: u32 = 0;
        let mut slot_index: u32 = 0;
        let rc = unsafe {
            (HOST_TEXTURE_RING_METHODS_VTABLE.acquire_next)(
                std::ptr::null(),
                &mut h as *mut *const c_void,
                &mut w as *mut u32,
                &mut hgt as *mut u32,
                &mut fmt as *mut u32,
                &mut id_bytes as *mut _,
                &mut id_len as *mut u32,
                &mut slot_index as *mut u32,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("acquire_next: null ring handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn copy_pixel_buffer_to_slot_rejects_null_ring_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_TEXTURE_RING_METHODS_VTABLE.copy_pixel_buffer_to_slot)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                32,
                32,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("copy_pixel_buffer_to_slot: null ring handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn slot_rejects_null_ring_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut h: *const c_void = std::ptr::null();
        let mut w: u32 = 0;
        let mut hgt: u32 = 0;
        let mut fmt: u32 = 0;
        let mut id_bytes = [0u8;
            crate::core::context::TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES];
        let mut id_len: u32 = 0;
        let mut slot_index: u32 = 0;
        let rc = unsafe {
            (HOST_TEXTURE_RING_METHODS_VTABLE.slot)(
                std::ptr::null(),
                0,
                &mut h as *mut *const c_void,
                &mut w as *mut u32,
                &mut hgt as *mut u32,
                &mut fmt as *mut u32,
                &mut id_bytes as *mut _,
                &mut id_len as *mut u32,
                &mut slot_index as *mut u32,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("slot: null ring handle"),
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
mod gpu_lim_tier1_wire_format_tests {
    //! Tier-1 wire-format tests for every callback on
    //! [`GpuContextLimitedAccessVTable`].
    //!
    //! Each test passes a null `handle` (and where applicable a null
    //! out-param or invalid input) and asserts the documented contract:
    //!
    //! - Lifecycle callbacks (clone/drop, Arc refcount bumps, etc.)
    //!   short-circuit on null and do not crash.
    //! - Probe callbacks (`strong_count_pixel_buffer`,
    //!   `plane_*_pixel_buffer`, `texture_registration_current_layout`,
    //!   etc.) return their documented default value.
    //! - Result-returning callbacks (`acquire_*`, `resolve_*`,
    //!   `command_queue`, `create_command_buffer*`, `blit_copy*`, ...)
    //!   return rc=1 with a callback-prefixed UTF-8 error in `err_buf`
    //!   and leave their out-slot unwritten.
    //! - `surface_store` writes a null-handle β-shape (the "None"
    //!   sentinel) regardless of input.
    //!
    //! `escalate_begin` / `escalate_end` are covered by
    //! [`gpu_lim_escalate_vtable_tests`]; `texture_native_dma_buf_fd`
    //! by [`gpu_lim_texture_native_dma_buf_fd_tests`].
    //!
    //! The vtable's `layout_version` field is locked against
    //! `GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION` so a
    //! cdylib-side ABI bump can't drift from the host's wiring.
    //!
    //! Tests that build a real `GpuContext` via `make_host_handle`
    //! carry `#[serial]` for the same NVIDIA dual-`VkDevice` reason
    //! as the escalate-vtable suite
    //! (`docs/learnings/nvidia-dual-vulkan-device-crash.md`).

    use super::*;
    use serial_test::serial;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    // ------------------------------------------------------------------
    // Layout-version match
    // ------------------------------------------------------------------

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.layout_version,
            streamlib_plugin_abi::GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION,
        );
    }

    // ------------------------------------------------------------------
    // Lifecycle callbacks — null is a documented no-op
    // ------------------------------------------------------------------

    /// Generates a `null_handle_no_crash` test for a single-argument
    /// lifecycle callback (clone/drop) that takes `handle: *const c_void`
    /// and returns `()` — null is documented as a no-op.
    macro_rules! null_handle_no_crash_test {
        ($test_name:ident, $field:ident) => {
            #[test]
            fn $test_name() {
                unsafe {
                    (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.$field)(std::ptr::null());
                }
            }
        };
    }

    null_handle_no_crash_test!(drop_handle_handles_null, drop_handle);
    null_handle_no_crash_test!(clone_pixel_buffer_handles_null, clone_pixel_buffer);
    null_handle_no_crash_test!(drop_pixel_buffer_handles_null, drop_pixel_buffer);
    null_handle_no_crash_test!(clone_texture_handles_null, clone_texture);
    null_handle_no_crash_test!(drop_texture_handles_null, drop_texture);
    null_handle_no_crash_test!(
        drop_pooled_texture_handle_handles_null,
        drop_pooled_texture_handle
    );
    null_handle_no_crash_test!(clone_storage_buffer_handles_null, clone_storage_buffer);
    null_handle_no_crash_test!(drop_storage_buffer_handles_null, drop_storage_buffer);
    null_handle_no_crash_test!(clone_uniform_buffer_handles_null, clone_uniform_buffer);
    null_handle_no_crash_test!(drop_uniform_buffer_handles_null, drop_uniform_buffer);
    null_handle_no_crash_test!(clone_vertex_buffer_handles_null, clone_vertex_buffer);
    null_handle_no_crash_test!(drop_vertex_buffer_handles_null, drop_vertex_buffer);
    null_handle_no_crash_test!(clone_index_buffer_handles_null, clone_index_buffer);
    null_handle_no_crash_test!(drop_index_buffer_handles_null, drop_index_buffer);
    null_handle_no_crash_test!(
        clone_texture_registration_handles_null,
        clone_texture_registration
    );
    null_handle_no_crash_test!(
        drop_texture_registration_handles_null,
        drop_texture_registration
    );
    null_handle_no_crash_test!(clone_rhi_command_queue_handles_null, clone_rhi_command_queue);
    null_handle_no_crash_test!(drop_rhi_command_queue_handles_null, drop_rhi_command_queue);
    null_handle_no_crash_test!(drop_command_buffer_handles_null, drop_command_buffer);
    null_handle_no_crash_test!(commit_command_buffer_handles_null, commit_command_buffer);
    null_handle_no_crash_test!(
        commit_and_wait_command_buffer_handles_null,
        commit_and_wait_command_buffer
    );

    // ------------------------------------------------------------------
    // Probe callbacks — null returns the documented sentinel
    // ------------------------------------------------------------------

    #[test]
    fn clone_handle_returns_null_on_null_input() {
        let out = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.clone_handle)(std::ptr::null())
        };
        assert!(out.is_null());
    }

    #[test]
    fn strong_count_pixel_buffer_returns_zero_on_null() {
        let n = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.strong_count_pixel_buffer)(
                std::ptr::null(),
            )
        };
        assert_eq!(n, 0);
    }

    #[test]
    fn plane_base_address_pixel_buffer_returns_null_on_null_handle() {
        let p = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.plane_base_address_pixel_buffer)(
                std::ptr::null(),
                0,
            )
        };
        assert!(p.is_null());
    }

    #[test]
    fn plane_size_pixel_buffer_returns_zero_on_null_handle() {
        let n = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.plane_size_pixel_buffer)(
                std::ptr::null(),
                0,
            )
        };
        assert_eq!(n, 0);
    }

    #[test]
    fn texture_registration_texture_returns_null_on_null_handle() {
        let p = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.texture_registration_texture)(
                std::ptr::null(),
            )
        };
        assert!(p.is_null());
    }

    #[test]
    fn texture_registration_current_layout_returns_zero_on_null_handle() {
        let v = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.texture_registration_current_layout)(
                std::ptr::null(),
            )
        };
        assert_eq!(v, 0, "VK_IMAGE_LAYOUT_UNDEFINED == 0");
    }

    #[test]
    fn texture_registration_update_layout_handles_null_no_crash() {
        // Two-arg shape (handle, layout_raw); null handle short-circuits
        // before the atomic store. The macro above is single-arg only,
        // so this gets its own test.
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.texture_registration_update_layout)(
                std::ptr::null(),
                42,
            );
        }
    }

    // ------------------------------------------------------------------
    // Update / register callbacks (no err_buf, no return) — null gpu
    // handle is a documented no-op
    // ------------------------------------------------------------------

    #[test]
    fn register_texture_handles_null_gpu_no_crash() {
        let id = b"abc";
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.register_texture)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                std::ptr::null(),
                0,
            );
        }
    }

    #[test]
    fn update_texture_registration_layout_handles_null_gpu_no_crash() {
        let id = b"abc";
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.update_texture_registration_layout)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                42,
            );
        }
    }

    #[test]
    fn unregister_texture_handles_null_gpu_no_crash() {
        let id = b"abc";
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.unregister_texture)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
            );
        }
    }

    #[test]
    fn copy_texture_command_buffer_handles_null_no_crash() {
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.copy_texture_command_buffer)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            );
        }
    }

    // ------------------------------------------------------------------
    // surface_store — always writes a defined β-shape; null gpu_handle
    // yields the "None" sentinel (null handle + null vtable)
    // ------------------------------------------------------------------

    #[test]
    fn surface_store_writes_null_beta_shape_on_null_gpu_handle() {
        // SAFETY: SurfaceStore is `#[repr(C)] (handle, vtable)`; the
        // callback always writes through the out-pointer first, so a
        // zero-init landing slot is safe to read after the call.
        let mut out: crate::core::context::SurfaceStore = unsafe { std::mem::zeroed() };
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.surface_store)(
                std::ptr::null(),
                &mut out as *mut _ as *mut c_void,
            );
        }
        assert!(out.is_none(), "null gpu_handle must produce a None β-shape");
    }

    #[test]
    fn surface_store_handles_null_out_param_no_crash() {
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.surface_store)(
                std::ptr::null(),
                std::ptr::null_mut(),
            );
        }
    }

    // ------------------------------------------------------------------
    // Result-returning callbacks (rc=1, err_buf populated)
    // ------------------------------------------------------------------

    /// Generates a null-gpu-handle test for a callback whose signature
    /// is `(gpu_handle, out, err_buf, err_buf_cap, err_len) -> i32` —
    /// the most common shape. `err_marker` is a substring expected in
    /// the err_buf message.
    macro_rules! null_gpu_handle_err_test {
        ($test_name:ident, $field:ident, $err_marker:expr) => {
            #[test]
            fn $test_name() {
                let (mut buf, mut len) = make_err_buf();
                let mut out_storage = [0u8; 256];
                let rc = unsafe {
                    (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.$field)(
                        std::ptr::null(),
                        out_storage.as_mut_ptr() as *mut c_void,
                        buf.as_mut_ptr(),
                        buf.len(),
                        &mut len,
                    )
                };
                assert_eq!(rc, 1);
                let msg = err_buf_as_str(&buf, len);
                assert!(msg.contains($err_marker), "got: {msg}");
            }
        };
    }

    null_gpu_handle_err_test!(
        command_queue_returns_error_on_null_gpu_handle,
        command_queue,
        "command_queue: null gpu handle"
    );

    null_gpu_handle_err_test!(
        create_command_buffer_returns_error_on_null_gpu_handle,
        create_command_buffer,
        "create_command_buffer: null gpu handle"
    );

    #[test]
    #[serial]
    fn command_queue_returns_error_on_null_out_param() {
        // null gpu_handle path runs first; need a non-null synthetic
        // handle to reach the null-out-param branch. Build a host-mode
        // handle if available; otherwise skip — this test is purely
        // about the wrapper's null-out-param guard, which on a null
        // gpu_handle is unreachable.
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.command_queue)(
                handle,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("command_queue: null out_queue"), "got: {msg}");
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn create_command_buffer_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.create_command_buffer)(
                handle,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("create_command_buffer: null out_cb"), "got: {msg}");
        unsafe { free_host_handle(handle) };
    }

    // --- acquire_texture ---

    #[test]
    fn acquire_texture_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_texture)(
                std::ptr::null(),
                64,
                64,
                0,
                0,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("acquire_texture: null gpu handle"), "got: {msg}");
    }

    #[test]
    #[serial]
    fn acquire_texture_returns_error_on_null_out_pooled_handle() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_texture)(
                handle,
                64,
                64,
                0,
                0,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_texture: null out_pooled_handle"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn acquire_texture_returns_error_on_invalid_format_raw() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_texture)(
                handle,
                64,
                64,
                99, // invalid format_raw
                0,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_texture: invalid format_raw"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- resolve_texture_by_surface_id ---

    #[test]
    fn resolve_texture_by_surface_id_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_texture_by_surface_id)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_texture_by_surface_id: null gpu handle"),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn resolve_texture_by_surface_id_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_texture_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_texture_by_surface_id: null out_texture"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn resolve_texture_by_surface_id_returns_error_on_invalid_utf8() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        // 0xFF, 0xFF, 0xFF is invalid UTF-8.
        let id: [u8; 3] = [0xFF, 0xFF, 0xFF];
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_texture_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_texture_by_surface_id: surface_id not valid UTF-8"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- resolve_texture_registration_by_surface_id ---

    #[test]
    fn resolve_texture_registration_by_surface_id_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .resolve_texture_registration_by_surface_id)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains(
                "resolve_texture_registration_by_surface_id: null gpu handle"
            ),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn resolve_texture_registration_by_surface_id_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .resolve_texture_registration_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains(
                "resolve_texture_registration_by_surface_id: null out_registration"
            ),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn resolve_texture_registration_by_surface_id_returns_error_on_invalid_utf8() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id: [u8; 3] = [0xFF, 0xFF, 0xFF];
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .resolve_texture_registration_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains(
                "resolve_texture_registration_by_surface_id: surface_id not valid UTF-8"
            ),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- acquire_{storage,uniform,vertex,index}_buffer ---
    // Linux: null gpu handle / null out_buffer → rc=1 + per-slot msg.
    // Non-Linux: always rc=1 + "not available on this platform".

    #[cfg(target_os = "linux")]
    mod buffer_acquire_linux {
        use super::*;

        macro_rules! buffer_acquire_null_gpu_test {
            ($test_name:ident, $field:ident, $err_marker:expr) => {
                #[test]
                fn $test_name() {
                    let (mut buf, mut len) = make_err_buf();
                    let mut out_storage = [0u8; 256];
                    let rc = unsafe {
                        (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.$field)(
                            std::ptr::null(),
                            1024,
                            out_storage.as_mut_ptr() as *mut c_void,
                            buf.as_mut_ptr(),
                            buf.len(),
                            &mut len,
                        )
                    };
                    assert_eq!(rc, 1);
                    let msg = err_buf_as_str(&buf, len);
                    assert!(msg.contains($err_marker), "got: {msg}");
                }
            };
        }

        buffer_acquire_null_gpu_test!(
            acquire_storage_buffer_returns_error_on_null_gpu_handle,
            acquire_storage_buffer,
            "acquire_storage_buffer: null gpu handle"
        );
        buffer_acquire_null_gpu_test!(
            acquire_uniform_buffer_returns_error_on_null_gpu_handle,
            acquire_uniform_buffer,
            "acquire_uniform_buffer: null gpu handle"
        );
        buffer_acquire_null_gpu_test!(
            acquire_vertex_buffer_returns_error_on_null_gpu_handle,
            acquire_vertex_buffer,
            "acquire_vertex_buffer: null gpu handle"
        );
        buffer_acquire_null_gpu_test!(
            acquire_index_buffer_returns_error_on_null_gpu_handle,
            acquire_index_buffer,
            "acquire_index_buffer: null gpu handle"
        );

        macro_rules! buffer_acquire_null_out_test {
            ($test_name:ident, $field:ident, $err_marker:expr) => {
                #[test]
                #[serial]
                fn $test_name() {
                    let Some((handle, _arc)) = make_host_handle() else {
                        return;
                    };
                    let (mut buf, mut len) = make_err_buf();
                    let rc = unsafe {
                        (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.$field)(
                            handle,
                            1024,
                            std::ptr::null_mut(),
                            buf.as_mut_ptr(),
                            buf.len(),
                            &mut len,
                        )
                    };
                    assert_eq!(rc, 1);
                    let msg = err_buf_as_str(&buf, len);
                    assert!(msg.contains($err_marker), "got: {msg}");
                    unsafe { free_host_handle(handle) };
                }
            };
        }

        buffer_acquire_null_out_test!(
            acquire_storage_buffer_returns_error_on_null_out_buffer,
            acquire_storage_buffer,
            "acquire_storage_buffer: null out_buffer"
        );
        buffer_acquire_null_out_test!(
            acquire_uniform_buffer_returns_error_on_null_out_buffer,
            acquire_uniform_buffer,
            "acquire_uniform_buffer: null out_buffer"
        );
        buffer_acquire_null_out_test!(
            acquire_vertex_buffer_returns_error_on_null_out_buffer,
            acquire_vertex_buffer,
            "acquire_vertex_buffer: null out_buffer"
        );
        buffer_acquire_null_out_test!(
            acquire_index_buffer_returns_error_on_null_out_buffer,
            acquire_index_buffer,
            "acquire_index_buffer: null out_buffer"
        );
    }

    #[cfg(not(target_os = "linux"))]
    mod buffer_acquire_non_linux {
        use super::*;

        macro_rules! buffer_acquire_not_available_test {
            ($test_name:ident, $field:ident, $err_marker:expr) => {
                #[test]
                fn $test_name() {
                    let (mut buf, mut len) = make_err_buf();
                    let mut out_storage = [0u8; 256];
                    let rc = unsafe {
                        (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.$field)(
                            std::ptr::null(),
                            1024,
                            out_storage.as_mut_ptr() as *mut c_void,
                            buf.as_mut_ptr(),
                            buf.len(),
                            &mut len,
                        )
                    };
                    assert_eq!(rc, 1);
                    let msg = err_buf_as_str(&buf, len);
                    assert!(msg.contains($err_marker), "got: {msg}");
                }
            };
        }

        buffer_acquire_not_available_test!(
            acquire_storage_buffer_reports_not_available,
            acquire_storage_buffer,
            "not available on this platform"
        );
        buffer_acquire_not_available_test!(
            acquire_uniform_buffer_reports_not_available,
            acquire_uniform_buffer,
            "not available on this platform"
        );
        buffer_acquire_not_available_test!(
            acquire_vertex_buffer_reports_not_available,
            acquire_vertex_buffer,
            "not available on this platform"
        );
        buffer_acquire_not_available_test!(
            acquire_index_buffer_reports_not_available,
            acquire_index_buffer,
            "not available on this platform"
        );
    }

    // --- create_command_buffer_from_queue ---

    #[test]
    fn create_command_buffer_from_queue_returns_error_on_null_queue_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.create_command_buffer_from_queue)(
                std::ptr::null(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("create_command_buffer_from_queue: null queue handle"),
            "got: {msg}"
        );
    }

    // --- copy_pixel_buffer_to_texture ---
    // Linux: tier-1 cover; non-Linux: stub returns "not available".

    #[cfg(target_os = "linux")]
    #[test]
    fn copy_pixel_buffer_to_texture_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.copy_pixel_buffer_to_texture)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("copy_pixel_buffer_to_texture: null gpu handle"),
            "got: {msg}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial]
    fn copy_pixel_buffer_to_texture_returns_error_on_null_pixel_buffer_or_texture() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.copy_pixel_buffer_to_texture)(
                handle,
                std::ptr::null(),
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains(
                "copy_pixel_buffer_to_texture: null pixel_buffer or texture"
            ),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn copy_pixel_buffer_to_texture_reports_not_available_on_non_linux() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.copy_pixel_buffer_to_texture)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("copy_pixel_buffer_to_texture: not available on this platform"),
            "got: {msg}"
        );
    }

    // --- blit_copy ---

    #[test]
    fn blit_copy_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.blit_copy)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("blit_copy: null gpu handle"), "got: {msg}");
    }

    #[test]
    #[serial]
    fn blit_copy_returns_error_on_null_src_or_dst() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.blit_copy)(
                handle,
                std::ptr::null(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("blit_copy: null src or dst"), "got: {msg}");
        unsafe { free_host_handle(handle) };
    }

    // --- blit_copy_iosurface ---
    // macOS-only behaviour: null gpu / null dst → per-cause err.
    // Non-macOS: stub returns "not available on this platform (macOS-only)".

    #[cfg(target_os = "macos")]
    #[test]
    fn blit_copy_iosurface_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.blit_copy_iosurface)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("blit_copy_iosurface: null gpu handle"),
            "got: {msg}"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn blit_copy_iosurface_reports_not_available_on_non_macos() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.blit_copy_iosurface)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("blit_copy_iosurface: not available on this platform"),
            "got: {msg}"
        );
    }

    // --- check_out_surface ---

    #[test]
    fn check_out_surface_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.check_out_surface)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("check_out_surface: null gpu handle"),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn check_out_surface_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.check_out_surface)(
                handle,
                id.as_ptr(),
                id.len(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("check_out_surface: null out_pixel_buffer"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn check_out_surface_returns_error_on_invalid_utf8() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id: [u8; 3] = [0xFF, 0xFF, 0xFF];
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.check_out_surface)(
                handle,
                id.as_ptr(),
                id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("check_out_surface: surface_id not valid UTF-8"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- acquire_pixel_buffer ---

    #[test]
    fn acquire_pixel_buffer_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut id_buf = [0u8; 256];
        let mut id_len: usize = 0;
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_pixel_buffer)(
                std::ptr::null(),
                64,
                64,
                0x42475241, // valid Bgra32
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_pixel_buffer: null gpu handle"),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn acquire_pixel_buffer_returns_error_on_null_out_pixel_buffer() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let mut id_buf = [0u8; 256];
        let mut id_len: usize = 0;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_pixel_buffer)(
                handle,
                64,
                64,
                0x42475241,
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_pixel_buffer: null out_pixel_buffer"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn acquire_pixel_buffer_returns_error_on_invalid_format_raw() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let mut id_buf = [0u8; 256];
        let mut id_len: usize = 0;
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_pixel_buffer)(
                handle,
                64,
                64,
                0xDEAD_BEEF, // invalid format_raw
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_pixel_buffer: invalid format_raw"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- get_pixel_buffer ---

    #[test]
    fn get_pixel_buffer_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let pool_id = b"pool-x";
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.get_pixel_buffer)(
                std::ptr::null(),
                pool_id.as_ptr(),
                pool_id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("get_pixel_buffer: null gpu handle"), "got: {msg}");
    }

    #[test]
    #[serial]
    fn get_pixel_buffer_returns_error_on_null_out_pixel_buffer() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let pool_id = b"pool-x";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.get_pixel_buffer)(
                handle,
                pool_id.as_ptr(),
                pool_id.len(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("get_pixel_buffer: null out_pixel_buffer"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn get_pixel_buffer_returns_error_on_invalid_utf8_pool_id() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let pool_id: [u8; 3] = [0xFF, 0xFF, 0xFF];
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.get_pixel_buffer)(
                handle,
                pool_id.as_ptr(),
                pool_id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("get_pixel_buffer: pool_id not valid UTF-8"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- resolve_pixel_buffer_by_surface_id ---

    #[test]
    fn resolve_pixel_buffer_by_surface_id_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_pixel_buffer_by_surface_id)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_pixel_buffer_by_surface_id: null gpu handle"),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn resolve_pixel_buffer_by_surface_id_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_pixel_buffer_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_pixel_buffer_by_surface_id: null out_pixel_buffer"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn resolve_pixel_buffer_by_surface_id_returns_error_on_invalid_utf8() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id: [u8; 3] = [0xFF, 0xFF, 0xFF];
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_pixel_buffer_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains(
                "resolve_pixel_buffer_by_surface_id: surface_id not valid UTF-8"
            ),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // ------------------------------------------------------------------
    // Helpers — build a host-mode `gpu_handle` so the null-out / invalid-
    // input branches downstream of the null-handle guard can fire.
    //
    // Tests that take a real GpuContext are inherently unsafe in the
    // workspace lib suite when other tests construct VkDevices
    // concurrently (NVIDIA dual-VkDevice SIGSEGV per
    // `docs/learnings/nvidia-dual-vulkan-device-crash.md`). The
    // escalate-vtable tests use `#[serial]` for that reason. Tier-1
    // wire-format checks here either pass `null` (no GpuContext needed)
    // or build a fresh GpuContext per test — the latter case is
    // tolerated to be skipped via `init_for_platform` returning Err on
    // hosts without a GPU; subsequent calls then short-circuit the
    // test via early `return`. The host-handle-using tests do NOT race
    // because they never create a second VkDevice concurrently with the
    // serial escalate suite — the same `make_host_handle` shape used
    // there is reused here for symmetry.
    // ------------------------------------------------------------------

    fn make_host_handle() -> Option<(*const c_void, Arc<crate::core::context::GpuContext>)> {
        let gpu = crate::core::context::GpuContext::init_for_platform().ok()?;
        let arc = Arc::new(gpu);
        let boxed: Box<Arc<crate::core::context::GpuContext>> = Box::new(Arc::clone(&arc));
        let handle = Box::into_raw(boxed) as *const c_void;
        Some((handle, arc))
    }

    unsafe fn free_host_handle(handle: *const c_void) {
        let _ = unsafe {
            Box::from_raw(handle as *mut Arc<crate::core::context::GpuContext>)
        };
    }
}

#[cfg(test)]
mod runtime_context_vtable_null_handle_guards {
    //! Regression locks for the null-handle guards added to the
    //! `RuntimeContextVTable` callbacks. Each test calls the wrapper
    //! with a null `ctx` and asserts the documented default return
    //! value (matching `run_host_extern_c`'s panic-default). Without
    //! the guard the wrapper would deref a null `*const RuntimeContext`
    //! before returning, SIGSEGVing the test runner.
    //!
    //! Mental-revert check: removing any guard reverts the wrapper to
    //! `unsafe { &*(null) }` then a field read on the resulting
    //! reference — SIGSEGV, test failure (process abort) rather than
    //! the documented default.
    //!
    //! Lives in this PR alongside the source change so the engine-
    //! level fix and its test land together (the test backfill in
    //! PR B / #960 then layers the broader tier-1 coverage on top of
    //! these guards).

    use super::*;

    #[test]
    fn runtime_id_copy_returns_zero_on_null_ctx() {
        let mut out = [0u8; 16];
        let mut len: usize = 999;
        let n = unsafe {
            (HOST_RUNTIME_CONTEXT_VTABLE.runtime_id_copy)(
                std::ptr::null(),
                out.as_mut_ptr(),
                out.len(),
                &mut len,
            )
        };
        assert_eq!(n, 0);
        assert_eq!(len, 0, "out_len must be cleared on null ctx");
    }

    #[test]
    fn processor_id_copy_returns_minus_one_on_null_ctx() {
        let mut out = [0u8; 16];
        let mut len: usize = 999;
        let n = unsafe {
            (HOST_RUNTIME_CONTEXT_VTABLE.processor_id_copy)(
                std::ptr::null(),
                out.as_mut_ptr(),
                out.len(),
                &mut len,
            )
        };
        assert_eq!(n, -1, "-1 encodes Option::None");
        assert_eq!(len, 0, "out_len must be cleared on null ctx");
    }

    #[test]
    fn is_paused_returns_true_on_null_ctx() {
        let v = unsafe { (HOST_RUNTIME_CONTEXT_VTABLE.is_paused)(std::ptr::null()) };
        assert!(v, "pause-on-failure is the conservative default");
    }

    #[test]
    fn should_process_returns_false_on_null_ctx() {
        let v = unsafe {
            (HOST_RUNTIME_CONTEXT_VTABLE.should_process)(std::ptr::null())
        };
        assert!(!v, "halt-on-failure is the conservative default");
    }

    /// Locks the documented placeholder behaviour of
    /// `gpu_full_access`: the wrapper ignores `ctx` and returns null
    /// unconditionally because cross-DSO FullAccess wiring lives on
    /// the inline-by-value shim today, not through this callback.
    /// This is NOT a null-handle-guard lock (no guard to revert);
    /// it's a placeholder-shape lock — if a future change wires
    /// real FullAccess dispatch here, this test fails and forces
    /// the implementor to revisit.
    #[test]
    fn gpu_full_access_returns_null_unconditionally_today() {
        let p = unsafe {
            (HOST_RUNTIME_CONTEXT_VTABLE.gpu_full_access)(std::ptr::null())
        };
        assert!(p.is_null());
    }

    /// Companion to [`gpu_full_access_returns_null_unconditionally_today`].
    /// Same placeholder-shape lock; same caveat (not a null-handle
    /// guard — the wrapper ignores `_ctx`).
    #[test]
    fn gpu_limited_access_returns_null_unconditionally_today() {
        let p = unsafe {
            (HOST_RUNTIME_CONTEXT_VTABLE.gpu_limited_access)(std::ptr::null())
        };
        assert!(p.is_null());
    }

    #[test]
    fn audio_clock_handle_returns_null_on_null_ctx() {
        let p = unsafe {
            (HOST_RUNTIME_CONTEXT_VTABLE.audio_clock_handle)(std::ptr::null())
        };
        assert!(p.is_null());
    }

    #[test]
    fn runtime_ops_handle_returns_null_on_null_ctx() {
        let p = unsafe {
            (HOST_RUNTIME_CONTEXT_VTABLE.runtime_ops_handle)(std::ptr::null())
        };
        assert!(p.is_null());
    }
}

#[cfg(test)]
mod audio_clock_vtable_null_handle_guards {
    //! Regression locks for the null-handle guards added to the
    //! `AudioClockVTable` callbacks. Same shape as the
    //! `RuntimeContextVTable` guards module: mental-revert removes
    //! the guard, the wrapper SIGSEGVs the test runner.
    //!
    //! `on_tick`'s guard additionally invokes `drop_user_data` so the
    //! cdylib's boxed `user_data` doesn't leak — verified via a
    //! `Drop`-counting fixture.

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn sample_rate_returns_zero_on_null_handle() {
        let v = unsafe { (HOST_AUDIO_CLOCK_VTABLE.sample_rate)(std::ptr::null()) };
        assert_eq!(v, 0);
    }

    #[test]
    fn buffer_size_returns_zero_on_null_handle() {
        let v = unsafe { (HOST_AUDIO_CLOCK_VTABLE.buffer_size)(std::ptr::null()) };
        assert_eq!(v, 0);
    }

    /// Counter shared with the on_tick test's `drop_user_data` callback
    /// so the test can assert the user_data reclamation actually fires.
    static ON_TICK_DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn dummy_tick_callback(
        _user_data: *mut c_void,
        _ctx: streamlib_plugin_abi::AudioTickContextRepr,
    ) {
        // Never fires in the null-handle test — the host short-circuits
        // before registering.
    }

    unsafe extern "C" fn counting_drop_user_data(user_data: *mut c_void) {
        ON_TICK_DROP_COUNT.fetch_add(1, Ordering::SeqCst);
        // SAFETY: in this test we leaked a `Box<u8>` into user_data;
        // reclaim it here.
        if !user_data.is_null() {
            unsafe {
                let _ = Box::from_raw(user_data as *mut u8);
            }
        }
    }

    #[test]
    fn on_tick_drops_user_data_on_null_handle() {
        // Mental-revert: without the null-handle guard the wrapper
        // would still construct the bridge (now hoisted above the
        // null check), then deref a null `*const SharedAudioClock`
        // to call `clock.on_tick(...)` and SIGSEGV before the bridge
        // could move into `cb`. With the guard, the bridge drops
        // before the deref → `drop_user_data` fires exactly once.
        //
        // Mental-revert for the bigger fix in the same commit
        // (removing `drop_user_data` from the wrapper's third arg
        // and hoisting bridge construction above the null check):
        // restoring the old third-arg block-expression cleanup would
        // re-introduce the eager-arg-eval double-fire on every
        // call (success or null) — this test would observe
        // `after == before + 2` and fail.
        let before = ON_TICK_DROP_COUNT.load(Ordering::SeqCst);
        // Leak a Box<u8> so counting_drop_user_data has something to
        // reclaim (mirrors cdylib's Box<oneshot::Sender>-shaped pattern).
        let user_data = Box::into_raw(Box::new(0u8)) as *mut c_void;
        unsafe {
            (HOST_AUDIO_CLOCK_VTABLE.on_tick)(
                std::ptr::null(),
                dummy_tick_callback,
                user_data,
                counting_drop_user_data,
            );
        }
        let after = ON_TICK_DROP_COUNT.load(Ordering::SeqCst);
        assert_eq!(
            after,
            before + 1,
            "drop_user_data must fire exactly once on null-handle short-circuit"
        );
    }

    /// Success-path companion to the null-handle test. Exercises the
    /// real `clock.on_tick(...)` storage path with a tiny ad-hoc
    /// `SharedAudioClock` and asserts `drop_user_data` fires exactly
    /// once across the full lifecycle (registration → clock drop).
    /// Locks the eager-arg-eval double-free fix: restoring the old
    /// third-arg block-expression cleanup would observe `after ==
    /// before + 2` and fail this test.
    #[test]
    fn on_tick_drops_user_data_exactly_once_on_success_path() {
        use crate::core::context::{AudioClockConfig, SharedAudioClock, SoftwareAudioClock};
        use std::sync::Arc as StdArc;
        let before = ON_TICK_DROP_COUNT.load(Ordering::SeqCst);
        let user_data = Box::into_raw(Box::new(0u8)) as *mut c_void;
        // Build a tiny clock just for this test. Drops at the end
        // of the function, firing the bridge's Drop (which fires
        // drop_user_data exactly once if the fix holds).
        let clock: SharedAudioClock = StdArc::new(SoftwareAudioClock::new(
            AudioClockConfig::new(48_000, 512),
        ));
        let handle = &clock as *const SharedAudioClock as *const c_void;
        unsafe {
            (HOST_AUDIO_CLOCK_VTABLE.on_tick)(
                handle,
                dummy_tick_callback,
                user_data,
                counting_drop_user_data,
            );
        }
        // Drop the clock before reading the counter so the bridge's
        // Drop fires deterministically.
        drop(clock);
        let after = ON_TICK_DROP_COUNT.load(Ordering::SeqCst);
        assert_eq!(
            after,
            before + 1,
            "drop_user_data must fire exactly once across the full \
             on_tick lifecycle (registration → clock drop); \
             `before + 2` indicates the eager-arg-eval double-free \
             regressed"
        );
    }
}

#[cfg(test)]
mod runtime_ops_vtable_null_handle_guards {
    //! Regression locks for the null-handle guards added to the
    //! `RuntimeOpsVTable` callbacks. Each callback is
    //! submit-with-completion (void return + completion callback):
    //! the contract is that completion fires exactly once. Null
    //! handle must fire the completion with `status = -1` and an
    //! error message identifying the offending op — mental-revert
    //! removes the guard, the wrapper SIGSEGVs through
    //! `&*(null as *const Arc<dyn RuntimeOperations>)`.
    //!
    //! Each test installs a tiny completion that pushes
    //! `(status, message)` into a shared queue; the assertion
    //! confirms a single error completion fired with the expected
    //! per-op marker.

    use super::*;
    use std::sync::{Arc as StdArc, Mutex};

    struct CompletionSink {
        events: Mutex<Vec<(i32, Vec<u8>)>>,
    }

    impl CompletionSink {
        fn new() -> StdArc<Self> {
            StdArc::new(Self { events: Mutex::new(Vec::new()) })
        }
    }

    unsafe extern "C" fn record_completion(
        user_data: *mut c_void,
        status: i32,
        result_ptr: *const u8,
        result_len: usize,
    ) {
        let sink_arc = unsafe { StdArc::from_raw(user_data as *const CompletionSink) };
        let payload = if result_len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(result_ptr, result_len) }.to_vec()
        };
        sink_arc.events.lock().expect("poisoned").push((status, payload));
        // Re-leak so the host's CompletionGuard's Drop (if it fires
        // again — it shouldn't, but defensive) can still find it.
        // In practice the guard's `fire_err_msg` consumes via `mut`,
        // so this re-leak is just paranoia matching the cdylib's
        // RAII-trampoline shape.
        let _ = StdArc::into_raw(sink_arc);
    }

    fn install_sink_user_data() -> (*mut c_void, StdArc<CompletionSink>) {
        let sink = CompletionSink::new();
        let user_data = StdArc::into_raw(StdArc::clone(&sink)) as *mut c_void;
        (user_data, sink)
    }

    fn assert_single_err_completion(sink: &CompletionSink, expected_marker: &str) {
        let events = sink.events.lock().expect("poisoned");
        assert_eq!(events.len(), 1, "expected exactly one completion fire");
        let (status, payload) = &events[0];
        assert_eq!(*status, -1, "null-handle must produce err status");
        let msg = std::str::from_utf8(payload).expect("UTF-8");
        assert!(
            msg.contains(expected_marker),
            "expected marker `{expected_marker}` in msg: {msg}"
        );
    }

    /// After each test the test's CompletionSink Arc still holds one
    /// extra refcount (the original `StdArc::into_raw` we passed as
    /// user_data, never reclaimed by the host on the null-handle
    /// path). Reclaim it explicitly so the sink doesn't leak.
    unsafe fn reclaim_sink(user_data: *mut c_void) {
        let _ = unsafe { StdArc::from_raw(user_data as *const CompletionSink) };
    }

    #[test]
    fn add_processor_fires_error_completion_on_null_handle() {
        let (user_data, sink) = install_sink_user_data();
        unsafe {
            (HOST_RUNTIME_OPS_VTABLE.add_processor)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                record_completion,
                user_data,
            );
        }
        assert_single_err_completion(&sink, "add_processor: null handle");
        unsafe { reclaim_sink(user_data) };
    }

    #[test]
    fn remove_processor_fires_error_completion_on_null_handle() {
        let (user_data, sink) = install_sink_user_data();
        unsafe {
            (HOST_RUNTIME_OPS_VTABLE.remove_processor)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                record_completion,
                user_data,
            );
        }
        assert_single_err_completion(&sink, "remove_processor: null handle");
        unsafe { reclaim_sink(user_data) };
    }

    #[test]
    fn connect_fires_error_completion_on_null_handle() {
        let (user_data, sink) = install_sink_user_data();
        unsafe {
            (HOST_RUNTIME_OPS_VTABLE.connect)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                record_completion,
                user_data,
            );
        }
        assert_single_err_completion(&sink, "connect: null handle");
        unsafe { reclaim_sink(user_data) };
    }

    #[test]
    fn disconnect_fires_error_completion_on_null_handle() {
        let (user_data, sink) = install_sink_user_data();
        unsafe {
            (HOST_RUNTIME_OPS_VTABLE.disconnect)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                record_completion,
                user_data,
            );
        }
        assert_single_err_completion(&sink, "disconnect: null handle");
        unsafe { reclaim_sink(user_data) };
    }

    #[test]
    fn to_json_fires_error_completion_on_null_handle() {
        let (user_data, sink) = install_sink_user_data();
        unsafe {
            (HOST_RUNTIME_OPS_VTABLE.to_json)(
                std::ptr::null(),
                record_completion,
                user_data,
            );
        }
        assert_single_err_completion(&sink, "to_json: null handle");
        unsafe { reclaim_sink(user_data) };
    }
}

#[cfg(test)]
mod surface_store_vtable_tier1_wire_format_tests {
    //! Tier-1 wire-format tests for [`HOST_SURFACE_STORE_VTABLE`].
    //!
    //! Every callback on the vtable goes through `ss_inner`, which
    //! already short-circuits on a null handle. This module covers
    //! the full tier-1 contract:
    //!
    //! - `layout_version_matches_constant` — locks the wire-format
    //!   layout version against the cdylib-visible constant.
    //! - `clone_handle` / `drop_handle` null-handle locks — the
    //!   Arc-lifecycle pair.
    //! - For each result-returning callback (10 of them): null-
    //!   handle → rc=1 with per-callback err marker.
    //! - For the 4 Linux-only callbacks (`register_texture`,
    //!   `register_pixel_buffer_with_timeline`, `lookup_texture`,
    //!   `update_image_layout`): same Linux contract; non-Linux
    //!   stubs return rc=1 with "not available on this platform".
    //!
    //! Mental-revert: removing the null-handle guard from
    //! `ss_inner` makes every result-returning test SIGSEGV (the
    //! wrapper would deref a null `*const SurfaceStoreInner`). The
    //! per-callback inner null checks (`if pixel_buffer.is_null()`,
    //! `if texture.is_null()`, `if out_pixel_buffer.is_null()`,
    //! `if out_texture.is_null() || out_layout_raw.is_null()`) are
    //! NOT individually locked by this module — tier-1 scope is the
    //! `ss_inner` null-handle guard plus the layout-version match
    //! plus the per-callback err-marker text. The per-arg inner
    //! checks belong to a deeper coverage tier.

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    // ------------------------------------------------------------------
    // Layout-version match
    // ------------------------------------------------------------------

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_SURFACE_STORE_VTABLE.layout_version,
            streamlib_plugin_abi::SURFACE_STORE_VTABLE_LAYOUT_VERSION,
        );
    }

    // ------------------------------------------------------------------
    // Handle-lifecycle (clone_handle / drop_handle)
    // ------------------------------------------------------------------

    #[test]
    fn clone_handle_handles_null_no_crash() {
        unsafe {
            (HOST_SURFACE_STORE_VTABLE.clone_handle)(std::ptr::null());
        }
    }

    #[test]
    fn drop_handle_handles_null_no_crash() {
        unsafe {
            (HOST_SURFACE_STORE_VTABLE.drop_handle)(std::ptr::null());
        }
    }

    // ------------------------------------------------------------------
    // Result-returning callbacks: null-handle returns rc=1 with err msg
    // ------------------------------------------------------------------

    #[test]
    fn connect_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.connect)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("connect: null handle"));
    }

    #[test]
    fn disconnect_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.disconnect)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("disconnect: null handle"));
    }

    #[test]
    fn check_in_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut id_buf = [0u8; 64];
        let mut id_len: usize = 0;
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.check_in)(
                std::ptr::null(),
                std::ptr::null(),
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("check_in: null handle"));
    }

    #[test]
    fn check_out_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out = [0u8; 256];
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.check_out)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("check_out: null handle"));
    }

    #[test]
    fn register_buffer_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let pool_id = b"pool-x";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.register_buffer)(
                std::ptr::null(),
                pool_id.as_ptr(),
                pool_id.len(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("register_buffer: null handle"),
        );
    }

    #[test]
    fn lookup_buffer_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let pool_id = b"pool-x";
        let mut out = [0u8; 256];
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.lookup_buffer)(
                std::ptr::null(),
                pool_id.as_ptr(),
                pool_id.len(),
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("lookup_buffer: null handle"),
        );
    }

    #[test]
    fn release_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.release)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("release: null handle"));
    }

    // ------------------------------------------------------------------
    // Linux-only callbacks
    // ------------------------------------------------------------------

    #[cfg(target_os = "linux")]
    #[test]
    fn register_texture_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.register_texture)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
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
            err_buf_as_str(&buf, len).contains("register_texture: null handle"),
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn register_pixel_buffer_with_timeline_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.register_pixel_buffer_with_timeline)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                std::ptr::null(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len)
            .contains("register_pixel_buffer_with_timeline: null handle"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lookup_texture_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_tex = [0u8; 256];
        let mut out_layout: i32 = 0;
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.lookup_texture)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                out_tex.as_mut_ptr() as *mut c_void,
                &mut out_layout,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("lookup_texture: null handle"),
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn update_image_layout_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.update_image_layout)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len)
            .contains("update_image_layout: null handle"));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn register_texture_reports_not_available_on_non_linux() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.register_texture)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len)
            .contains("register_texture: not available on this platform"));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn register_pixel_buffer_with_timeline_reports_not_available_on_non_linux() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.register_pixel_buffer_with_timeline)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                std::ptr::null(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains(
            "register_pixel_buffer_with_timeline: not available on this platform"
        ));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn lookup_texture_reports_not_available_on_non_linux() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_tex = [0u8; 256];
        let mut out_layout: i32 = 0;
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.lookup_texture)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                out_tex.as_mut_ptr() as *mut c_void,
                &mut out_layout,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len)
            .contains("lookup_texture: not available on this platform"));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn update_image_layout_reports_not_available_on_non_linux() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_SURFACE_STORE_VTABLE.update_image_layout)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len)
            .contains("update_image_layout: not available on this platform"));
    }
}

#[cfg(test)]
mod runtime_context_vtable_tier1_wire_format_tests {
    //! Tier-1 wire-format tests for [`HOST_RUNTIME_CONTEXT_VTABLE`].
    //!
    //! Per-callback null-handle coverage lives in
    //! [`runtime_context_vtable_null_handle_guards`] above — that
    //! module landed alongside the engine PR that added the
    //! null-handle guards each test relies on (mental-revert removes
    //! the guard, the wrapper SIGSEGVs the test runner). This module
    //! completes the tier-1 set with the wire-format invariant the
    //! null-handle suite doesn't cover: the static vtable's
    //! `layout_version` field must match the constant cdylibs read
    //! against.
    //!
    //! No callback on `RuntimeContextVTable` takes an out-param or
    //! a variant-typed input, so the "null out-param" and
    //! "invalid input" tier-1 categories don't apply here.

    use super::*;

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_RUNTIME_CONTEXT_VTABLE.layout_version,
            streamlib_plugin_abi::RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION,
        );
    }
}

#[cfg(test)]
mod audio_clock_vtable_tier1_wire_format_tests {
    //! Tier-1 wire-format tests for [`HOST_AUDIO_CLOCK_VTABLE`].
    //!
    //! Per-callback null-handle coverage lives in
    //! [`audio_clock_vtable_null_handle_guards`] above (3 tests —
    //! `sample_rate`, `buffer_size`, `on_tick`). The on_tick
    //! single-fire invariant is locked twice over: once on the
    //! null-handle path, once on the success path against a real
    //! `SoftwareAudioClock`. This module adds the
    //! `layout_version_matches_constant` lock.
    //!
    //! `sample_rate` / `buffer_size` are primitive-returning and
    //! take no out-param; `on_tick` takes a callback trio whose
    //! ownership semantics are covered by the null-handle and
    //! success-path tests in the guards module. The "null
    //! out-param" / "invalid input" tier-1 categories don't apply.

    use super::*;

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_AUDIO_CLOCK_VTABLE.layout_version,
            streamlib_plugin_abi::AUDIO_CLOCK_VTABLE_LAYOUT_VERSION,
        );
    }
}

#[cfg(test)]
mod runtime_ops_vtable_tier1_wire_format_tests {
    //! Tier-1 wire-format tests for [`HOST_RUNTIME_OPS_VTABLE`].
    //!
    //! Per-callback null-handle coverage for the 5
    //! submit-with-completion ops (`add_processor`,
    //! `remove_processor`, `connect`, `disconnect`, `to_json`)
    //! lives in [`runtime_ops_vtable_null_handle_guards`] above.
    //! This module adds:
    //!
    //! - `layout_version_matches_constant` — locks the v2 layout
    //!   version against the cdylib-visible constant.
    //! - `clone_handle` / `drop_handle` null-handle coverage — the
    //!   v2 Arc-lifecycle pair already had explicit guards
    //!   (`if owned_handle.is_null() { return; }`); we test that the
    //!   contract holds.
    //! - `CompletionGuard` fire-exactly-once contract — the host-
    //!   side RAII guard around the cdylib's "completion fires
    //!   exactly once" promise. Two cases:
    //!     - Drop without fire → abort completion fires with
    //!       `status = -1` and the documented aborted-task message.
    //!     - `fire_err_msg` then drop → completion fires once, Drop
    //!       does NOT fire a second time.

    use super::*;
    use std::sync::{Arc as StdArc, Mutex};

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_RUNTIME_OPS_VTABLE.layout_version,
            streamlib_plugin_abi::RUNTIME_OPS_VTABLE_LAYOUT_VERSION,
        );
    }

    #[test]
    fn clone_handle_returns_null_on_null_borrowed() {
        let out = unsafe {
            (HOST_RUNTIME_OPS_VTABLE.clone_handle)(std::ptr::null())
        };
        assert!(out.is_null());
    }

    #[test]
    fn drop_handle_handles_null_owned_no_crash() {
        unsafe {
            (HOST_RUNTIME_OPS_VTABLE.drop_handle)(std::ptr::null());
        }
    }

    // ------------------------------------------------------------------
    // CompletionGuard fire-exactly-once contract
    // ------------------------------------------------------------------

    struct CompletionSink {
        events: Mutex<Vec<(i32, Vec<u8>)>>,
    }

    impl CompletionSink {
        fn new() -> StdArc<Self> {
            StdArc::new(Self { events: Mutex::new(Vec::new()) })
        }
    }

    unsafe extern "C" fn record_completion(
        user_data: *mut c_void,
        status: i32,
        result_ptr: *const u8,
        result_len: usize,
    ) {
        let sink_arc = unsafe { StdArc::from_raw(user_data as *const CompletionSink) };
        let payload = if result_len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(result_ptr, result_len) }.to_vec()
        };
        sink_arc.events.lock().expect("poisoned").push((status, payload));
        let _ = StdArc::into_raw(sink_arc);
    }

    fn install_sink_user_data() -> (*mut c_void, StdArc<CompletionSink>) {
        let sink = CompletionSink::new();
        let user_data = StdArc::into_raw(StdArc::clone(&sink)) as *mut c_void;
        (user_data, sink)
    }

    unsafe fn reclaim_sink(user_data: *mut c_void) {
        let _ = unsafe { StdArc::from_raw(user_data as *const CompletionSink) };
    }

    #[test]
    fn completion_guard_drop_without_fire_fires_aborted_completion() {
        // Mental-revert: removing the `if !self.fired` branch in
        // CompletionGuard::Drop reverts to silent drop on un-fired
        // guards. The cdylib's `rx.await` then hangs forever instead
        // of returning the aborted-task error. This test would fail
        // because `events` would be empty.
        let (user_data, sink) = install_sink_user_data();
        {
            let _guard = CompletionGuard::new(record_completion, user_data);
            // Drop without firing.
        }
        let events = sink.events.lock().expect("poisoned");
        assert_eq!(events.len(), 1, "Drop must fire exactly one completion");
        let (status, payload) = &events[0];
        assert_eq!(*status, -1, "aborted completion uses status -1");
        let msg = std::str::from_utf8(payload).expect("UTF-8");
        assert!(
            msg.contains("runtime-ops host task aborted before completion"),
            "got: {msg}"
        );
        drop(events);
        unsafe { reclaim_sink(user_data) };
    }

    #[test]
    fn completion_guard_fire_then_drop_does_not_double_fire() {
        // Mental-revert: removing `self.fired = true;` from
        // fire_err_msg reverts to Drop firing again, this test
        // observes `events.len() == 2` and fails.
        let (user_data, sink) = install_sink_user_data();
        let guard = CompletionGuard::new(record_completion, user_data);
        guard.fire_err_msg(b"deliberate-test-msg");
        let events = sink.events.lock().expect("poisoned");
        assert_eq!(events.len(), 1, "fire_err_msg must fire exactly once");
        let (status, payload) = &events[0];
        assert_eq!(*status, -1);
        assert_eq!(payload, b"deliberate-test-msg");
        drop(events);
        unsafe { reclaim_sink(user_data) };
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
#[cfg(test)]
mod output_writer_vtable_tier1_wire_format_tests {
    use super::*;

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_OUTPUT_WRITER_VTABLE.layout_version,
            streamlib_plugin_abi::OUTPUT_WRITER_VTABLE_LAYOUT_VERSION,
        );
    }

    #[test]
    fn write_raw_returns_error_on_null_handle() {
        let mut err_buf = [0u8; 256];
        let mut err_len = 0usize;
        let port = b"any_port";
        let data = b"payload";
        let rc = unsafe {
            (HOST_OUTPUT_WRITER_VTABLE.write_raw)(
                std::ptr::null(),
                port.as_ptr(),
                port.len(),
                data.as_ptr(),
                data.len(),
                0,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        assert_eq!(rc, 1);
        let msg = std::str::from_utf8(&err_buf[..err_len]).unwrap();
        assert!(
            msg.contains("null OutputWriter handle"),
            "unexpected err message: {msg}"
        );
    }

    #[test]
    fn write_raw_returns_error_on_invalid_utf8_port() {
        let inner = std::sync::Arc::new(crate::iceoryx2::OutputWriterInner::new());
        let handle =
            std::sync::Arc::into_raw(inner) as *const std::ffi::c_void;
        let mut err_buf = [0u8; 256];
        let mut err_len = 0usize;
        let bad_port = b"\xff\xfe"; // not utf-8
        let data = b"payload";
        let rc = unsafe {
            (HOST_OUTPUT_WRITER_VTABLE.write_raw)(
                handle,
                bad_port.as_ptr(),
                bad_port.len(),
                data.as_ptr(),
                data.len(),
                0,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        assert_eq!(rc, 1);
        let msg = std::str::from_utf8(&err_buf[..err_len]).unwrap();
        assert!(
            msg.contains("port not UTF-8"),
            "unexpected err message: {msg}"
        );
        unsafe {
            std::sync::Arc::<crate::iceoryx2::OutputWriterInner>::decrement_strong_count(
                handle as *const _,
            );
        }
    }

    #[test]
    fn has_port_returns_false_on_null_handle() {
        let port = b"any_port";
        let result =
            unsafe { (HOST_OUTPUT_WRITER_VTABLE.has_port)(std::ptr::null(), port.as_ptr(), port.len()) };
        assert!(!result);
    }

    #[test]
    fn clone_arc_returns_null_on_null_handle() {
        let result =
            unsafe { (HOST_OUTPUT_WRITER_VTABLE.clone_arc)(std::ptr::null()) };
        assert!(result.is_null());
    }

    #[test]
    fn drop_arc_is_noop_on_null_handle() {
        // No panic, no segfault — the function returns cleanly.
        unsafe {
            (HOST_OUTPUT_WRITER_VTABLE.drop_arc)(std::ptr::null());
        }
    }

    /// End-to-end refcount accounting: clone_arc on a real Arc::into_raw
    /// handle bumps the strong count by one and returns the same handle;
    /// drop_arc decrements. Pair them and the inner survives until the
    /// last decrement.
    #[test]
    fn clone_drop_arc_balance_strong_count() {
        let inner = std::sync::Arc::new(crate::iceoryx2::OutputWriterInner::new());
        let inner_for_test = inner.clone();
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 2);
        let raw =
            std::sync::Arc::into_raw(inner) as *const std::ffi::c_void;
        // strong_count now 2 again (the into_raw handle counts).
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 2);

        let cloned = unsafe { (HOST_OUTPUT_WRITER_VTABLE.clone_arc)(raw) };
        assert_eq!(cloned, raw);
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 3);

        unsafe { (HOST_OUTPUT_WRITER_VTABLE.drop_arc)(cloned) };
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 2);

        unsafe { (HOST_OUTPUT_WRITER_VTABLE.drop_arc)(raw) };
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 1);
    }
}

/// Tier-1 wire-format tests for [`HOST_INPUT_MAILBOXES_VTABLE`] (issue
/// #894).
#[cfg(test)]
mod input_mailboxes_vtable_tier1_wire_format_tests {
    use super::*;

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_INPUT_MAILBOXES_VTABLE.layout_version,
            streamlib_plugin_abi::INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION,
        );
    }

    #[test]
    fn read_raw_returns_error_on_null_handle() {
        let mut buf = [0u8; 64];
        let mut out_len = 0usize;
        let mut out_ts = 0i64;
        let mut has_data = false;
        let mut err_buf = [0u8; 256];
        let mut err_len = 0usize;
        let port = b"any_port";
        let rc = unsafe {
            (HOST_INPUT_MAILBOXES_VTABLE.read_raw)(
                std::ptr::null(),
                port.as_ptr(),
                port.len(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut out_len as *mut usize,
                &mut out_ts as *mut i64,
                &mut has_data as *mut bool,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        assert_eq!(rc, 1);
        let msg = std::str::from_utf8(&err_buf[..err_len]).unwrap();
        assert!(
            msg.contains("null InputMailboxes handle"),
            "unexpected err message: {msg}"
        );
        assert!(!has_data);
        assert_eq!(out_len, 0);
    }

    #[test]
    fn read_raw_returns_error_on_invalid_utf8_port() {
        let inner = std::sync::Arc::new(crate::iceoryx2::InputMailboxesInner::new());
        let handle =
            std::sync::Arc::into_raw(inner) as *const std::ffi::c_void;
        let mut buf = [0u8; 64];
        let mut out_len = 0usize;
        let mut out_ts = 0i64;
        let mut has_data = false;
        let mut err_buf = [0u8; 256];
        let mut err_len = 0usize;
        let bad_port = b"\xff\xfe";
        let rc = unsafe {
            (HOST_INPUT_MAILBOXES_VTABLE.read_raw)(
                handle,
                bad_port.as_ptr(),
                bad_port.len(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut out_len as *mut usize,
                &mut out_ts as *mut i64,
                &mut has_data as *mut bool,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        assert_eq!(rc, 1);
        let msg = std::str::from_utf8(&err_buf[..err_len]).unwrap();
        assert!(
            msg.contains("port not UTF-8"),
            "unexpected err message: {msg}"
        );
        unsafe {
            std::sync::Arc::<crate::iceoryx2::InputMailboxesInner>::decrement_strong_count(
                handle as *const _,
            );
        }
    }

    #[test]
    fn read_raw_returns_no_data_on_empty_mailbox() {
        let inner = std::sync::Arc::new(crate::iceoryx2::InputMailboxesInner::new());
        inner.add_port(
            "p",
            8,
            crate::iceoryx2::ReadMode::ReadNextInOrder,
        );
        let handle =
            std::sync::Arc::into_raw(inner) as *const std::ffi::c_void;
        let mut buf = [0u8; 64];
        let mut out_len = 0usize;
        let mut out_ts = 0i64;
        let mut has_data = true; // start true to verify the wrapper sets it false
        let mut err_buf = [0u8; 256];
        let mut err_len = 0usize;
        let port = b"p";
        let rc = unsafe {
            (HOST_INPUT_MAILBOXES_VTABLE.read_raw)(
                handle,
                port.as_ptr(),
                port.len(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut out_len as *mut usize,
                &mut out_ts as *mut i64,
                &mut has_data as *mut bool,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        assert_eq!(rc, 0);
        assert!(!has_data);
        assert_eq!(out_len, 0);
        unsafe {
            std::sync::Arc::<crate::iceoryx2::InputMailboxesInner>::decrement_strong_count(
                handle as *const _,
            );
        }
    }

    #[test]
    fn has_data_returns_false_on_null_handle() {
        let port = b"any";
        let result = unsafe {
            (HOST_INPUT_MAILBOXES_VTABLE.has_data)(
                std::ptr::null(),
                port.as_ptr(),
                port.len(),
            )
        };
        assert!(!result);
    }

    #[test]
    fn clone_arc_returns_null_on_null_handle() {
        let result = unsafe { (HOST_INPUT_MAILBOXES_VTABLE.clone_arc)(std::ptr::null()) };
        assert!(result.is_null());
    }

    #[test]
    fn drop_arc_is_noop_on_null_handle() {
        unsafe { (HOST_INPUT_MAILBOXES_VTABLE.drop_arc)(std::ptr::null()) };
    }

    /// End-to-end refcount accounting: clone_arc bumps strong count
    /// and returns the same handle; drop_arc decrements. Mirrors the
    /// OutputWriter sibling test.
    #[test]
    fn clone_drop_arc_balance_strong_count() {
        let inner = std::sync::Arc::new(crate::iceoryx2::InputMailboxesInner::new());
        let inner_for_test = inner.clone();
        let raw =
            std::sync::Arc::into_raw(inner) as *const std::ffi::c_void;
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 2);
        let cloned = unsafe { (HOST_INPUT_MAILBOXES_VTABLE.clone_arc)(raw) };
        assert_eq!(cloned, raw);
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 3);
        unsafe { (HOST_INPUT_MAILBOXES_VTABLE.drop_arc)(cloned) };
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 2);
        unsafe { (HOST_INPUT_MAILBOXES_VTABLE.drop_arc)(raw) };
        assert_eq!(std::sync::Arc::strong_count(&inner_for_test), 1);
    }
}
