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
    PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION, RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION,
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

const SKEW_ORIGIN: &str = "streamlib::plugin::install_host_services";

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
