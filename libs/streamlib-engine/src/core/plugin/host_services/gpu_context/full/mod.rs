// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `GpuContextFullAccessVTable` callbacks, split per
//! banner-bounded section of the original `gpu_context.rs` file. Each
//! submodule owns the wrappers for one concern; the parent's
//! `HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE` static wires the function
//! pointers up by name.
//!
//! `drop_handle` is a defensive no-op kept here at the FullAccess root
//! because `GpuContextFullAccess::Drop` dispatches on the struct's
//! `handle_kind` discriminator directly without routing through this
//! vtable slot — host-mode (Boxed) runs `Box::from_raw` in-process;
//! cdylib-mode (ScopeToken) releases the gate via the LimitedAccess
//! vtable's `escalate_end` callback. The slot is preserved at the same
//! vtable offset for layout-version stability.

use std::ffi::c_void;

use super::super::run_host_extern_c;

#[cfg(target_os = "linux")]
mod kernel_construction;
#[cfg(target_os = "linux")]
mod kernel_lifecycle;
mod methods;
mod render_target;

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) use kernel_construction::{
    host_gpu_full_create_compute_kernel, host_gpu_full_create_graphics_kernel,
    host_gpu_full_create_ray_tracing_kernel, host_gpu_full_create_texture_ring,
};
#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) use kernel_lifecycle::{
    host_gpu_full_clone_acceleration_structure, host_gpu_full_clone_color_converter,
    host_gpu_full_clone_command_recorder, host_gpu_full_clone_compute_kernel,
    host_gpu_full_clone_graphics_kernel, host_gpu_full_clone_ray_tracing_kernel,
    host_gpu_full_clone_texture_ring, host_gpu_full_drop_acceleration_structure,
    host_gpu_full_drop_color_converter, host_gpu_full_drop_command_recorder,
    host_gpu_full_drop_compute_kernel, host_gpu_full_drop_graphics_kernel,
    host_gpu_full_drop_ray_tracing_kernel, host_gpu_full_drop_texture_ring,
};
pub(in crate::core::plugin::host_services) use methods::{
    host_gpu_full_acquire_output_texture, host_gpu_full_build_tlas,
    host_gpu_full_build_triangles_blas, host_gpu_full_check_in_surface,
    host_gpu_full_color_converter, host_gpu_full_create_command_recorder,
    host_gpu_full_create_timeline_semaphore, host_gpu_full_gpu_capabilities,
    host_gpu_full_host_vulkan_device_arc, host_gpu_full_host_vulkan_texture_arc,
    host_gpu_full_import_dma_buf_storage_buffer, host_gpu_full_supports_ray_tracing_pipeline,
    host_gpu_full_upload_pixel_buffer_as_texture, host_gpu_full_wait_device_idle,
};
pub(in crate::core::plugin::host_services) use render_target::host_gpu_full_acquire_render_target_dma_buf_image;

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_handle(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_full_drop_handle",
        || {
            let _ = handle;
        },
        (),
    )
}
#[cfg(test)]
mod gpu_full_access_vtable_tests {
    use std::ffi::c_void;

    use super::super::HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE;
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

    // v9 slot: host_vulkan_device_arc takes a scope token (not an
    // err-buf). The null-token case bottoms out in
    // `with_scope(0, ...) → None`, so the callback returns a null
    // pointer. Mental-revert: stub the callback body to call
    // `Arc::into_raw(...)` directly on a freshly-cloned Arc without
    // checking the token; this test trips on the resulting non-null
    // return and the unmatched `from_raw` would Drop the Arc, lowering
    // the refcount on the host's actual `Arc<HostVulkanDevice>`.
    #[test]
    fn host_vulkan_device_arc_returns_null_on_null_token() {
        let raw = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.host_vulkan_device_arc)(
                std::ptr::null(),
            )
        };
        assert!(raw.is_null(), "null scope token must yield null pointer");
    }

    // v10 slot: host_vulkan_texture_arc takes a raw texture handle
    // (not an err-buf). The null-handle case short-circuits in the
    // wrapper before any deref, returning a null pointer. Mental-
    // revert: remove the `if texture_handle.is_null()` guard inside
    // `host_gpu_full_host_vulkan_texture_arc`; the wrapper would then
    // UB-deref the null pointer as `*const TextureInner` and the test
    // runner would SIGSEGV.
    #[test]
    fn host_vulkan_texture_arc_returns_null_on_null_handle() {
        let raw = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.host_vulkan_texture_arc)(
                std::ptr::null(),
            )
        };
        assert!(raw.is_null(), "null texture handle must yield null pointer");
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
        let services = super::super::super::runtime_facing::host_services_for_self(&node);
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

