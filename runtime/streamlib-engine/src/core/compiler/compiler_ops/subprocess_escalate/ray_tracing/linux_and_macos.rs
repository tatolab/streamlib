// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use super::super::hex_encoded_wire_bytes::decode_hex;
use super::super::kernel_shader_stage_source::registered_shader_stage_source;
use super::super::surface_bound_kernel_binding::{
    DeclaredKernelBindingUnderPlanning, SuppliedKernelBindingUnderPlanning,
    bound_surface_layout_publish_pairs, descriptor_layout_transition_pairs,
    plan_supplied_surface_bound_kernel_bindings, publish_bound_surface_layouts_to_surface_share,
    reflected_kernel_binding_response, refuse_a_kernel_binding_name_supplied_twice,
    resolve_planned_surface_bound_kernel_bindings,
    transition_bound_kernel_inputs_into_descriptor_layouts,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRayTracingBindingKind, EscalateRequestRegisterAccelerationStructureBlas,
    EscalateRequestRegisterAccelerationStructureTlas, EscalateRequestRegisterRayTracingKernel,
    EscalateRequestRegisterRayTracingKernelGroupKind,
    EscalateRequestRegisterRayTracingKernelStageStage, EscalateRequestRunRayTracingKernel,
    RAY_TRACING_STAGE_INDEX_NONE,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::GpuContextLimitedAccess;
use crate::core::rhi::SurfaceBoundKernelBindingKind;

/// The RHI binding kind a ray-tracing wire enum names.
pub(super) fn ray_tracing_binding_kind_from_wire(
    kind: EscalateRayTracingBindingKind,
) -> crate::core::rhi::RayTracingBindingKind {
    use crate::core::rhi::RayTracingBindingKind;
    match kind {
        EscalateRayTracingBindingKind::AccelerationStructure => {
            RayTracingBindingKind::AccelerationStructure
        }
        EscalateRayTracingBindingKind::SampledTexture => RayTracingBindingKind::SampledTexture,
        EscalateRayTracingBindingKind::StorageBuffer => RayTracingBindingKind::StorageBuffer,
        EscalateRayTracingBindingKind::StorageImage => RayTracingBindingKind::StorageImage,
        EscalateRayTracingBindingKind::UniformBuffer => RayTracingBindingKind::UniformBuffer,
    }
}

/// The wire enum for a ray-tracing binding kind.
pub(super) fn ray_tracing_binding_kind_to_wire(
    kind: crate::core::rhi::RayTracingBindingKind,
) -> EscalateRayTracingBindingKind {
    use crate::core::rhi::RayTracingBindingKind;
    match kind {
        RayTracingBindingKind::AccelerationStructure => {
            EscalateRayTracingBindingKind::AccelerationStructure
        }
        RayTracingBindingKind::SampledTexture => EscalateRayTracingBindingKind::SampledTexture,
        RayTracingBindingKind::StorageBuffer => EscalateRayTracingBindingKind::StorageBuffer,
        RayTracingBindingKind::StorageImage => EscalateRayTracingBindingKind::StorageImage,
        RayTracingBindingKind::UniformBuffer => EscalateRayTracingBindingKind::UniformBuffer,
    }
}

/// Whether a ray-tracing binding kind is one a trace can name a surface for.
///
/// The acceleration structure is excluded because a trace resolves it through
/// the acceleration-structure registry, not through a surface.
pub(super) fn surface_bound_ray_tracing_binding_kind(
    kind: crate::core::rhi::RayTracingBindingKind,
) -> Option<SurfaceBoundKernelBindingKind> {
    use crate::core::rhi::RayTracingBindingKind;
    match kind {
        RayTracingBindingKind::SampledTexture => {
            Some(SurfaceBoundKernelBindingKind::SampledTexture)
        }
        RayTracingBindingKind::StorageImage => Some(SurfaceBoundKernelBindingKind::StorageImage),
        RayTracingBindingKind::AccelerationStructure
        | RayTracingBindingKind::StorageBuffer
        | RayTracingBindingKind::UniformBuffer => None,
    }
}

/// Build a triangle-geometry BLAS for a subprocess customer, against
/// `GpuContext`.
///
/// Decodes the hex-encoded vertex (`f32` triples) and index (`u32` triples)
/// blobs, checks triangle-shape consistency, and registers the built structure
/// under a fresh `as_id` a later trace names it by.
///
/// Failure modes (each an [`EscalateResponse::Err`] keyed by the request_id):
/// 1. `vertices_hex` / `indices_hex` doesn't decode as hex bytes.
/// 2. Vertex blob length is not a multiple of 12 (one vertex = 3 × f32).
/// 3. Index blob length is not a multiple of 12 (one triangle = 3 × u32).
/// 4. The device does not expose the `VK_KHR_ray_tracing_pipeline` chain.
/// 5. Empty geometry, or an acceleration-structure build failure.
pub(in super::super) fn handle_register_acceleration_structure_blas(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRegisterAccelerationStructureBlas,
) -> EscalateResponse {
    let vertex_bytes = match decode_hex(&req.vertices_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_acceleration_structure_blas: vertices_hex decode: {e}"),
            });
        }
    };
    if vertex_bytes.len() % 12 != 0 {
        return EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!(
                "register_acceleration_structure_blas: vertex blob length {} is not a \
                 multiple of 12 bytes (one vertex = 3 × f32)",
                vertex_bytes.len()
            ),
        });
    }
    let vertices: Vec<f32> = vertex_bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    let index_bytes = match decode_hex(&req.indices_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_acceleration_structure_blas: indices_hex decode: {e}"),
            });
        }
    };
    if index_bytes.len() % 12 != 0 {
        return EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!(
                "register_acceleration_structure_blas: index blob length {} is not a \
                 multiple of 12 bytes (one triangle = 3 × u32)",
                index_bytes.len()
            ),
        });
    }
    let indices: Vec<u32> = index_bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    let registered = sandbox.escalate(|full| {
        refuse_a_device_without_ray_tracing(full, "register_acceleration_structure_blas")?;
        let blas = full.build_triangles_blas(&req.label, &vertices, &indices)?;
        Ok(full.register_acceleration_structure(blas))
    });

    match registered {
        Ok(acceleration_structure_id) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id: acceleration_structure_id,
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("register_acceleration_structure_blas failed: {e}"),
        }),
    }
}

/// Build a TLAS over previously-registered BLASes, against `GpuContext`.
///
/// Each instance's transform is exactly 12 floats (row-major 3×4) and its mask
/// is 8-bit; the `blas_id` resolves through the acceleration-structure registry
/// and must name a bottom-level structure. The TLAS keeps every referenced BLAS
/// alive for its own lifetime.
///
/// Failure modes (each an [`EscalateResponse::Err`] keyed by the request_id):
/// 1. Empty instance list — a TLAS needs at least one instance per the spec.
/// 2. An instance transform is not 12 floats, or its mask exceeds 0xff.
/// 3. An instance's `flags` sets a bit no `VkGeometryInstanceFlagsKHR` owns.
/// 4. The device does not expose the `VK_KHR_ray_tracing_pipeline` chain.
/// 5. An unknown `blas_id`, a `blas_id` naming a TLAS, or a build failure.
pub(in super::super) fn handle_register_acceleration_structure_tlas(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRegisterAccelerationStructureTlas,
) -> EscalateResponse {
    if req.instances.is_empty() {
        return EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: "register_acceleration_structure_tlas: instances must not be empty (TLAS \
                 requires at least one instance per Vulkan spec)"
                .to_string(),
        });
    }
    for (idx, inst) in req.instances.iter().enumerate() {
        if inst.transform.len() != 12 {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!(
                    "register_acceleration_structure_tlas: instance {idx} transform has \
                     {} floats, expected exactly 12 (row-major 3×4)",
                    inst.transform.len()
                ),
            });
        }
        if inst.mask > 0xff {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!(
                    "register_acceleration_structure_tlas: instance {idx} mask {} > 0xff \
                     (mask is 8-bit; wire form is uint32)",
                    inst.mask
                ),
            });
        }
    }

    let registered = sandbox.escalate(|full| {
        use crate::core::error::Error;
        use crate::vulkan::rhi::{
            AccelerationStructureKind, TlasInstanceDesc, geometry_instance_flags_from_raw_bitmask,
        };

        refuse_a_device_without_ray_tracing(full, "register_acceleration_structure_tlas")?;

        let mut instances = Vec::with_capacity(req.instances.len());
        for (idx, inst) in req.instances.iter().enumerate() {
            let blas = full
                .acceleration_structure_by_id(&inst.blas_id)
                .ok_or_else(|| {
                    Error::GpuError(format!(
                        "instance {idx} names no acceleration structure registered under id {:?}",
                        inst.blas_id
                    ))
                })?;
            if blas.kind() != AccelerationStructureKind::BottomLevel {
                return Err(Error::GpuError(format!(
                    "instance {idx} names {:?}, which is a top-level structure; a TLAS instance \
                     references a bottom-level one",
                    inst.blas_id
                )));
            }
            let t = &inst.transform;
            instances.push(TlasInstanceDesc {
                transform: [
                    [t[0], t[1], t[2], t[3]],
                    [t[4], t[5], t[6], t[7]],
                    [t[8], t[9], t[10], t[11]],
                ],
                custom_index: inst.custom_index,
                mask: inst.mask as u8,
                sbt_record_offset: inst.sbt_record_offset,
                flags: geometry_instance_flags_from_raw_bitmask(inst.flags)
                    .map_err(|e| Error::GpuError(format!("instance {idx}: {e}")))?,
                blas: (*blas).clone(),
            });
        }

        let tlas = full.build_tlas(&req.label, &instances)?;
        Ok(full.register_acceleration_structure(tlas))
    });

    match registered {
        Ok(acceleration_structure_id) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id: acceleration_structure_id,
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("register_acceleration_structure_tlas failed: {e}"),
        }),
    }
}

/// Refuse an op that needs the ray-tracing pipeline on a device without it.
///
/// Raised before any build so the caller gets the device's own answer rather
/// than an extension-missing failure from inside a structure build.
pub(super) fn refuse_a_device_without_ray_tracing(
    full: &crate::core::context::GpuContextFullAccess,
    op: &str,
) -> crate::core::error::Result<()> {
    full.host_vulkan_device_arc()?
        .refuse_without_the_ray_tracing_tier(op)
}

/// The compiler's name for a ray-tracing wire stage.
///
/// Distinct from [`ray_tracing_stage_from_wire`], which maps the same wire
/// value to the stage a shader group is built from: one names a pipeline stage
/// to compile for, the other names the stage a module fills.
pub(super) fn ray_tracing_pipeline_stage_from_wire(
    stage: EscalateRequestRegisterRayTracingKernelStageStage,
) -> crate::core::rhi::GlslCompilationTargetStage {
    use EscalateRequestRegisterRayTracingKernelStageStage as Wire;

    use crate::core::rhi::GlslCompilationTargetStage as Compiled;
    match stage {
        Wire::AnyHit => Compiled::RayAnyHit,
        Wire::Callable => Compiled::RayCallable,
        Wire::ClosestHit => Compiled::RayClosestHit,
        Wire::Intersection => Compiled::RayIntersection,
        Wire::Miss => Compiled::RayMiss,
        Wire::RayGen => Compiled::RayGeneration,
    }
}

/// One compiled ray-tracing stage: which pipeline stage it fills, the SPIR-V
/// that fills it, and the entry point inside that blob.
pub(super) struct PreparedRayTracingKernelStage {
    pub(super) stage: crate::core::rhi::RayTracingShaderStage,
    pub(super) spirv: Arc<[u8]>,
    pub(super) entry_point: String,
}

/// Everything a `register_ray_tracing_kernel` settles before it takes the
/// device gate: every stage compiled, the group layout read, the declaration
/// read.
pub(super) struct PreparedRayTracingKernelRegistration {
    pub(super) label: String,
    pub(super) stages: Vec<PreparedRayTracingKernelStage>,
    pub(super) groups: Vec<crate::core::rhi::RayTracingShaderGroup>,
    pub(super) declared_bindings: Vec<crate::core::rhi::RayTracingBindingDeclaration>,
    pub(super) push_constants: crate::core::rhi::RayTracingPushConstants,
    pub(super) max_recursion_depth: u32,
}

/// Read a `register_ray_tracing_kernel` request into what `GpuContext` builds a
/// kernel from, without touching the device.
pub(super) fn prepare_ray_tracing_kernel_registration(
    sandbox: &GpuContextLimitedAccess,
    req: EscalateRequestRegisterRayTracingKernel,
) -> std::result::Result<PreparedRayTracingKernelRegistration, String> {
    use crate::core::rhi::{
        RayTracingBindingDeclaration, RayTracingPushConstants, RayTracingShaderGroup,
        RayTracingShaderStageFlags,
    };

    let mut stages = Vec::with_capacity(req.stages.len());
    for (idx, st) in req.stages.iter().enumerate() {
        let stage_source = registered_shader_stage_source(
            &format!("stages[{idx}]."),
            &st.source,
            &st.spv_hex,
            ray_tracing_pipeline_stage_from_wire(st.stage),
            &st.entry_point,
        )?;
        stages.push(PreparedRayTracingKernelStage {
            stage: ray_tracing_stage_from_wire(st.stage),
            spirv: stage_source.spirv(sandbox).map_err(|e| e.to_string())?,
            entry_point: stage_source.entry_point().to_string(),
        });
    }

    let mut groups: Vec<RayTracingShaderGroup> = Vec::with_capacity(req.groups.len());
    for (idx, g) in req.groups.iter().enumerate() {
        groups.push(match g.kind {
            EscalateRequestRegisterRayTracingKernelGroupKind::General => {
                RayTracingShaderGroup::General {
                    general: g.general_stage,
                }
            }
            EscalateRequestRegisterRayTracingKernelGroupKind::TrianglesHit => {
                RayTracingShaderGroup::TrianglesHit {
                    closest_hit: optional_stage(g.closest_hit_stage),
                    any_hit: optional_stage(g.any_hit_stage),
                }
            }
            EscalateRequestRegisterRayTracingKernelGroupKind::ProceduralHit => {
                if g.intersection_stage == RAY_TRACING_STAGE_INDEX_NONE {
                    return Err(format!(
                        "groups[{idx}] procedural_hit must set intersection_stage (got \
                         {RAY_TRACING_STAGE_INDEX_NONE} which is the absent-sentinel)"
                    ));
                }
                RayTracingShaderGroup::ProceduralHit {
                    intersection: g.intersection_stage,
                    closest_hit: optional_stage(g.closest_hit_stage),
                    any_hit: optional_stage(g.any_hit_stage),
                }
            }
        });
    }

    let mut declared_bindings = Vec::with_capacity(req.bindings.len());
    for wire in &req.bindings {
        declared_bindings.push(RayTracingBindingDeclaration {
            name: wire.name.clone(),
            kind: ray_tracing_binding_kind_from_wire(wire.kind),
            stages: RayTracingShaderStageFlags::from_bits(wire.stages).ok_or_else(|| {
                format!(
                    "binding `{}` names stages {:#b}, which sets a bit no ray-tracing stage owns \
                     (1 = ray_gen, 2 = miss, 4 = closest_hit, 8 = any_hit, 16 = intersection, \
                     32 = callable)",
                    wire.name, wire.stages
                )
            })?,
        });
    }

    let push_constants = RayTracingPushConstants {
        size: req.push_constant_size,
        stages: RayTracingShaderStageFlags::from_bits(req.push_constant_stages).ok_or_else(
            || {
                format!(
                    "push_constant_stages {:#b} sets a bit no ray-tracing stage owns",
                    req.push_constant_stages
                )
            },
        )?,
    };

    Ok(PreparedRayTracingKernelRegistration {
        label: req.label,
        stages,
        groups,
        declared_bindings,
        push_constants,
        max_recursion_depth: req.max_recursion_depth,
    })
}

/// Build a ray-tracing kernel for a subprocess customer, against `GpuContext`.
///
/// The ray-tracing twin of [`handle_register_compute_kernel`](crate::core::compiler::compiler_ops::subprocess_escalate::compute::handle_register_compute_kernel), over N stages
/// rather than one: reflection across every stage derives the binding shape and
/// its names, the request's own declaration is checked against it, and
/// re-registering an identical kernel is a cache hit that answers with the same
/// `kernel_id`.
///
/// Failure modes (each an [`EscalateResponse::Err`] keyed by the request_id):
/// 1. A stage supplies neither `source` nor `spv_hex`, or both; its source does
///    not compile; or its hex doesn't decode.
/// 2. A `procedural_hit` group leaves `intersection_stage` at the sentinel.
/// 3. A binding's or the push-constant range's `stages` mask sets a bit no
///    ray-tracing stage owns.
/// 4. The device does not expose the `VK_KHR_ray_tracing_pipeline` chain.
/// 5. The blobs' `OpName` decorations were stripped, or the declaration
///    disagrees with reflection on a name, a kind, or a stage.
/// 6. Group/stage inconsistency, push-constant size mismatch, or pipeline build
///    failure.
pub(in super::super) fn handle_register_ray_tracing_kernel(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRegisterRayTracingKernel,
) -> EscalateResponse {
    use crate::core::rhi::RayTracingStage;

    let prepared = match prepare_ray_tracing_kernel_registration(sandbox, req) {
        Ok(prepared) => prepared,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_ray_tracing_kernel: {e}"),
            });
        }
    };

    let stages: Vec<RayTracingStage<'_>> = prepared
        .stages
        .iter()
        .map(|prepared_stage| RayTracingStage {
            stage: prepared_stage.stage,
            spv: &prepared_stage.spirv,
            entry_point: &prepared_stage.entry_point,
        })
        .collect();

    let registered = sandbox
        .escalate(|full| {
            refuse_a_device_without_ray_tracing(full, "register_ray_tracing_kernel")?;
            full.create_or_reuse_ray_tracing_kernel(
                &prepared.label,
                &stages,
                &prepared.groups,
                prepared.push_constants,
                prepared.max_recursion_depth,
                &prepared.declared_bindings,
            )
        })
        .and_then(|(kernel_id, kernel)| {
            let bindings = kernel
                .bindings()
                .iter()
                .map(|spec| {
                    reflected_kernel_binding_response(
                        &kernel_id,
                        spec.binding,
                        ray_tracing_binding_kind_to_wire(spec.kind).wire_name(),
                        spec.name.as_deref(),
                    )
                })
                .collect::<crate::core::error::Result<Vec<_>>>()?;
            Ok((kernel_id, bindings))
        });

    match registered {
        Ok((kernel_id, bindings)) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id: kernel_id,
            bindings: Some(bindings),
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("register_ray_tracing_kernel failed: {e}"),
        }),
    }
}

/// Trace one grid with a registered ray-tracing kernel, its bindings resolved
/// by name.
///
/// The trace is synchronous on the host — `trace_rays` submits and waits on its
/// own fence — so by the time this emits an `Ok`, the GPU work has retired and
/// the writes to the output storage image are visible to any later submission
/// on the same device.
///
/// An `acceleration_structure` binding names an `as_id` a prior
/// `register_acceleration_structure_tlas` returned; every other kind names a
/// surface. Every binding error raises before anything is submitted, and names
/// the kernel's own bindings.
pub(in super::super) fn handle_run_ray_tracing_kernel(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRunRayTracingKernel,
) -> EscalateResponse {
    let push_constants = match decode_hex(&req.push_constants_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("run_ray_tracing_kernel: push_constants_hex decode: {e}"),
            });
        }
    };

    let traced = sandbox.escalate(|full| {
        let kernel = full
            .ray_tracing_kernel_by_id(&req.kernel_id)
            .ok_or_else(|| {
                crate::core::error::Error::GpuError(format!(
                    "run_ray_tracing_kernel: no kernel registered under id {:?}",
                    req.kernel_id
                ))
            })?;
        bind_and_trace_ray_tracing_kernel(full, &kernel, &req, &push_constants)
    });

    match traced {
        Ok(()) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            // Echo the kernel_id back — the trace is sync host-side, so no
            // separate handle is allocated per trace.
            handle_id: req.kernel_id,
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("run_ray_tracing_kernel failed: {e}"),
        }),
    }
}

/// Resolve every named binding onto the kernel's slots, then trace.
///
/// The plan is total and every target is resolved before the first `set_*`
/// call, so a refused trace never leaves the kernel holding a mix of this
/// trace's bindings and the last one's.
pub(super) fn bind_and_trace_ray_tracing_kernel(
    full: &crate::core::context::GpuContextFullAccess,
    kernel: &crate::vulkan::rhi::VulkanRayTracingKernel,
    req: &EscalateRequestRunRayTracingKernel,
    push_constants: &[u8],
) -> crate::core::error::Result<()> {
    use crate::core::error::Error;
    use crate::core::rhi::RayTracingBindingKind;
    use crate::vulkan::rhi::{AccelerationStructureKind, VulkanStage};

    let declared_specs = kernel.bindings();

    // Checked over the whole array before it is split, so a name supplied twice
    // is refused whichever half each copy would land in.
    let declared_names: Vec<&str> = declared_specs
        .iter()
        .filter_map(|spec| spec.name.as_deref())
        .collect();
    refuse_a_kernel_binding_name_supplied_twice(
        "trace",
        req.bindings.iter().map(|wire| wire.name.as_str()),
        &declared_names,
    )?;

    // The acceleration structures come out first: they resolve through their
    // own registry rather than through a surface, so the surface planner never
    // sees them and the kernel's declaration for them is checked here.
    let mut acceleration_structure_bindings = Vec::new();
    let mut surface_supplied = Vec::with_capacity(req.bindings.len());
    for wire in &req.bindings {
        let declared_as_acceleration_structure = declared_specs
            .iter()
            .find(|spec| spec.name.as_deref() == Some(wire.name.as_str()))
            .filter(|spec| spec.kind == RayTracingBindingKind::AccelerationStructure);
        let Some(declaration) = declared_as_acceleration_structure else {
            surface_supplied.push(SuppliedKernelBindingUnderPlanning {
                name: wire.name.as_str(),
                target_id: wire.target_id.as_str(),
                kind_wire_name: wire.kind.wire_name(),
            });
            continue;
        };
        if ray_tracing_binding_kind_from_wire(wire.kind)
            != RayTracingBindingKind::AccelerationStructure
        {
            return Err(Error::GpuError(format!(
                "binding `{}` was supplied as {} but this kernel declares it \
                 acceleration_structure",
                wire.name,
                wire.kind.wire_name()
            )));
        }
        let slot = declaration.binding;
        let tlas = full
            .acceleration_structure_by_id(&wire.target_id)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "binding `{}` names no acceleration structure registered under id {:?}",
                    wire.name, wire.target_id
                ))
            })?;
        if tlas.kind() != AccelerationStructureKind::TopLevel {
            return Err(Error::GpuError(format!(
                "binding `{}` names {:?}, which is a bottom-level structure; a trace binds the \
                 top-level one a `register_acceleration_structure_tlas` returned",
                wire.name, wire.target_id
            )));
        }
        acceleration_structure_bindings.push((slot, tlas));
    }

    // Declared acceleration structures are dropped from the surface planner's
    // view of the declaration too, so its missing-binding check counts only
    // what it is responsible for.
    let declared: Vec<DeclaredKernelBindingUnderPlanning<'_>> = declared_specs
        .iter()
        .filter(|spec| spec.kind != RayTracingBindingKind::AccelerationStructure)
        .map(|spec| DeclaredKernelBindingUnderPlanning {
            binding_slot: spec.binding,
            name: spec.name.as_deref(),
            kind_wire_name: ray_tracing_binding_kind_to_wire(spec.kind).wire_name(),
            surface_bound_kind: surface_bound_ray_tracing_binding_kind(spec.kind),
        })
        .collect();
    let planned =
        plan_supplied_surface_bound_kernel_bindings("trace", &surface_supplied, &declared)?;
    for spec in declared_specs
        .iter()
        .filter(|spec| spec.kind == RayTracingBindingKind::AccelerationStructure)
    {
        if acceleration_structure_bindings
            .iter()
            .any(|(slot, _)| *slot == spec.binding)
        {
            continue;
        }
        let declared_name = spec.name.as_deref().ok_or_else(|| {
            Error::GpuError(format!(
                "this kernel holds an unnamed acceleration-structure binding at slot {}; \
                 reflection refuses these, so this kernel did not come through registration",
                spec.binding
            ))
        })?;
        return Err(Error::GpuError(format!(
            "binding `{declared_name}` was not supplied; bindings do not persist between traces, \
             so every trace supplies all of them"
        )));
    }

    let bound_inputs = resolve_planned_surface_bound_kernel_bindings(full, planned)?;
    transition_bound_kernel_inputs_into_descriptor_layouts(
        full,
        "escalate_ray_tracing_trace_input_layouts",
        VulkanStage::ALL_COMMANDS,
        &descriptor_layout_transition_pairs(&bound_inputs),
    )?;

    for (slot, tlas) in &acceleration_structure_bindings {
        kernel.set_acceleration_structure(*slot, tlas)?;
    }
    for binding in &bound_inputs {
        let texture = binding.registration.texture();
        match binding.planned.kind {
            SurfaceBoundKernelBindingKind::SampledTexture => {
                kernel.set_sampled_texture(binding.planned.binding_slot, texture)?
            }
            SurfaceBoundKernelBindingKind::StorageImage => {
                kernel.set_storage_image(binding.planned.binding_slot, texture)?
            }
        }
    }

    // A kernel that declares push constants must be given them even when the
    // payload is empty, so `set_push_constants` produces the size mismatch
    // rather than the trace running against whatever the kernel's staged buffer
    // last held.
    if kernel.push_constant_size() > 0 || !push_constants.is_empty() {
        kernel.set_push_constants(push_constants)?;
    }

    let traced = kernel.trace_rays(req.width, req.height, req.depth);
    if traced.is_ok() {
        publish_bound_surface_layouts_to_surface_share(
            full,
            &bound_surface_layout_publish_pairs(&bound_inputs),
        );
    }
    drop(bound_inputs);
    traced
}

/// Convert a sentinel-encoded wire stage index back into an
/// `Option<u32>`. The wire form uses `0xFFFFFFFF` to mean "absent"
/// because the field is always present on the wire.
pub(super) fn optional_stage(idx: u32) -> Option<u32> {
    if idx == RAY_TRACING_STAGE_INDEX_NONE {
        None
    } else {
        Some(idx)
    }
}

/// The pipeline stage a ray-tracing wire stage's module fills.
pub(super) fn ray_tracing_stage_from_wire(
    stage: EscalateRequestRegisterRayTracingKernelStageStage,
) -> crate::core::rhi::RayTracingShaderStage {
    use EscalateRequestRegisterRayTracingKernelStageStage as W;

    use crate::core::rhi::RayTracingShaderStage;
    match stage {
        W::RayGen => RayTracingShaderStage::RayGen,
        W::Miss => RayTracingShaderStage::Miss,
        W::ClosestHit => RayTracingShaderStage::ClosestHit,
        W::AnyHit => RayTracingShaderStage::AnyHit,
        W::Intersection => RayTracingShaderStage::Intersection,
        W::Callable => RayTracingShaderStage::Callable,
    }
}
