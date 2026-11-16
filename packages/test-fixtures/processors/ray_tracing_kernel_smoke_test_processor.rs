// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


#[cfg(target_os = "linux")]
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
    execution = manual,
    config = crate::test_fixture_processor_configs::RayTracingKernelSmokeTestProcessorConfig,
)]
#[meta(
    description = "Ray-tracing-kernel smoke test fixture — builds a single-triangle BLAS + identity TLAS via FullAccess, creates an RT kernel, acquires a STORAGE_BINDING Texture, runs a single trace_rays() to assert the binding methods (set_acceleration_structure / set_storage_image / set_push_constants / trace_rays) don't panic. Smoke-only; pixel correctness not asserted."
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

    // Manual-mode start() takes FullAccess directly.
    let full = ctx.gpu_full_access();

    // Probe RT capability first — per-platform RT support is a host
    // concern, so a device without VK_KHR_ray_tracing_pipeline still
    // counts as a passing run.
    let supports_rt = full.supports_ray_tracing_pipeline();
    if !supports_rt {
        return Ok(());
    }

    let pool_descriptor = TexturePoolDescriptor::new(
        SMOKE_SURFACE_SIZE,
        SMOKE_SURFACE_SIZE,
        TextureFormat::Rgba8Unorm,
    )
    .with_usage(TextureUsages::COPY_SRC | TextureUsages::STORAGE_BINDING)
    .with_label("ray_tracing_kernel_smoke_target");
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
