// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-Rust unit tests for the acceleration-structure and ray-tracing
//! escalate handlers.
//!
//! Mirrors `graphics_kernel_dispatch`: the wire validation that raises
//! before the device gate runs everywhere CI does, and everything that
//! builds a structure or a pipeline gates on a device that exposes the
//! ray-tracing extension chain.

use super::super::handle_escalate_op;
use super::super::handle_lifecycle::EscalateHandleRegistry;
use super::super::kernel_shader_stage_source::SPIRV_MAGIC_LE;
use super::linux::prepare_ray_tracing_kernel_registration;
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
use crate::core::runtime::mesh::a_mesh_link_ingress_table_carrying_nothing;

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
const TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL: &str = "\
#version 460
#extension GL_EXT_ray_tracing : require
layout(set = 0, binding = 0) uniform accelerationStructureEXT scene_geometry;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D traced_output;
layout(location = 0) rayPayloadEXT vec3 traced_colour;
void main() {
    vec2 pixel_centre = vec2(gl_LaunchIDEXT.xy) + vec2(0.5);
    vec2 normalized_device_coordinate =
pixel_centre / vec2(gl_LaunchSizeEXT.xy) * 2.0 - 1.0;
    traced_colour = vec3(0.0);
    traceRayEXT(
scene_geometry,
gl_RayFlagsOpaqueEXT,
0xff,
0, 0, 0,
vec3(normalized_device_coordinate.x, -normalized_device_coordinate.y, 1.0),
0.001,
vec3(0.0, 0.0, -1.0),
100.0,
0
    );
    imageStore(traced_output, ivec2(gl_LaunchIDEXT.xy), vec4(traced_colour, 1.0));
}
";

/// The same pass with the payload inverted before it is stored, so
/// registering it produces a different pipeline — and therefore a
/// different kernel id — from [`TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL`].
const TRACE_AND_INVERT_RAY_GEN_GLSL: &str = "\
#version 460
#extension GL_EXT_ray_tracing : require
layout(set = 0, binding = 0) uniform accelerationStructureEXT scene_geometry;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D traced_output;
layout(location = 0) rayPayloadEXT vec3 traced_colour;
void main() {
    vec2 pixel_centre = vec2(gl_LaunchIDEXT.xy) + vec2(0.5);
    vec2 normalized_device_coordinate =
pixel_centre / vec2(gl_LaunchSizeEXT.xy) * 2.0 - 1.0;
    traced_colour = vec3(0.0);
    traceRayEXT(
scene_geometry,
gl_RayFlagsOpaqueEXT,
0xff,
0, 0, 0,
vec3(normalized_device_coordinate.x, -normalized_device_coordinate.y, 1.0),
0.001,
vec3(0.0, 0.0, -1.0),
100.0,
0
    );
    imageStore(
traced_output,
ivec2(gl_LaunchIDEXT.xy),
vec4(vec3(1.0) - traced_colour, 1.0)
    );
}
";

/// A ray that hit nothing paints black.
const MISS_PAINTS_BLACK_GLSL: &str = "\
#version 460
#extension GL_EXT_ray_tracing : require
layout(location = 0) rayPayloadInEXT vec3 traced_colour;
void main() {
    traced_colour = vec3(0.0);
}
";

/// A ray that hit the scene's one triangle paints white, so a traced
/// pixel says which of the two stages ran for it.
const CLOSEST_HIT_PAINTS_WHITE_GLSL: &str = "\
#version 460
#extension GL_EXT_ray_tracing : require
layout(location = 0) rayPayloadInEXT vec3 traced_colour;
void main() {
    traced_colour = vec3(1.0);
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

fn stage_from_glsl(
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
/// from GLSL — which is what the wire carries now that the engine owns
/// compilation. Tests that need a specific shape mutate fields after
/// calling.
fn register_from_glsl(
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
            stage_from_glsl(
                EscalateRequestRegisterRayTracingKernelStageStage::RayGen,
                ray_gen_source,
            ),
            stage_from_glsl(
                EscalateRequestRegisterRayTracingKernelStageStage::Miss,
                MISS_PAINTS_BLACK_GLSL,
            ),
            stage_from_glsl(
                EscalateRequestRegisterRayTracingKernelStageStage::ClosestHit,
                CLOSEST_HIT_PAINTS_WHITE_GLSL,
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
        &a_mesh_link_ingress_table_carrying_nothing(),
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
        &a_mesh_link_ingress_table_carrying_nothing(),
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
        register_from_glsl("scene-kernel", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL),
    )
    .handle_id;

    let held_output = sandbox
        .escalate(|full| {
            let output = full.acquire_texture(
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
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RunRayTracingKernel(req),
        )
        .expect("must produce a response")
    }
}

// ----- wire validation, before any device -----------------------

/// Both blobs are decoded before the escalate hop, so a malformed one
/// is refused without touching the GPU at all.
#[test]
fn register_blas_with_invalid_vertex_hex_returns_err() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_blas_with_invalid_vertex_hex: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                "blas-bad-vertices",
                "xyz123",
                &index_hex(A_SCENES_TRIANGLE_INDICES),
            )),
        )
        .expect("must produce a response"),
    );
    assert!(message.contains("vertices_hex"), "got: {message}");
}

#[test]
fn register_blas_with_invalid_index_hex_returns_err() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_blas_with_invalid_index_hex: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                "blas-bad-indices",
                &vertex_hex(A_SCENES_TRIANGLE_VERTICES),
                "xyz123",
            )),
        )
        .expect("must produce a response"),
    );
    assert!(message.contains("indices_hex"), "got: {message}");
}

/// A blob that is not a whole number of vertices — or of triangles —
/// names geometry that does not exist, and is refused before a build.
#[test]
fn register_blas_with_a_partial_vertex_or_triangle_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_blas_with_a_partial_vertex_or_triangle: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    for (request_id, vertices_hex, indices_hex) in [
        (
            "blas-partial-vertex",
            "00".repeat(11),
            index_hex(A_SCENES_TRIANGLE_INDICES),
        ),
        (
            "blas-partial-triangle",
            vertex_hex(A_SCENES_TRIANGLE_VERTICES),
            "00".repeat(8),
        ),
    ] {
        let message = refusal_message(
            handle_escalate_op(
                &sandbox,
                &registry,
                &a_mesh_link_ingress_table_carrying_nothing(),
                EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                    request_id,
                    &vertices_hex,
                    &indices_hex,
                )),
            )
            .expect("must produce a response"),
        );
        assert!(
            message.contains("multiple of 12"),
            "{request_id} got: {message}"
        );
    }
}

#[test]
fn register_tlas_with_no_instances_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_tlas_with_no_instances: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = make_tlas_req("tlas-empty", "unused");
    req.instances.clear();
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RegisterAccelerationStructureTlas(req),
        )
        .expect("must produce a response"),
    );
    assert!(message.contains("at least one instance"), "got: {message}");
}

#[test]
fn register_tlas_with_a_transform_that_is_not_a_row_major_3x4_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_tlas_with_a_wrong_length_transform: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = make_tlas_req("tlas-bad-transform", "unused");
    req.instances[0].transform = vec![1.0; 11];
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RegisterAccelerationStructureTlas(req),
        )
        .expect("must produce a response"),
    );
    assert!(message.contains("transform"), "got: {message}");
}

#[test]
fn register_tlas_with_a_mask_wider_than_eight_bits_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_tlas_with_an_oversized_mask: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = make_tlas_req("tlas-bad-mask", "unused");
    req.instances[0].mask = 0xfff;
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RegisterAccelerationStructureTlas(req),
        )
        .expect("must produce a response"),
    );
    assert!(message.contains("mask"), "got: {message}");
}

#[test]
fn run_with_invalid_push_constants_hex_returns_err() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("run_with_invalid_push_constants_hex: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = make_run_req("trace-bad-push", "kernel-x", "tlas-x", "surface-x");
    req.push_constants_hex = "qq".to_string();
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RunRayTracingKernel(req),
        )
        .expect("must produce a response"),
    );
    assert!(message.contains("push_constants_hex"), "got: {message}");
}

/// A stage mask is a bitfield the caller writes by hand, and a bit
/// outside the six ray-tracing stages names a stage no pipeline has a
/// module for at all.
#[test]
fn a_stage_mask_naming_a_bit_no_ray_tracing_stage_owns_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_stage_mask_naming_an_unowned_bit: no GPU — skipping");
        return;
    };
    let bit_no_stage_owns = RayTracingShaderStageFlags::ALL.bits() + 1;

    let mut declaration = register_from_glsl("k", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL);
    declaration.bindings[0].stages = bit_no_stage_owns;
    let message = prepare_ray_tracing_kernel_registration(&sandbox, declaration)
        .err()
        .expect("a binding declared for a stage no pipeline has must be refused");
    assert!(
        message.contains("scene_geometry") && message.contains("no ray-tracing stage owns"),
        "must name the binding and why the mask is wrong, got: {message}"
    );

    let mut push_constants = register_from_glsl("k", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL);
    push_constants.push_constant_stages = bit_no_stage_owns;
    let message = prepare_ray_tracing_kernel_registration(&sandbox, push_constants)
        .err()
        .expect("a push-constant range declared for the same stage must be refused");
    assert!(
        message.contains("push_constant_stages"),
        "must name the field, got: {message}"
    );
}

/// A procedural hit group without an intersection stage is a group with
/// nothing to intersect, and the sentinel is what "absent" looks like on
/// a wire where the field is always present.
#[test]
fn a_procedural_group_leaving_intersection_at_the_sentinel_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_procedural_group_without_an_intersection: no GPU — skipping");
        return;
    };
    let mut req = register_from_glsl("k-proc", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL);
    req.groups[2] = EscalateRequestRegisterRayTracingKernelGroup {
        kind: EscalateRequestRegisterRayTracingKernelGroupKind::ProceduralHit,
        general_stage: RAY_TRACING_STAGE_INDEX_NONE,
        closest_hit_stage: 2,
        any_hit_stage: RAY_TRACING_STAGE_INDEX_NONE,
        intersection_stage: RAY_TRACING_STAGE_INDEX_NONE,
    };
    let message = prepare_ray_tracing_kernel_registration(&sandbox, req)
        .err()
        .expect("a procedural group with no intersection stage must be refused");
    assert!(message.contains("procedural_hit"), "got: {message}");
}

/// A stage's hex is decoded before the escalate hop, and the refusal
/// names the stage it came from rather than just "the kernel".
#[test]
fn register_with_invalid_stage_hex_returns_err() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_with_invalid_stage_hex: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = register_from_glsl("k-bad-hex", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL);
    req.stages[1].source = String::new();
    req.stages[1].spv_hex = "qq".to_string();
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RegisterRayTracingKernel(req),
        )
        .expect("must produce a response"),
    );
    assert!(message.contains("stages[1].spv_hex"), "got: {message}");
}

/// Every ray-tracing stage the wire can name compiles for the stage it
/// names, and reaches the kernel classified as that stage.
///
/// Two separate six-arm mappings run per stage —
/// `ray_tracing_pipeline_stage_from_wire` picks what the compiler
/// targets and `ray_tracing_stage_from_wire` picks what the shader
/// group is built from — so a swapped pair in either would build a miss
/// shader as a closest-hit without complaint. Each body below is legal
/// only in its own stage: `rayPayloadEXT` is raygen-only,
/// `rayPayloadInEXT` is miss/hit-only, `reportIntersectionEXT` is
/// intersection-only and `callableDataInEXT` is callable-only, so a
/// mis-mapped compile target fails to compile rather than quietly
/// producing the wrong module.
#[test]
fn every_ray_tracing_wire_stage_compiles_for_the_stage_it_names() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("every_ray_tracing_wire_stage_compiles: no GPU — skipping");
        return;
    };
    let stages = [
        (
            EscalateRequestRegisterRayTracingKernelStageStage::RayGen,
            RayTracingShaderStage::RayGen,
            "layout(location = 0) rayPayloadEXT vec3 payload;\nvoid main() { payload = vec3(1.0); }",
        ),
        (
            EscalateRequestRegisterRayTracingKernelStageStage::Miss,
            RayTracingShaderStage::Miss,
            "layout(location = 0) rayPayloadInEXT vec3 payload;\nvoid main() { payload = vec3(0.0); }",
        ),
        (
            EscalateRequestRegisterRayTracingKernelStageStage::ClosestHit,
            RayTracingShaderStage::ClosestHit,
            "layout(location = 0) rayPayloadInEXT vec3 payload;\nvoid main() { payload = vec3(0.5); }",
        ),
        (
            EscalateRequestRegisterRayTracingKernelStageStage::AnyHit,
            RayTracingShaderStage::AnyHit,
            "layout(location = 0) rayPayloadInEXT vec3 payload;\nvoid main() { ignoreIntersectionEXT; }",
        ),
        (
            EscalateRequestRegisterRayTracingKernelStageStage::Intersection,
            RayTracingShaderStage::Intersection,
            "hitAttributeEXT vec2 barycentric;\nvoid main() { reportIntersectionEXT(1.0, 0u); }",
        ),
        (
            EscalateRequestRegisterRayTracingKernelStageStage::Callable,
            RayTracingShaderStage::Callable,
            "layout(location = 0) callableDataInEXT vec3 callable_payload;\nvoid main() { callable_payload = vec3(1.0); }",
        ),
    ];
    for (index, (wire_stage, expected_stage, body)) in stages.into_iter().enumerate() {
        let mut req = register_from_glsl(
            &format!("rt-glsl-{index}"),
            TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL,
        );
        req.bindings = Vec::new();
        req.groups = vec![general_group(0)];
        req.stages = vec![stage_from_glsl(
            wire_stage,
            &format!("#version 460\n#extension GL_EXT_ray_tracing : require\n{body}\n"),
        )];
        let prepared = prepare_ray_tracing_kernel_registration(&sandbox, req)
            .unwrap_or_else(|e| panic!("{wire_stage:?} did not compile as itself: {e}"));
        assert_eq!(
            prepared.stages[0].spirv.get(..4),
            Some(&SPIRV_MAGIC_LE[..]),
            "{wire_stage:?} reached the kernel as something other than SPIR-V"
        );
        assert_eq!(
            prepared.stages[0].stage, expected_stage,
            "{wire_stage:?} was classified as the wrong pipeline stage"
        );
    }
}

// ----- registration, over a ray-tracing device ------------------

/// Registration hands back the shape a trace needs: the shaders' own
/// names, each with the kind only the shaders know.
#[test]
fn registration_answers_with_the_shaders_binding_names_and_kinds() {
    let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
        println!("registration_answers_with_the_shaders_bindings: no RT device — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let ok = register_ray_tracing_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_glsl("reg", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL),
    );
    let bindings = ok.bindings.expect("a register response carries the shape");
    assert_eq!(
        bindings
            .iter()
            .map(|binding| (binding.name.as_str(), binding.kind.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("scene_geometry", "acceleration_structure"),
            ("traced_output", "storage_image"),
        ],
        "the raygen's own bindings, named and kinded as it declares them"
    );
}

/// Re-registering an identical kernel is free and keeps its id; a
/// different raygen stage is a different pipeline and gets its own.
#[test]
fn an_identical_registration_keeps_its_kernel_id_and_a_different_one_gets_its_own() {
    let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
        println!("an_identical_registration_keeps_its_kernel_id: no RT device — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let first = register_ray_tracing_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_glsl("a", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL),
    )
    .handle_id;
    let second = register_ray_tracing_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_glsl("b", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL),
    )
    .handle_id;
    assert_eq!(
        first, second,
        "an identical descriptor must produce the same kernel_id"
    );

    let other = register_ray_tracing_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_glsl("c", TRACE_AND_INVERT_RAY_GEN_GLSL),
    )
    .handle_id;
    assert_ne!(
        first, other,
        "a different raygen stage must produce a different kernel_id"
    );

    let held = sandbox
        .escalate(|full| {
            Ok((
                full.ray_tracing_kernel_by_id(&first),
                full.ray_tracing_kernel_by_id(&second),
            ))
        })
        .expect("the cache answers inside an escalate scope");
    let (a, b) = (held.0.expect("cached"), held.1.expect("cached"));
    assert!(
        std::sync::Arc::ptr_eq(&a, &b),
        "the second registration must reuse the first kernel, not build another"
    );
}

/// The cache key covers the shaders and the pipeline, not the caller's
/// assertion — so a wrong declaration refuses identically whether or
/// not somebody registered this kernel first.
#[test]
fn a_wrong_declaration_is_refused_even_when_the_kernel_is_cached() {
    let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
        println!("a_wrong_declaration_is_refused_when_cached: no RT device — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    register_ray_tracing_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_glsl("warm", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL),
    );

    let mut req = register_from_glsl("reg-wrong", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL);
    req.bindings = vec![EscalateRequestRegisterRayTracingKernelBinding {
        kind: EscalateRayTracingBindingKind::StorageBuffer,
        name: "scene_parameters".to_string(),
        stages: 0,
    }];
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RegisterRayTracingKernel(req),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("`scene_parameters`") && message.contains("`scene_geometry`"),
        "the refusal must name the bogus binding and the shaders' own: {message}"
    );
}

// ----- acceleration structures, over a ray-tracing device --------

/// Unlike a kernel, a structure holds device memory proportional to its
/// mesh — so every registration mints its own id rather than colliding
/// on content, and a TLAS is a different structure from its BLAS.
#[test]
fn every_acceleration_structure_registration_gets_its_own_id() {
    let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
        println!("every_acceleration_structure_registration: no RT device — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let first_blas = register_acceleration_structure_or_panic(
        &sandbox,
        &registry,
        EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
            "blas-a",
            &vertex_hex(A_SCENES_TRIANGLE_VERTICES),
            &index_hex(A_SCENES_TRIANGLE_INDICES),
        )),
    );
    let second_blas = register_acceleration_structure_or_panic(
        &sandbox,
        &registry,
        EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
            "blas-b",
            &vertex_hex(A_SCENES_TRIANGLE_VERTICES),
            &index_hex(A_SCENES_TRIANGLE_INDICES),
        )),
    );
    assert_ne!(
        first_blas, second_blas,
        "an identical mesh registered twice is two structures, not one"
    );

    let tlas = register_acceleration_structure_or_panic(
        &sandbox,
        &registry,
        EscalateRequest::RegisterAccelerationStructureTlas(make_tlas_req("tlas-a", &first_blas)),
    );
    assert_ne!(tlas, first_blas);
    assert_ne!(tlas, second_blas);
}

/// A structure is the one escalate-minted resource whose device memory
/// is proportional to what the caller supplied, so a long-running helper
/// has to be able to hand it back — the same `release_handle` a surface
/// is handed back through, since nothing else would reach the registry.
#[test]
fn a_registered_acceleration_structure_is_released_through_release_handle() {
    let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
        println!("a_registered_acceleration_structure_is_released: no RT device — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let blas = register_acceleration_structure_or_panic(
        &sandbox,
        &registry,
        EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
            "blas-to-release",
            &vertex_hex(A_SCENES_TRIANGLE_VERTICES),
            &index_hex(A_SCENES_TRIANGLE_INDICES),
        )),
    );

    let released = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: "release-blas".to_string(),
            handle_id: blas.clone(),
        }),
    )
    .expect("must produce a response");
    assert!(
        matches!(released, EscalateResponse::Ok(_)),
        "releasing a registered structure must succeed: {released:?}"
    );

    // The id is gone, not merely unreferenced: a second release finds
    // nothing, and a trace naming it would too.
    let released_twice = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: "release-blas-again".to_string(),
            handle_id: blas,
        }),
    )
    .expect("must produce a response");
    let message = refusal_message(released_twice);
    assert!(message.contains("not found in registry"), "{message}");
}

#[test]
fn a_tlas_instance_naming_an_unregistered_structure_is_refused() {
    let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
        println!("a_tlas_instance_naming_an_unregistered_structure: no RT device — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RegisterAccelerationStructureTlas(make_tlas_req(
                "tlas-unknown",
                "definitely-not-a-registered-structure",
            )),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("names no acceleration structure registered under id"),
        "got: {message}"
    );
}

/// A TLAS instance references a bottom-level structure. Naming a
/// top-level one is a caller mistake the registry can catch, and every
/// id looks alike from the outside.
#[test]
fn a_tlas_instance_naming_a_top_level_structure_is_refused() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("a_tlas_instance_naming_a_top_level_structure: no RT device — skipping");
        return;
    };
    let message = refusal_message(
        handle_escalate_op(
            &scene.sandbox,
            &scene.registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            EscalateRequest::RegisterAccelerationStructureTlas(make_tlas_req(
                "tlas-over-tlas",
                &scene.tlas_id,
            )),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("is a top-level structure"),
        "got: {message}"
    );
}

// ----- tracing, over a ray-tracing device ------------------------

#[test]
fn tracing_with_an_unregistered_kernel_id_is_refused() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("tracing_with_an_unregistered_kernel_id: no RT device — skipping");
        return;
    };
    let mut req = scene.run_req("trace-unknown-kernel");
    req.kernel_id = "definitely-not-a-registered-kernel".to_string();
    let message = refusal_message(scene.trace(req));
    assert!(
        message.contains("no kernel registered under id"),
        "got: {message}"
    );
}

/// An acceleration-structure binding resolves through the structure
/// registry rather than through a surface, so an id no registration
/// minted is refused there rather than falling through to the surface
/// planner and getting a surface's error text.
#[test]
fn a_trace_naming_an_unregistered_acceleration_structure_is_refused() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("a_trace_naming_an_unregistered_structure: no RT device — skipping");
        return;
    };
    let mut req = scene.run_req("trace-unknown-structure");
    req.bindings[0].target_id = "definitely-not-a-registered-structure".to_string();
    let message = refusal_message(scene.trace(req));
    assert!(
        message.contains("binding `scene_geometry` names no acceleration structure"),
        "must name the binding, got: {message}"
    );
}

/// The structure a trace binds is the top-level one a
/// `register_acceleration_structure_tlas` returned; a BLAS id is the
/// same shape of string and would otherwise reach the descriptor.
#[test]
fn a_trace_binding_a_bottom_level_structure_is_refused() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("a_trace_binding_a_bottom_level_structure: no RT device — skipping");
        return;
    };
    let mut req = scene.run_req("trace-blas");
    req.bindings[0].target_id = scene.blas_id.clone();
    let message = refusal_message(scene.trace(req));
    assert!(
        message.contains("is a bottom-level structure"),
        "got: {message}"
    );
}

/// The acceleration structure never reaches the surface planner, so its
/// missing-binding rule is enforced separately — and has to fire.
#[test]
fn a_declared_acceleration_structure_left_out_is_refused() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("a_declared_acceleration_structure_left_out: no RT device — skipping");
        return;
    };
    let mut req = scene.run_req("trace-no-structure");
    req.bindings
        .retain(|binding| binding.name != "scene_geometry");
    let message = refusal_message(scene.trace(req));
    assert!(
        message.contains("binding `scene_geometry` was not supplied"),
        "must name the missing binding, got: {message}"
    );
    assert!(
        message.contains("do not persist between traces"),
        "must say why there is no fallback, got: {message}"
    );
}

/// Supplying the structure under another kind would otherwise send it
/// to the surface planner, which would look for a surface named by an
/// `as_id`.
#[test]
fn an_acceleration_structure_supplied_as_another_kind_is_refused() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("an_acceleration_structure_supplied_as_another_kind: no RT — skipping");
        return;
    };
    let mut req = scene.run_req("trace-wrong-kind");
    req.bindings[0].kind = EscalateRayTracingBindingKind::StorageImage;
    let message = refusal_message(scene.trace(req));
    assert!(
        message.contains("binding `scene_geometry` was supplied as storage_image"),
        "must name the binding and the kind supplied, got: {message}"
    );
    assert!(
        message.contains("declares it acceleration_structure"),
        "must name the kind the kernel declares, got: {message}"
    );
}

/// The whole array is checked before the acceleration structures are
/// split out of it, so a name supplied twice is refused whichever half
/// the second copy would land in — and by the one rule the surface
/// planner spells, not a second wording of it.
#[test]
fn a_name_supplied_twice_is_refused() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("a_name_supplied_twice: no RT device — skipping");
        return;
    };
    let mut req = scene.run_req("trace-twice");
    let structure_binding = req.bindings[0].clone();
    assert_eq!(
        structure_binding.name, "scene_geometry",
        "the duplicate has to be the structure, which the surface planner never sees"
    );
    req.bindings.push(structure_binding);
    let message = refusal_message(scene.trace(req));
    assert!(
        message.contains("binding `scene_geometry` was supplied twice"),
        "must name the duplicate, got: {message}"
    );
    assert!(
        message.contains("`traced_output`"),
        "must name every binding this kernel declares, got: {message}"
    );
    assert!(
        message.contains("exactly once per trace"),
        "must state the rule in the caller's own noun, got: {message}"
    );
}

/// The op end to end, over a real device: a trace resolves the scene
/// and the storage image by the raygen's own names for them, launches
/// the grid, and leaves the engine's layout record agreeing with the
/// layout the trace left the image in.
///
/// The storage image is seeded by a transfer with a sentinel no stage
/// can produce, so a trace that bound nothing fails on the pixels
/// rather than passing on undefined contents — and because that seed
/// leaves the image in `TRANSFER_DST_OPTIMAL`, a trace that did not
/// barrier its bound inputs would write it through a descriptor its
/// layout does not satisfy. Both the hit and the miss stage must have
/// run, which is what proves the structure reached the descriptor.
#[test]
fn a_trace_resolves_its_bindings_by_name_and_writes_the_storage_image() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("a_trace_resolves_its_bindings_by_name: no RT device — skipping");
        return;
    };
    match scene.trace(scene.run_req("trace-scene")) {
        EscalateResponse::Ok(ok) => {
            assert_eq!(ok.request_id, "trace-scene");
            assert_eq!(
                ok.handle_id, scene.kernel_id,
                "the trace response echoes the kernel_id"
            );
            assert!(
                ok.timeline_value.is_none(),
                "run_ray_tracing_kernel responses carry no timeline"
            );
        }
        other => panic!("the trace failed: {other:?}"),
    }

    // Asserted before the readback, which transitions the image itself:
    // an unpublished layout would leave the next consumer's barrier
    // naming an oldLayout the image has already left.
    let published = scene
        .sandbox
        .escalate(|full| {
            Ok(full
                .resolve_texture_registration_by_surface_id(
                    A_TRACED_SCENES_OUTPUT_SURFACE_UUID,
                    None,
                    TRACED_GRID_WIDTH,
                    TRACED_GRID_HEIGHT,
                )?
                .current_layout())
        })
        .expect("the storage image still resolves");
    assert_eq!(
        published,
        streamlib_consumer_rhi::VulkanLayout::GENERAL,
        "the trace must publish the layout it left the storage image in"
    );

    let traced = scene
        .sandbox
        .escalate(|full| {
            let readback = full.create_texture_readback(
                "trace-readback",
                TRACED_GRID_WIDTH,
                TRACED_GRID_HEIGHT,
                TextureFormat::Rgba8Unorm,
            )?;
            let ticket = readback.submit(
                scene._held_output.texture(),
                crate::core::rhi::TextureSourceLayout::General,
            )?;
            Ok(readback.wait_and_read(ticket, 2_000_000_000)?.to_vec())
        })
        .expect("the storage image reads back");

    let mut hit_pixels = 0usize;
    let mut missed_pixels = 0usize;
    for (pixel_index, pixel) in traced.chunks_exact(4).enumerate() {
        if pixel == HIT_RGBA {
            hit_pixels += 1;
        } else if pixel == MISSED_RGBA {
            missed_pixels += 1;
        } else {
            panic!(
                "pixel {pixel_index} is {pixel:?}, which no stage of this kernel writes — \
                 the trace left the seeded sentinel, so `traced_output` was never written"
            );
        }
    }
    assert!(
        hit_pixels > 0,
        "no pixel hit the scene's triangle — `scene_geometry` did not reach the descriptor"
    );
    assert!(
        missed_pixels > 0,
        "every pixel hit, so the launch grid never left the triangle and the miss stage \
         proved nothing"
    );
}
