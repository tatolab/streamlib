// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration-test fixture: a dlopen'd processor that exercises a
//! handful of the `VulkanRayTracingKernelMethodsVTable` slots end-to-end
//! from cdylib code.
//!
//! Smoke-only — narrower scope than the compute-kernel CPU-reference
//! test. Validates that the vtable round-trips for kernel construction,
//! AS-binding setter, storage-image setter, push-constant staging, and
//! a `trace_rays()` dispatch complete without panicking or returning
//! an error code. Pixel correctness is not asserted.
//!
//! Lifecycle:
//!   1. `setup()` — nothing.
//!   2. `start()` —
//!      a. Probe the FullAccess vtable's
//!         `supports_ray_tracing_pipeline()` slot. If the device
//!         doesn't expose `VK_KHR_ray_tracing_pipeline`, write `OK`
//!         immediately — the cdylib vtable round-trip itself
//!         succeeded; per-platform RT support is a host concern.
//!      b. Build a single-triangle BLAS via
//!         `gpu_full_access().build_triangles_blas(...)`. Exercises
//!         the FullAccess vtable's `build_triangles_blas` slot.
//!      c. Build an identity TLAS over the BLAS via
//!         `gpu_full_access().build_tlas(...)`. Exercises the
//!         FullAccess vtable's `build_tlas` slot.
//!      d. Construct a [`RayTracingKernelDescriptor`] for the
//!         embedded rgen+rmiss+rchit SPIR-V (acceleration_structure
//!         + storage_image bindings, push-constant variant gate) and
//!         create the kernel via
//!         `gpu_full_access().create_ray_tracing_kernel(...)`.
//!         Exercises the FullAccess vtable's
//!         `create_ray_tracing_kernel` slot.
//!      e. Acquire a STORAGE_BINDING + COPY_SRC render-target
//!         `Texture` via `gpu_limited_access().acquire_texture(...)`.
//!      f. Stage AS binding via
//!         `kernel.set_acceleration_structure(0, &tlas)` —
//!         exercises the `set_acceleration_structure` vtable slot.
//!      g. Stage storage image binding via
//!         `kernel.set_storage_image(1, texture)` — exercises the
//!         `set_storage_image` vtable slot.
//!      h. Stage push constants via
//!         `kernel.set_push_constants_value(&variant)` — exercises
//!         the `set_push_constants` vtable slot.
//!      i. Drive `kernel.trace_rays(width, height, 1)` against the
//!         acquired texture — exercises the `trace_rays` vtable slot
//!         end-to-end (including the SBT + queue submit + fence
//!         wait).
//!      j. Write `OK` or `ERR:<message>` to the configured
//!         `output_path` so the integration test can assert the
//!         round-trip succeeded.
//!   3. `teardown()` — nothing; the kernel + AS + texture drop
//!      naturally.
//!
//! What this locks: a regression that breaks any of
//! `supports_ray_tracing_pipeline`, `build_triangles_blas`,
//! `build_tlas`, `create_ray_tracing_kernel`, `acquire_texture`,
//! `set_acceleration_structure`, `set_storage_image`,
//! `set_push_constants`, or `trace_rays` at the cdylib boundary
//! surfaces here as either a missing output file (cdylib panicked at
//! the FFI boundary) or `ERR:<message>` in the file.

use streamlib::engine_internal::core::context::TexturePoolDescriptor;
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;

/// SPIR-V for the smoke-test ray-gen stage. Compiled by `build.rs`.
#[cfg(target_os = "linux")]
const SMOKE_RGEN_SPV: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/ray_tracing_kernel_smoke_rgen.spv"
));

/// SPIR-V for the smoke-test miss stage. Compiled by `build.rs`.
#[cfg(target_os = "linux")]
const SMOKE_RMISS_SPV: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/ray_tracing_kernel_smoke_rmiss.spv"
));

/// SPIR-V for the smoke-test closest-hit stage. Compiled by `build.rs`.
#[cfg(target_os = "linux")]
const SMOKE_RCHIT_SPV: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/ray_tracing_kernel_smoke_rchit.spv"
));

#[cfg(target_os = "linux")]
const SMOKE_SURFACE_SIZE: u32 = 64;

#[streamlib::sdk::processor(
    "@tatolab/test-fixtures/RayTracingKernelSmokeTestProcessor@1.0.0",
    execution = manual,
    config = crate::_generated_::RayTracingKernelSmokeTestProcessorConfig,
)]
pub struct RayTracingKernelSmokeTest {}

impl ManualProcessor for RayTracingKernelSmokeTest::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();

        let outcome = run_ray_tracing_kernel_smoke(ctx);

        let line = match outcome {
            Ok(()) => "OK".to_string(),
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &line).map_err(|e| {
            Error::Runtime(format!(
                "RayTracingKernelSmokeTest: write {output_path}: {e}"
            ))
        })?;
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn run_ray_tracing_kernel_smoke(ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
    use streamlib::sdk::engine::host_rhi::TlasInstanceDesc;
    use streamlib::sdk::rhi::{
        RayTracingBindingSpec, RayTracingKernelDescriptor, RayTracingPushConstants,
        RayTracingShaderGroup, RayTracingShaderStageFlags, RayTracingStage, TextureFormat,
        TextureUsages,
    };

    let gpu_limited = ctx.gpu_limited_access();

    // Single triangle centered at the origin in the XY plane.
    let vertices: Vec<f32> = vec![0.0, -0.5, 0.0, -0.5, 0.5, 0.0, 0.5, 0.5, 0.0];
    let indices: Vec<u32> = vec![0, 1, 2];

    // Manual-mode start() takes FullAccess directly; the engine
    // wraps cdylib lifecycle dispatch in `with_cdylib_scope` (#1075),
    // so `ctx.gpu_full_access()` is `ScopeToken`-flavored and
    // dispatches through the FullAccess vtable transparently.
    // Same coverage as the pre-#1075 escalate path; the wrap is the
    // engine-side replacement for the explicit `.escalate(|full|...)`.
    let full = ctx.gpu_full_access();

    // Probe RT capability first — on devices without
    // VK_KHR_ray_tracing_pipeline, the cdylib vtable round-trip
    // itself succeeded; per-platform RT support is a host concern.
    let supports_rt = full.supports_ray_tracing_pipeline();
    if !supports_rt {
        return Ok(());
    }

    let pool_descriptor = TexturePoolDescriptor {
        width: SMOKE_SURFACE_SIZE,
        height: SMOKE_SURFACE_SIZE,
        format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::COPY_SRC | TextureUsages::STORAGE_BINDING,
        label: Some("ray_tracing_kernel_smoke_target"),
    };
    let texture_handle = gpu_limited
        .acquire_texture(&pool_descriptor)
        .map_err(|e| Error::Runtime(format!("acquire_texture: {e}")))?;

    // Build BLAS + TLAS + kernel — all FullAccess-privileged, all
    // dispatch directly through the wrapped scope.
    let (kernel, tlas) = (|| -> Result<_> {
        let blas = full.build_triangles_blas("rt-smoke-blas", &vertices, &indices)?;
        let instances = vec![TlasInstanceDesc::identity(blas)];
        let tlas = full.build_tlas("rt-smoke-tlas", &instances)?;

        let stages = [
            RayTracingStage::ray_gen(SMOKE_RGEN_SPV),
            RayTracingStage::miss(SMOKE_RMISS_SPV),
            RayTracingStage::closest_hit(SMOKE_RCHIT_SPV),
        ];
        let groups = [
            RayTracingShaderGroup::General { general: 0 },
            RayTracingShaderGroup::General { general: 1 },
            RayTracingShaderGroup::TrianglesHit {
                closest_hit: Some(2),
                any_hit: None,
            },
        ];
        let bindings = [
            RayTracingBindingSpec::acceleration_structure(0, RayTracingShaderStageFlags::RAYGEN),
            RayTracingBindingSpec::storage_image(1, RayTracingShaderStageFlags::RAYGEN),
        ];
        let push_constants = RayTracingPushConstants {
            size: std::mem::size_of::<u32>() as u32,
            stages: RayTracingShaderStageFlags::RAYGEN,
        };
        let descriptor = RayTracingKernelDescriptor {
            label: "ray_tracing_kernel_smoke",
            stages: &stages,
            groups: &groups,
            bindings: &bindings,
            push_constants,
            max_recursion_depth: 1,
        };
        let kernel = full.create_ray_tracing_kernel(&descriptor)?;
        Ok((kernel, tlas))
    })()
    .map_err(|e| Error::Runtime(format!("create_ray_tracing_kernel setup: {e}")))?;

    kernel
        .set_acceleration_structure(0, &tlas)
        .map_err(|e| Error::Runtime(format!("set_acceleration_structure: {e}")))?;
    kernel
        .set_storage_image(1, texture_handle.texture())
        .map_err(|e| Error::Runtime(format!("set_storage_image: {e}")))?;

    let variant: u32 = 0;
    kernel
        .set_push_constants_value(&variant)
        .map_err(|e| Error::Runtime(format!("set_push_constants_value: {e}")))?;

    kernel
        .trace_rays(SMOKE_SURFACE_SIZE, SMOKE_SURFACE_SIZE, 1)
        .map_err(|e| Error::Runtime(format!("trace_rays: {e}")))?;

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn run_ray_tracing_kernel_smoke(_ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
    Err(Error::Runtime(
        "RayTracingKernelSmokeTest: ray-tracing kernel dispatch is Linux-only today".into(),
    ))
}
