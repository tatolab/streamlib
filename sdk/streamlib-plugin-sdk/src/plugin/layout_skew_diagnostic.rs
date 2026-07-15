// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Fail-loud plugin-ABI version-skew diagnostic for `install_host_services`
//! (M32 #1253) — cdylib (engine-free SDK) arm.
//!
//! Before this, every refusal path in `install_host_services` was a bare
//! `return None`: the outer `HostServices.abi_layout_version` check plus
//! one check per inner vtable's `layout_version`. A cdylib built against a
//! skewed plugin-ABI train loaded into a mismatched host would surface only
//! a downstream "processor not registered" — never a version-skew message
//! naming the vtable that mismatched.
//!
//! [`validate_host_services_layout`] routes every one of those checks
//! through [`report_layout_skew`], which emits an operator-actionable error
//! naming the mismatched vtable + both layout versions. It uses the host's
//! `iceoryx_log_emit` callback — a leading, ABI-version-pinned field the
//! host loader has already validated (the plugin's `abi_version` is read at
//! offset 0 and refused before `register` runs, so within a matched
//! `STREAMLIB_ABI_VERSION` the leading callbacks sit at stable offsets even
//! when the appended `HostServices` fields have drifted). This is
//! deliberately NOT a `tracing` diagnostic: install runs before the cdylib's
//! tracing forwarder is wired, so a local `tracing::error!` would log into a
//! void — the `iceoryx_log_emit` callback routes to the host process.
//!
//! This file is a logic-identical twin of the engine's
//! `runtime/streamlib-engine/src/core/plugin/host_services/layout_skew_diagnostic.rs`
//! — the two are held in sync by `twin_drift_guard`. Apply every change to
//! BOTH.

use streamlib_plugin_abi::{
    AUDIO_CLOCK_VTABLE_LAYOUT_VERSION, GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION,
    GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION, HOST_SERVICES_LAYOUT_VERSION,
    HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE_LAYOUT_VERSION, HostLogLevel, HostServices,
    INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION, OUTPUT_WRITER_VTABLE_LAYOUT_VERSION,
    PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION,
    RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION,
    RHI_COMMAND_RECORDER_METHODS_VTABLE_LAYOUT_VERSION, RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION,
    RUNTIME_OPS_VTABLE_LAYOUT_VERSION, SURFACE_STORE_VTABLE_LAYOUT_VERSION,
    TEXTURE_RING_METHODS_VTABLE_LAYOUT_VERSION,
    VIDEO_DECODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION,
    VIDEO_ENCODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION,
    VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE_LAYOUT_VERSION,
    VULKAN_COMPUTE_KERNEL_METHODS_VTABLE_LAYOUT_VERSION,
    VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE_LAYOUT_VERSION,
    VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE_LAYOUT_VERSION,
    VULKAN_TEXTURE_READBACK_METHODS_VTABLE_LAYOUT_VERSION,
};

// Origin tag threaded to the host's `iceoryx_log_emit` for every skew
// refusal. The bare `install_host_services` fn name (no leading module
// path) is the accurate, operator-actionable pointer to where the refusal
// originates — no top-level module path exists to name here, and a
// module-path-shaped literal would additionally trip the top-level-shortcut
// boundary check.
const SKEW_ORIGIN: &str = "install_host_services";

pub(crate) unsafe fn report_layout_skew(
    services: &HostServices,
    vtable_name: &str,
    expected: u32,
    got: u32,
) {
    let message = format!(
        "plugin ABI layout skew: `{vtable_name}` layout_version mismatch — \
         host expects {expected}, plugin was built against {got}. Rebuild the \
         plugin against the host's plugin-ABI train (this cdylib is refused)."
    );
    unsafe {
        (services.iceoryx_log_emit)(
            services.host,
            HostLogLevel::Error,
            SKEW_ORIGIN.as_ptr(),
            SKEW_ORIGIN.len(),
            message.as_ptr(),
            message.len(),
        );
    }
}

pub(crate) unsafe fn validate_host_services_layout(services: &HostServices) -> Result<(), ()> {
    if services.abi_layout_version != HOST_SERVICES_LAYOUT_VERSION {
        unsafe {
            report_layout_skew(
                services,
                "HostServices",
                HOST_SERVICES_LAYOUT_VERSION,
                services.abi_layout_version,
            )
        };
        return Err(());
    }

    macro_rules! check_inner_vtable {
        ($field:ident, $name:literal, $expected:expr) => {
            if !services.$field.is_null() {
                let got = unsafe { (*services.$field).layout_version };
                if got != $expected {
                    unsafe { report_layout_skew(services, $name, $expected, got) };
                    return Err(());
                }
            }
        };
    }

    check_inner_vtable!(
        runtime_context_vtable,
        "RuntimeContextVTable",
        RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        audio_clock_vtable,
        "AudioClockVTable",
        AUDIO_CLOCK_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        runtime_ops_vtable,
        "RuntimeOpsVTable",
        RUNTIME_OPS_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        gpu_context_limited_access_vtable,
        "GpuContextLimitedAccessVTable",
        GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        surface_store_vtable,
        "SurfaceStoreVTable",
        SURFACE_STORE_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        gpu_context_full_access_vtable,
        "GpuContextFullAccessVTable",
        GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        texture_ring_methods_vtable,
        "TextureRingMethodsVTable",
        TEXTURE_RING_METHODS_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        vulkan_compute_kernel_methods_vtable,
        "VulkanComputeKernelMethodsVTable",
        VULKAN_COMPUTE_KERNEL_METHODS_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        vulkan_graphics_kernel_methods_vtable,
        "VulkanGraphicsKernelMethodsVTable",
        VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        vulkan_ray_tracing_kernel_methods_vtable,
        "VulkanRayTracingKernelMethodsVTable",
        VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        vulkan_acceleration_structure_methods_vtable,
        "VulkanAccelerationStructureMethodsVTable",
        VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        rhi_color_converter_methods_vtable,
        "RhiColorConverterMethodsVTable",
        RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        rhi_command_recorder_methods_vtable,
        "RhiCommandRecorderMethodsVTable",
        RHI_COMMAND_RECORDER_METHODS_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        output_writer_vtable,
        "OutputWriterVTable",
        OUTPUT_WRITER_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        input_mailboxes_vtable,
        "InputMailboxesVTable",
        INPUT_MAILBOXES_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        present_target_methods_vtable,
        "PresentTargetMethodsVTable",
        PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        video_encoder_session_methods_vtable,
        "VideoEncoderSessionMethodsVTable",
        VIDEO_ENCODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        video_decoder_session_methods_vtable,
        "VideoDecoderSessionMethodsVTable",
        VIDEO_DECODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        host_timeline_semaphore_methods_vtable,
        "HostTimelineSemaphoreMethodsVTable",
        HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE_LAYOUT_VERSION
    );
    check_inner_vtable!(
        vulkan_texture_readback_methods_vtable,
        "VulkanTextureReadbackMethodsVTable",
        VULKAN_TEXTURE_READBACK_METHODS_VTABLE_LAYOUT_VERSION
    );

    Ok(())
}

// This test module is part of the logic-identical twin contract (held in
// sync by `twin_drift_guard`): it must stay byte/logic-identical in BOTH
// `layout_skew_diagnostic.rs` copies. It runs GPU-free / iceoryx2-free in
// `cargo test --lib` for both the engine and the engine-free SDK, so a
// same-in-both revert of the emit-and-refuse to a bare `return Err(())`
// (which `twin_drift_guard` cannot catch, since it only detects drift
// BETWEEN the copies) fails a real behavior assertion in each crate.
#[cfg(test)]
mod skew_diagnostic_behavior_tests {
    use super::{SKEW_ORIGIN, validate_host_services_layout};
    use core::cell::RefCell;
    use core::ffi::c_void;
    use streamlib_plugin_abi::{
        HOST_SERVICES_LAYOUT_VERSION, HostHandle, HostInterest, HostLogLevel, HostServices,
        PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION, PresentTargetMethodsVTable, ProcessorVTable,
    };

    // One captured `iceoryx_log_emit` call. The diagnostic threads the
    // `HostServices.host` opaque handle straight into the callback, so the
    // test points `host` at a sink and reads the emit back here.
    struct CapturedSkewLog {
        level: HostLogLevel,
        origin: String,
        message: String,
    }

    // Capturing `iceoryx_log_emit`: `host` is a `*const RefCell<Vec<...>>`
    // the test owns; copy the origin + message out and record them.
    unsafe extern "C" fn capturing_iceoryx_log_emit(
        host: HostHandle,
        level: HostLogLevel,
        origin_ptr: *const u8,
        origin_len: usize,
        message_ptr: *const u8,
        message_len: usize,
    ) {
        let sink = unsafe { &*(host as *const RefCell<Vec<CapturedSkewLog>>) };
        let origin = unsafe {
            core::str::from_utf8_unchecked(core::slice::from_raw_parts(origin_ptr, origin_len))
        }
        .to_string();
        let message = unsafe {
            core::str::from_utf8_unchecked(core::slice::from_raw_parts(message_ptr, message_len))
        }
        .to_string();
        sink.borrow_mut().push(CapturedSkewLog {
            level,
            origin,
            message,
        });
    }

    // The layout-skew path never invokes these — they exist only to fill the
    // `#[repr(C)]` struct's non-null fn-pointer fields.
    unsafe extern "C" fn unused_register_callsite(
        _: HostHandle,
        _: *const u8,
        _: usize,
        _: HostLogLevel,
    ) -> HostInterest {
        HostInterest::Never
    }
    unsafe extern "C" fn unused_enabled(
        _: HostHandle,
        _: *const u8,
        _: usize,
        _: HostLogLevel,
    ) -> bool {
        false
    }
    unsafe extern "C" fn unused_tracing_emit(
        _: HostHandle,
        _: *const u8,
        _: usize,
        _: HostLogLevel,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
    ) {
    }
    unsafe extern "C" fn unused_pubsub_publish(
        _: HostHandle,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
    ) {
    }
    unsafe extern "C" fn unused_schema_register(
        _: HostHandle,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
    ) {
    }
    unsafe extern "C" fn unused_schema_lookup(
        _: HostHandle,
        _: *const u8,
        _: usize,
        _: extern "C" fn(*mut c_void, *const u8, usize),
        _: *mut c_void,
    ) {
    }
    unsafe extern "C" fn unused_processor_register(
        _: HostHandle,
        _: *const u8,
        _: usize,
        _: *const ProcessorVTable,
    ) -> i32 {
        0
    }

    // A `HostServices` whose outer version matches and every inner vtable is
    // null. `host` points at `sink`; the caller flips one field to inject a
    // mismatch. Building it here keeps each test to the single mismatch it
    // exercises.
    fn host_services_with_capture(sink: &RefCell<Vec<CapturedSkewLog>>) -> HostServices {
        HostServices {
            abi_layout_version: HOST_SERVICES_LAYOUT_VERSION,
            _reserved_padding: 0,
            host: sink as *const RefCell<Vec<CapturedSkewLog>> as HostHandle,
            tracing_register_callsite: unused_register_callsite,
            tracing_enabled: unused_enabled,
            tracing_emit: unused_tracing_emit,
            pubsub_publish: unused_pubsub_publish,
            schema_register: unused_schema_register,
            schema_lookup: unused_schema_lookup,
            iceoryx_log_emit: capturing_iceoryx_log_emit,
            processor_register: unused_processor_register,
            runtime_context_vtable: core::ptr::null(),
            audio_clock_vtable: core::ptr::null(),
            runtime_ops_vtable: core::ptr::null(),
            gpu_context_limited_access_vtable: core::ptr::null(),
            surface_store_vtable: core::ptr::null(),
            gpu_context_full_access_vtable: core::ptr::null(),
            texture_ring_methods_vtable: core::ptr::null(),
            vulkan_compute_kernel_methods_vtable: core::ptr::null(),
            vulkan_graphics_kernel_methods_vtable: core::ptr::null(),
            vulkan_ray_tracing_kernel_methods_vtable: core::ptr::null(),
            vulkan_acceleration_structure_methods_vtable: core::ptr::null(),
            rhi_color_converter_methods_vtable: core::ptr::null(),
            rhi_command_recorder_methods_vtable: core::ptr::null(),
            output_writer_vtable: core::ptr::null(),
            input_mailboxes_vtable: core::ptr::null(),
            present_target_methods_vtable: core::ptr::null(),
            video_encoder_session_methods_vtable: core::ptr::null(),
            video_decoder_session_methods_vtable: core::ptr::null(),
            host_timeline_semaphore_methods_vtable: core::ptr::null(),
            vulkan_texture_readback_methods_vtable: core::ptr::null(),
        }
    }

    // The pinned `{ layout_version, _reserved_padding }` prefix every inner
    // vtable carries at offset 0. The diagnostic reads only `layout_version`
    // @0 before refusing on a mismatch, so a prefix-shaped stand-in is a
    // sound, driver-free way to inject a skewed inner vtable (this IS the
    // "layout_version pinned at offset 0, read first" ABI contract — the
    // diagnostic must work even when the rest of the vtable has drifted).
    #[repr(C)]
    struct PinnedInnerVtablePrefix {
        layout_version: u32,
        _reserved_padding: u32,
    }

    #[test]
    fn inner_vtable_layout_skew_names_vtable_and_both_versions_then_refuses() {
        let expected = PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION;
        let got = expected.wrapping_add(7);
        let skewed = PinnedInnerVtablePrefix {
            layout_version: got,
            _reserved_padding: 0,
        };
        let sink: RefCell<Vec<CapturedSkewLog>> = RefCell::new(Vec::new());
        let mut services = host_services_with_capture(&sink);
        services.present_target_methods_vtable =
            &skewed as *const PinnedInnerVtablePrefix as *const PresentTargetMethodsVTable;

        let result = unsafe { validate_host_services_layout(&services) };
        assert!(
            result.is_err(),
            "a layout-skewed inner vtable must refuse the install"
        );

        let emitted = sink.borrow();
        assert_eq!(
            emitted.len(),
            1,
            "exactly one actionable diagnostic must be emitted (dropping the \
             emit-and-refuse to a bare `return Err(())` fails here)"
        );
        let log = &emitted[0];
        assert_eq!(log.level, HostLogLevel::Error);
        assert_eq!(log.origin, SKEW_ORIGIN);
        assert!(
            log.message.contains("PresentTargetMethodsVTable"),
            "diagnostic must NAME the mismatched vtable — got: {}",
            log.message
        );
        assert!(
            log.message.contains(&expected.to_string()),
            "diagnostic must state the host's expected layout version — got: {}",
            log.message
        );
        assert!(
            log.message.contains(&got.to_string()),
            "diagnostic must state the plugin's built-against layout version — got: {}",
            log.message
        );
    }

    #[test]
    fn outer_host_services_layout_skew_names_struct_and_both_versions_then_refuses() {
        let expected = HOST_SERVICES_LAYOUT_VERSION;
        let got = expected.wrapping_add(3);
        let sink: RefCell<Vec<CapturedSkewLog>> = RefCell::new(Vec::new());
        let mut services = host_services_with_capture(&sink);
        services.abi_layout_version = got;

        let result = unsafe { validate_host_services_layout(&services) };
        assert!(
            result.is_err(),
            "a skewed outer HostServices layout version must refuse the install"
        );

        let emitted = sink.borrow();
        assert_eq!(emitted.len(), 1, "exactly one actionable diagnostic");
        let log = &emitted[0];
        assert_eq!(log.level, HostLogLevel::Error);
        assert_eq!(log.origin, SKEW_ORIGIN);
        assert!(
            log.message.contains("HostServices"),
            "diagnostic must NAME the mismatched struct — got: {}",
            log.message
        );
        assert!(
            log.message.contains(&expected.to_string()),
            "diagnostic must state the host's expected layout version — got: {}",
            log.message
        );
        assert!(
            log.message.contains(&got.to_string()),
            "diagnostic must state the plugin's built-against layout version — got: {}",
            log.message
        );
    }

    #[test]
    fn matched_layout_returns_ok_and_emits_no_diagnostic() {
        let sink: RefCell<Vec<CapturedSkewLog>> = RefCell::new(Vec::new());
        let services = host_services_with_capture(&sink);

        let result = unsafe { validate_host_services_layout(&services) };
        assert!(
            result.is_ok(),
            "a matched-version, all-null-inner HostServices must install cleanly"
        );
        assert!(
            sink.borrow().is_empty(),
            "the happy path must emit no skew diagnostic"
        );
    }
}
