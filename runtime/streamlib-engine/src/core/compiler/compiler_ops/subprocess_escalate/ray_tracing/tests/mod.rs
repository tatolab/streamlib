// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::super::handle_escalate_op;
use super::super::handle_lifecycle::{
    EscalateHandleRegistry, release_everything_a_torn_down_helper_held,
};
use super::super::kernel_shader_stage_source::SPIRV_MAGIC_LE;
use super::linux_and_macos::prepare_ray_tracing_kernel_registration;
use crate::core::compiler::compiler_ops::subprocess_escalate::WhatAHelperPlacedNodeReaches;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRayTracingBindingKind, EscalateRequestRegisterAccelerationStructureBlas,
    EscalateRequestRegisterAccelerationStructureTlas,
    EscalateRequestRegisterAccelerationStructureTlasInstance,
    EscalateRequestRegisterRayTracingKernel, EscalateRequestRegisterRayTracingKernelBinding,
    EscalateRequestRegisterRayTracingKernelGroup, EscalateRequestRegisterRayTracingKernelGroupKind,
    EscalateRequestRegisterRayTracingKernelStage,
    EscalateRequestRegisterRayTracingKernelStageStage, EscalateRequestReleaseHandle,
    EscalateRequestRunRayTracingKernel, EscalateRequestRunRayTracingKernelBinding,
    RAY_TRACING_STAGE_INDEX_NONE,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseOk;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::{
    EscalateRequest, EscalateResponse,
};
use crate::core::context::{
    GpuContext, GpuContextLimitedAccess, PooledTextureHandle, TexturePoolDescriptor,
};
use crate::core::rhi::{
    PixelFormat, RayTracingShaderStage, RayTracingShaderStageFlags, TextureFormat, TextureUsages,
};

mod acceleration_structure_tests;
mod ray_tracing_kernel_registration_tests;
mod tracing_tests;
mod wire_validation_tests;

/// Ray tracing is a device capability rather than an installed bridge,
/// so there is nothing to set up — only a device to have or not have.
fn make_gpu_sandbox_if_available() -> Option<GpuContextLimitedAccess> {
    GpuContext::init_for_platform_sync()
        .ok()
        .map(GpuContextLimitedAccess::new)
}

/// A sandbox whose device exposes the `VK_KHR_ray_tracing_pipeline`
/// chain — what every structure build and every pipeline build needs.
fn make_ray_tracing_sandbox_if_available() -> Option<GpuContextLimitedAccess> {
    let sandbox = make_gpu_sandbox_if_available()?;
    let ray_tracing_capable = sandbox
        .escalate(|full| Ok(full.supports_ray_tracing_pipeline()))
        .unwrap_or(false);
    ray_tracing_capable.then_some(sandbox)
}

fn refusal_message(response: EscalateResponse) -> String {
    match response {
        EscalateResponse::Err(err) => err.message,
        other => panic!("expected Err, got {other:?}"),
    }
}

/// Traces one ray per pixel straight down `-Z` at the bound structure
/// and writes whatever the hit or miss stage left in the payload. Both
/// bindings are resolved by the names this source gives them.
const TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_SLANG: &str = "\
[[vk::binding(0, 0)]] RaytracingAccelerationStructure scene_geometry;
[[vk::binding(1, 0)]] [format(\"rgba8\")] RWTexture2D<float4> traced_output;
struct TracedColourPayload { float3 traced_colour; };
void main() {
    float2 pixel_centre = float2(DispatchRaysIndex().xy) + float2(0.5);
    float2 normalized_device_coordinate =
        pixel_centre / float2(DispatchRaysDimensions().xy) * 2.0 - 1.0;
    RayDesc ray;
    ray.Origin = float3(normalized_device_coordinate.x, -normalized_device_coordinate.y, 1.0);
    ray.TMin = 0.001;
    ray.Direction = float3(0.0, 0.0, -1.0);
    ray.TMax = 100.0;
    TracedColourPayload payload;
    payload.traced_colour = float3(0.0);
    TraceRay(scene_geometry, RAY_FLAG_FORCE_OPAQUE, 0xff, 0, 0, 0, ray, payload);
    traced_output[DispatchRaysIndex().xy] = float4(payload.traced_colour, 1.0);
}
";

/// The same pass with the payload inverted before it is stored, so
/// registering it produces a different pipeline — and therefore a
/// different kernel id — from [`TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_SLANG`].
const TRACE_AND_INVERT_RAY_GEN_SLANG: &str = "\
[[vk::binding(0, 0)]] RaytracingAccelerationStructure scene_geometry;
[[vk::binding(1, 0)]] [format(\"rgba8\")] RWTexture2D<float4> traced_output;
struct TracedColourPayload { float3 traced_colour; };
void main() {
    float2 pixel_centre = float2(DispatchRaysIndex().xy) + float2(0.5);
    float2 normalized_device_coordinate =
        pixel_centre / float2(DispatchRaysDimensions().xy) * 2.0 - 1.0;
    RayDesc ray;
    ray.Origin = float3(normalized_device_coordinate.x, -normalized_device_coordinate.y, 1.0);
    ray.TMin = 0.001;
    ray.Direction = float3(0.0, 0.0, -1.0);
    ray.TMax = 100.0;
    TracedColourPayload payload;
    payload.traced_colour = float3(0.0);
    TraceRay(scene_geometry, RAY_FLAG_FORCE_OPAQUE, 0xff, 0, 0, 0, ray, payload);
    traced_output[DispatchRaysIndex().xy] = float4(float3(1.0) - payload.traced_colour, 1.0);
}
";

/// A ray that hit nothing paints black.
const MISS_PAINTS_BLACK_SLANG: &str = "\
struct TracedColourPayload { float3 traced_colour; };
void main(inout TracedColourPayload payload) {
    payload.traced_colour = float3(0.0);
}
";

/// A ray that hit the scene's one triangle paints white, so a traced
/// pixel says which of the two stages ran for it.
const CLOSEST_HIT_PAINTS_WHITE_SLANG: &str = "\
struct TracedColourPayload { float3 traced_colour; };
void main(inout TracedColourPayload payload, BuiltInTriangleIntersectionAttributes hit) {
    payload.traced_colour = float3(1.0);
}
";

/// A pixel the ray hit, a pixel it missed, and the sentinel the storage
/// image is seeded with so an untouched pixel is distinguishable from
/// either.
const HIT_RGBA: [u8; 4] = [255, 255, 255, 255];
const MISSED_RGBA: [u8; 4] = [0, 0, 0, 255];
const UNTRACED_SENTINEL_RGBA: [u8; 4] = [255, 0, 255, 255];

/// One triangle facing the launch grid, centred on the origin so a
/// trace over the whole grid both hits and misses it.
const A_SCENES_TRIANGLE_VERTICES: &[f32] = &[0.0, -0.5, 0.0, -0.5, 0.5, 0.0, 0.5, 0.5, 0.0];
const A_SCENES_TRIANGLE_INDICES: &[u32] = &[0, 1, 2];

const TRACED_GRID_WIDTH: u32 = 64;
const TRACED_GRID_HEIGHT: u32 = 64;

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Encode `[f32]` as the lowercase hex blob the wire expects.
fn vertex_hex(vertices: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(vertices.len() * 4);
    for vertex in vertices {
        bytes.extend_from_slice(&vertex.to_le_bytes());
    }
    bytes_to_hex(&bytes)
}

/// Encode `[u32]` as the lowercase hex blob the wire expects.
fn index_hex(indices: &[u32]) -> String {
    let mut bytes = Vec::with_capacity(indices.len() * 4);
    for index in indices {
        bytes.extend_from_slice(&index.to_le_bytes());
    }
    bytes_to_hex(&bytes)
}

fn make_blas_req(
    request_id: &str,
    vertices_hex: &str,
    indices_hex: &str,
) -> EscalateRequestRegisterAccelerationStructureBlas {
    EscalateRequestRegisterAccelerationStructureBlas {
        request_id: request_id.to_string(),
        label: "a-scenes-triangle".to_string(),
        vertices_hex: vertices_hex.to_string(),
        indices_hex: indices_hex.to_string(),
    }
}

fn make_tlas_req(
    request_id: &str,
    blas_id: &str,
) -> EscalateRequestRegisterAccelerationStructureTlas {
    EscalateRequestRegisterAccelerationStructureTlas {
        request_id: request_id.to_string(),
        label: "a-scenes-instance".to_string(),
        instances: vec![EscalateRequestRegisterAccelerationStructureTlasInstance {
            blas_id: blas_id.to_string(),
            transform: vec![
                1.0, 0.0, 0.0, 0.0, //
                0.0, 1.0, 0.0, 0.0, //
                0.0, 0.0, 1.0, 0.0,
            ],
            custom_index: 7,
            mask: 0xff,
            sbt_record_offset: 0,
            flags: 0,
        }],
    }
}

fn general_group(stage_index: u32) -> EscalateRequestRegisterRayTracingKernelGroup {
    EscalateRequestRegisterRayTracingKernelGroup {
        kind: EscalateRequestRegisterRayTracingKernelGroupKind::General,
        general_stage: stage_index,
        closest_hit_stage: RAY_TRACING_STAGE_INDEX_NONE,
        any_hit_stage: RAY_TRACING_STAGE_INDEX_NONE,
        intersection_stage: RAY_TRACING_STAGE_INDEX_NONE,
    }
}

fn triangles_hit_group(
    closest_hit_stage_index: u32,
) -> EscalateRequestRegisterRayTracingKernelGroup {
    EscalateRequestRegisterRayTracingKernelGroup {
        kind: EscalateRequestRegisterRayTracingKernelGroupKind::TrianglesHit,
        general_stage: RAY_TRACING_STAGE_INDEX_NONE,
        closest_hit_stage: closest_hit_stage_index,
        any_hit_stage: RAY_TRACING_STAGE_INDEX_NONE,
        intersection_stage: RAY_TRACING_STAGE_INDEX_NONE,
    }
}

fn stage_from_slang(
    stage: EscalateRequestRegisterRayTracingKernelStageStage,
    source: &str,
) -> EscalateRequestRegisterRayTracingKernelStage {
    EscalateRequestRegisterRayTracingKernelStage {
        entry_point: String::new(),
        source: source.to_string(),
        spv_hex: String::new(),
        stage,
    }
}

/// The three-stage kernel every registration test starts from, built
/// from Slang, which is what the wire carries. Tests that need a
/// specific shape mutate fields after calling.
fn register_from_slang(
    request_id: &str,
    ray_gen_source: &str,
) -> EscalateRequestRegisterRayTracingKernel {
    EscalateRequestRegisterRayTracingKernel {
        bindings: vec![
            EscalateRequestRegisterRayTracingKernelBinding {
                kind: EscalateRayTracingBindingKind::AccelerationStructure,
                name: "scene_geometry".to_string(),
                stages: RayTracingShaderStageFlags::RAYGEN.bits(),
            },
            EscalateRequestRegisterRayTracingKernelBinding {
                kind: EscalateRayTracingBindingKind::StorageImage,
                name: "traced_output".to_string(),
                stages: RayTracingShaderStageFlags::RAYGEN.bits(),
            },
        ],
        groups: vec![general_group(0), general_group(1), triangles_hit_group(2)],
        label: "a-tracing-kernel".to_string(),
        max_recursion_depth: 1,
        push_constant_size: 0,
        push_constant_stages: 0,
        request_id: request_id.to_string(),
        stages: vec![
            stage_from_slang(
                EscalateRequestRegisterRayTracingKernelStageStage::RayGen,
                ray_gen_source,
            ),
            stage_from_slang(
                EscalateRequestRegisterRayTracingKernelStageStage::Miss,
                MISS_PAINTS_BLACK_SLANG,
            ),
            stage_from_slang(
                EscalateRequestRegisterRayTracingKernelStageStage::ClosestHit,
                CLOSEST_HIT_PAINTS_WHITE_SLANG,
            ),
        ],
    }
}

/// Baseline `run_ray_tracing_kernel` request — the scene bound by the
/// raygen's own name for it, the storage image by its own.
fn make_run_req(
    request_id: &str,
    kernel_id: &str,
    tlas_id: &str,
    output_surface_uuid: &str,
) -> EscalateRequestRunRayTracingKernel {
    EscalateRequestRunRayTracingKernel {
        bindings: vec![
            EscalateRequestRunRayTracingKernelBinding {
                kind: EscalateRayTracingBindingKind::AccelerationStructure,
                name: "scene_geometry".to_string(),
                target_id: tlas_id.to_string(),
            },
            EscalateRequestRunRayTracingKernelBinding {
                kind: EscalateRayTracingBindingKind::StorageImage,
                name: "traced_output".to_string(),
                target_id: output_surface_uuid.to_string(),
            },
        ],
        depth: 1,
        height: TRACED_GRID_HEIGHT,
        kernel_id: kernel_id.to_string(),
        push_constants_hex: String::new(),
        request_id: request_id.to_string(),
        width: TRACED_GRID_WIDTH,
    }
}

fn register_ray_tracing_kernel_or_panic(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    req: EscalateRequestRegisterRayTracingKernel,
) -> EscalateResponseOk {
    let response = handle_escalate_op(
        sandbox,
        registry,
        &WhatAHelperPlacedNodeReaches::default(),
        EscalateRequest::RegisterRayTracingKernel(req),
    )
    .expect("must produce a response");
    match response {
        EscalateResponse::Ok(ok) => ok,
        other => panic!("registering the ray-tracing kernel failed: {other:?}"),
    }
}

fn register_acceleration_structure_or_panic(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    req: EscalateRequest,
) -> String {
    let response = handle_escalate_op(
        sandbox,
        registry,
        &WhatAHelperPlacedNodeReaches::default(),
        req,
    )
    .expect("must produce a response");
    match response {
        EscalateResponse::Ok(ok) => ok.handle_id,
        other => panic!("registering the acceleration structure failed: {other:?}"),
    }
}

/// One registered kernel over one registered scene, writing into one
/// registered storage image seeded with [`UNTRACED_SENTINEL_RGBA`].
///
/// The pooled handle is held for the caller's lifetime: dropping it
/// hands the slot back, and the registration would then name a recycled
/// texture.
struct ARayTracedSceneUnderTest {
    sandbox: GpuContextLimitedAccess,
    registry: std::sync::Arc<EscalateHandleRegistry>,
    kernel_id: String,
    blas_id: String,
    tlas_id: String,
    _held_output: PooledTextureHandle,
}

const A_TRACED_SCENES_OUTPUT_SURFACE_UUID: &str = "traced-output-surface";

fn make_ray_traced_scene_if_available() -> Option<ARayTracedSceneUnderTest> {
    let sandbox = make_ray_tracing_sandbox_if_available()?;
    let registry = EscalateHandleRegistry::new();
    let blas_id = register_acceleration_structure_or_panic(
        &sandbox,
        &registry,
        EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
            "scene-blas",
            &vertex_hex(A_SCENES_TRIANGLE_VERTICES),
            &index_hex(A_SCENES_TRIANGLE_INDICES),
        )),
    );
    let tlas_id = register_acceleration_structure_or_panic(
        &sandbox,
        &registry,
        EscalateRequest::RegisterAccelerationStructureTlas(make_tlas_req("scene-tlas", &blas_id)),
    );
    let kernel_id = register_ray_tracing_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("scene-kernel", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_SLANG),
    )
    .handle_id;

    let held_output = sandbox
        .escalate(|full| {
            let output = full.host_inner().acquire_texture(
                &TexturePoolDescriptor::new(
                    TRACED_GRID_WIDTH,
                    TRACED_GRID_HEIGHT,
                    TextureFormat::Rgba8Unorm,
                )
                .with_usage(
                    TextureUsages::STORAGE_BINDING
                        | TextureUsages::COPY_DST
                        | TextureUsages::COPY_SRC,
                ),
            )?;
            full.register_texture(
                A_TRACED_SCENES_OUTPUT_SURFACE_UUID,
                output.texture().clone(),
            );

            let (_pool_id, seed_buffer) = full.acquire_pixel_buffer(
                TRACED_GRID_WIDTH,
                TRACED_GRID_HEIGHT,
                PixelFormat::Rgba32,
            )?;
            let plane = seed_buffer.buffer_ref().plane_base_address(0);
            unsafe {
                for pixel in 0..(TRACED_GRID_WIDTH as usize * TRACED_GRID_HEIGHT as usize) {
                    std::ptr::copy_nonoverlapping(
                        UNTRACED_SENTINEL_RGBA.as_ptr(),
                        plane.add(pixel * 4),
                        4,
                    );
                }
            }
            full.copy_pixel_buffer_to_texture(
                &seed_buffer,
                output.texture(),
                A_TRACED_SCENES_OUTPUT_SURFACE_UUID,
                TRACED_GRID_WIDTH,
                TRACED_GRID_HEIGHT,
            )?;
            Ok(output)
        })
        .expect("a seeded storage image to trace into");

    Some(ARayTracedSceneUnderTest {
        sandbox,
        registry,
        kernel_id,
        blas_id,
        tlas_id,
        _held_output: held_output,
    })
}

impl ARayTracedSceneUnderTest {
    fn run_req(&self, request_id: &str) -> EscalateRequestRunRayTracingKernel {
        make_run_req(
            request_id,
            &self.kernel_id,
            &self.tlas_id,
            A_TRACED_SCENES_OUTPUT_SURFACE_UUID,
        )
    }

    fn trace(&self, req: EscalateRequestRunRayTracingKernel) -> EscalateResponse {
        handle_escalate_op(
            &self.sandbox,
            &self.registry,
            &WhatAHelperPlacedNodeReaches::default(),
            EscalateRequest::RunRayTracingKernel(req),
        )
        .expect("must produce a response")
    }
}
