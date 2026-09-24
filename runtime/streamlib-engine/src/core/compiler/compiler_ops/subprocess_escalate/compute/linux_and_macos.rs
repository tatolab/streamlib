// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use super::super::hex_encoded_wire_bytes::decode_hex;
use super::super::kernel_shader_stage_source::registered_shader_stage_source;
use super::super::surface_bound_kernel_binding::{
    publish_bound_surface_layouts_to_surface_share, reflected_kernel_binding_response,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateComputeBindingKind, EscalateRequestRegisterComputeKernel,
    EscalateRequestRunComputeKernel, EscalateRequestRunComputeKernelBatch,
    EscalateRequestRunComputeKernelBinding,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::{
    BatchedComputeKernelDispatch, BatchedComputeKernelDispatchBinding, GpuContextLimitedAccess,
    TextureRegistration,
};
use crate::core::rhi::{GlslCompilationTargetStage, SurfaceBoundKernelBindingKind};
use crate::host_rhi::HostTextureExt as _;

/// The binding kind a wire enum names.
pub(super) fn compute_binding_kind_from_wire(
    kind: EscalateComputeBindingKind,
) -> crate::core::rhi::ComputeBindingKind {
    use crate::core::rhi::ComputeBindingKind;
    match kind {
        EscalateComputeBindingKind::SampledImage => ComputeBindingKind::SampledImage,
        EscalateComputeBindingKind::SampledTexture => ComputeBindingKind::SampledTexture,
        EscalateComputeBindingKind::StorageBuffer => ComputeBindingKind::StorageBuffer,
        EscalateComputeBindingKind::StorageImage => ComputeBindingKind::StorageImage,
        EscalateComputeBindingKind::UniformBuffer => ComputeBindingKind::UniformBuffer,
    }
}

/// The wire enum for a binding kind.
pub(super) fn compute_binding_kind_to_wire(
    kind: crate::core::rhi::ComputeBindingKind,
) -> EscalateComputeBindingKind {
    use crate::core::rhi::ComputeBindingKind;
    match kind {
        ComputeBindingKind::SampledImage => EscalateComputeBindingKind::SampledImage,
        ComputeBindingKind::SampledTexture => EscalateComputeBindingKind::SampledTexture,
        ComputeBindingKind::StorageBuffer => EscalateComputeBindingKind::StorageBuffer,
        ComputeBindingKind::StorageImage => EscalateComputeBindingKind::StorageImage,
        ComputeBindingKind::UniformBuffer => EscalateComputeBindingKind::UniformBuffer,
    }
}

/// Build a compute kernel for a subprocess customer, against `GpuContext`.
///
/// Reflection derives the binding shape and its names; the request's own
/// declaration is checked against it rather than replacing it. Re-registering
/// an identical kernel is a cache hit and answers with the same `kernel_id`.
///
/// The shader arrives as GLSL `source` the engine compiles, or as the
/// pre-compiled `spv_hex` escape hatch.
///
/// Failure modes (each an [`EscalateResponse::Err`] keyed by the request_id):
/// 1. Neither `source` nor `spv_hex` supplied, or both.
/// 2. `stage` names something other than `compute`, or nothing that is a
///    stage at all.
/// 3. `source` does not compile, or declares a non-`main` entry point.
/// 4. `spv_hex` doesn't decode as hex bytes.
/// 5. The blob's `OpName` decorations were stripped — bindings resolve by
///    name, so an unnamed binding cannot be bound at all.
/// 6. The declaration disagrees with reflection on a name or a kind.
/// 7. Push-constant size mismatch, or pipeline build failure.
pub(in super::super) fn handle_register_compute_kernel(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRegisterComputeKernel,
) -> EscalateResponse {
    // Parsed before it is judged, so a misspelling and a real-but-wrong stage
    // get different answers: one lists the stages that exist, the other says
    // which one this op means.
    if !req.stage.is_empty() {
        match GlslCompilationTargetStage::from_wire_name(&req.stage) {
            Ok(GlslCompilationTargetStage::Compute) => {}
            Ok(other) => {
                return EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!(
                        "register_compute_kernel carries stage `{}`; this op registers a \
                         compute kernel, so the only stage it compiles for is `{}`",
                        other.wire_name(),
                        GlslCompilationTargetStage::Compute.wire_name()
                    ),
                });
            }
            Err(e) => {
                return EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!("register_compute_kernel: {e}"),
                });
            }
        }
    }

    let shader_source = match registered_shader_stage_source(
        "",
        &req.source,
        &req.spv_hex,
        GlslCompilationTargetStage::Compute,
        &req.entry_point,
    ) {
        Ok(shader_source) => shader_source,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_compute_kernel: {e}"),
            });
        }
    };

    let spv = match shader_source.spirv(sandbox) {
        Ok(spv) => spv,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_compute_kernel: {e}"),
            });
        }
    };

    let declared: Vec<crate::core::rhi::ComputeBindingDeclaration> = req
        .bindings
        .iter()
        .map(|wire| crate::core::rhi::ComputeBindingDeclaration {
            name: wire.name.clone(),
            kind: compute_binding_kind_from_wire(wire.kind),
        })
        .collect();

    let registered = sandbox
        .escalate(|full| {
            full.create_or_reuse_compute_kernel(
                &spv,
                req.push_constant_size,
                &declared,
                shader_source.entry_point(),
            )
        })
        .and_then(|(kernel_id, kernel)| {
            // The caller dispatches by name and only the shader knows which
            // kind each name is, so the shape goes back with the id.
            let bindings = kernel
                .bindings()
                .iter()
                .map(|spec| {
                    reflected_kernel_binding_response(
                        &kernel_id,
                        spec.binding,
                        compute_binding_kind_to_wire(spec.kind).wire_name(),
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
            message: format!("register_compute_kernel failed: {e}"),
        }),
    }
}

/// Dispatch a registered compute kernel with its bindings resolved by name.
///
/// Compute dispatch on the host is synchronous — the dispatch runs as a
/// recording of one on the batch machinery, which waits out its submission
/// before returning — so by the time this emits an `Ok`, the GPU work has
/// retired and the writes are visible to any later submission on the same
/// device. The subprocess can advance its surface-share timeline on receipt.
///
/// Every binding error raises here, before anything is submitted, and names the
/// shader's own bindings so the caller can see what it should have supplied.
pub(in super::super) fn handle_run_compute_kernel(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRunComputeKernel,
) -> EscalateResponse {
    let push_constants = match decode_hex(&req.push_constants_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("run_compute_kernel: push_constants_hex decode: {e}"),
            });
        }
    };

    let dispatched = sandbox.escalate(|full| {
        let kernel = full.compute_kernel_by_id(&req.kernel_id).ok_or_else(|| {
            crate::core::error::Error::GpuError(format!(
                "run_compute_kernel: no kernel registered under id {:?}",
                req.kernel_id
            ))
        })?;
        bind_and_dispatch_compute_kernel(full, kernel, &req, push_constants)
    });

    match dispatched {
        Ok(()) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            // Echo the kernel_id back — compute is sync host-side, no
            // separate handle is allocated per dispatch.
            handle_id: req.kernel_id,
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("run_compute_kernel failed: {e}"),
        }),
    }
}

/// What one validated binding resolved to: the slot to write, the kind to
/// write it as, and the surface to look up.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct PlannedComputeBinding<'a> {
    pub(super) binding: u32,
    pub(super) kind: SurfaceBoundKernelBindingKind,
    pub(super) name: &'a str,
    pub(super) target_id: &'a str,
}

/// Match a dispatch's supplied bindings against the kernel's declared ones.
///
/// Every failure here is raised before any resource is bound and long before a
/// submission, and every message names the shader's own bindings. Bindings do
/// not persist on a kernel, so a dispatch supplies all of them or none:
///
/// - **duplicate** — one name supplied twice. Not expressible in a Python
///   mapping, which is why this is checked against the wire array rather than
///   left to the caller's language.
/// - **unknown** — a name the shader does not declare.
/// - **missing** — a declared name the dispatch omitted. There is no implicit
///   default and no carried-over value.
/// - **kind mismatch** — a name supplied as a kind the shader disagrees with.
/// - **unbindable kind** — a declared kind no surface can be named for
///   (buffers, samplerless images). Checked here so the plan is total before
///   any `set_*` call mutates the kernel's staged bindings.
pub(super) fn plan_supplied_compute_bindings<'a>(
    supplied: &'a [EscalateRequestRunComputeKernelBinding],
    declared: &'a [crate::core::rhi::ComputeBindingSpec],
) -> crate::core::error::Result<Vec<PlannedComputeBinding<'a>>> {
    use crate::core::error::Error;
    use crate::core::rhi::ComputeBindingKind;

    let declared_names: Vec<&str> = declared.iter().filter_map(|s| s.name.as_deref()).collect();
    // Built only when a refusal fires — this runs per frame, and the happy
    // path should not pay for the error text.
    let shader_declares = || crate::core::rhi::quote_declared_shader_binding_names(&declared_names);

    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for wire in supplied {
        if !seen.insert(wire.name.as_str()) {
            return Err(Error::GpuError(format!(
                "binding `{}` was supplied twice; this shader declares {}, each supplied \
                 exactly once per dispatch",
                wire.name,
                shader_declares()
            )));
        }
    }

    for name in &declared_names {
        if !seen.contains(name) {
            return Err(Error::GpuError(format!(
                "binding `{name}` was not supplied; bindings do not persist between dispatches, \
                 so every dispatch supplies all of {}",
                shader_declares()
            )));
        }
    }

    let mut planned = Vec::with_capacity(supplied.len());
    for wire in supplied {
        let spec = declared
            .iter()
            .find(|s| s.name.as_deref() == Some(wire.name.as_str()))
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "binding `{}` is not one this shader declares; it declares {}",
                    wire.name,
                    shader_declares()
                ))
            })?;
        let supplied_kind = compute_binding_kind_from_wire(wire.kind);
        if spec.kind != supplied_kind {
            return Err(Error::GpuError(format!(
                "binding `{}` was supplied as {:?} but this shader declares it {:?}",
                wire.name, supplied_kind, spec.kind
            )));
        }
        let surface_bound_kind = match spec.kind {
            ComputeBindingKind::StorageImage => SurfaceBoundKernelBindingKind::StorageImage,
            ComputeBindingKind::SampledTexture => SurfaceBoundKernelBindingKind::SampledTexture,
            ComputeBindingKind::SampledImage
            | ComputeBindingKind::StorageBuffer
            | ComputeBindingKind::UniformBuffer => {
                return Err(Error::GpuError(format!(
                    "binding `{}` is {:?}, which a dispatch cannot name a surface for — the \
                     surface-backed kinds are storage_image and sampled_texture",
                    wire.name, spec.kind
                )));
            }
        };
        planned.push(PlannedComputeBinding {
            binding: spec.binding,
            kind: surface_bound_kind,
            name: wire.name.as_str(),
            target_id: wire.target_id.as_str(),
        });
    }
    Ok(planned)
}

/// One resolved compute binding carried with the surface id it named, so
/// the transition and the layout publish pair by construction rather than
/// by a shared index — the desynchronisation rule
/// [`ResolvedSurfaceBoundKernelBinding`](crate::core::compiler::compiler_ops::subprocess_escalate::surface_bound_kernel_binding::ResolvedSurfaceBoundKernelBinding) documents.
pub(super) struct ResolvedComputeKernelDispatchBindingWithSurfaceId {
    surface_id: String,
    dispatch_binding: BatchedComputeKernelDispatchBinding,
}

/// Plan a dispatch's supplied bindings against the kernel, then resolve each
/// one to the device texture it names.
///
/// Shared by the two dispatch paths — one kernel on its own, and a kernel
/// inside a batch — so the extent convention below and the refusal wording
/// have one home rather than two that can drift.
pub(super) fn resolve_supplied_compute_bindings(
    full: &crate::core::context::GpuContextFullAccess,
    supplied: &[EscalateRequestRunComputeKernelBinding],
    kernel: &crate::vulkan::rhi::VulkanComputeKernel,
) -> crate::core::error::Result<Vec<ResolvedComputeKernelDispatchBindingWithSurfaceId>> {
    use crate::core::error::Error;

    // Borrowed, not cloned: this runs per frame, and the specs live on the
    // kernel for its whole life.
    let planned = plan_supplied_compute_bindings(supplied, kernel.host_inner().bindings())?;

    let mut resolved = Vec::with_capacity(planned.len());
    for binding in &planned {
        // Zero extent: a kernel binding names a surface the graph already has
        // as a device texture, which resolves from the same-process cache or
        // the surface-share service. The pixel-buffer fallback is the one
        // path that consults the extent, and it refuses a zero one — a
        // buffer-backed surface is not something a dispatch can bind.
        let registration = full
            .resolve_texture_registration_by_surface_id(binding.target_id, None, 0, 0)
            .map_err(|e| {
                Error::GpuError(format!(
                    "binding `{}` names surface {:?}, which this graph cannot resolve to a \
                     device texture: {e}",
                    binding.name, binding.target_id
                ))
            })?;
        resolved.push(ResolvedComputeKernelDispatchBindingWithSurfaceId {
            surface_id: binding.target_id.to_string(),
            dispatch_binding: BatchedComputeKernelDispatchBinding {
                binding: binding.binding,
                kind: binding.kind,
                registration,
            },
        });
    }

    // One image cannot serve two kinds in one dispatch. The descriptor layouts
    // are fixed and disagree — a combined image sampler is written
    // SHADER_READ_ONLY_OPTIMAL and a storage image GENERAL — so whatever layout
    // the texture is put in, one of the two descriptors is wrong.
    //
    // Compared after resolution and on the image, not on the id the caller
    // wrote: a published frame id and its pool slot are two spellings of one
    // texture (`<slot>#<generation>` resolves through the same cache entry as
    // `<slot>`), so a string comparison would let the pair through to exactly
    // the dispatch this refuses.
    for (index, (binding, plan)) in resolved.iter().zip(&planned).enumerate() {
        // A texture carrying no image is its own error, raised where the
        // descriptor would be written. Skipped rather than compared, because
        // two absent images are not one texture and refusing them here would
        // send the caller looking for a duplicate they did not write.
        let Some(image) = binding
            .dispatch_binding
            .registration
            .texture()
            .vulkan_inner()
            .image()
        else {
            continue;
        };
        let clashing = resolved[..index].iter().zip(&planned).find(|(prior, _)| {
            prior.dispatch_binding.kind != binding.dispatch_binding.kind
                && prior
                    .dispatch_binding
                    .registration
                    .texture()
                    .vulkan_inner()
                    .image()
                    == Some(image)
        });
        if let Some((prior, prior_plan)) = clashing {
            // Both ids, as the caller wrote them: a published frame id and its
            // pool slot are different strings for one texture, so naming only
            // one would leave the reader looking for a duplicate that is not
            // there on the page.
            return Err(Error::GpuError(format!(
                "bindings `{}` (surface {:?}) and `{}` (surface {:?}) name one texture but \
                 as {:?} and {:?}; no image layout satisfies both descriptors, so a \
                 dispatch reads and writes different surfaces or binds one of them alone",
                prior_plan.name,
                prior_plan.target_id,
                plan.name,
                plan.target_id,
                prior.dispatch_binding.kind,
                binding.dispatch_binding.kind
            )));
        }
    }
    Ok(resolved)
}

/// The `(surface id, registration)` pairs the post-dispatch layout publish
/// consumes, from resolver output — paired by construction, never by a
/// shared index.
pub(super) fn compute_bound_surface_layout_publish_pairs(
    resolved: &[ResolvedComputeKernelDispatchBindingWithSurfaceId],
) -> Vec<(String, TextureRegistration)> {
    resolved
        .iter()
        .map(|binding| {
            (
                binding.surface_id.clone(),
                binding.dispatch_binding.registration.clone(),
            )
        })
        .collect()
}

/// Run a recording of compute dispatches, then publish the layout every
/// bound surface landed in to the surface-share service — on success only,
/// so a refused recording leaves the published layouts as they arrived.
pub(super) fn dispatch_compute_recording_and_publish_bound_surface_layouts(
    full: &crate::core::context::GpuContextFullAccess,
    recording: &[BatchedComputeKernelDispatch],
    bound_surfaces: &[(String, TextureRegistration)],
) -> crate::core::error::Result<()> {
    full.dispatch_compute_kernel_batch(recording)?;
    publish_bound_surface_layouts_to_surface_share(full, bound_surfaces);
    Ok(())
}

/// Resolve every named binding, then run the dispatch as a recording of one
/// on the batch machinery — barriers and dispatch in one command buffer, one
/// submission, one fence wait.
///
/// The plan is total and every surface is resolved before anything records,
/// so a refused dispatch never leaves the kernel holding a mix of this
/// dispatch's bindings and the last one's. `VulkanComputeKernel::dispatch` is
/// not used here: its fence has no in-flight tracking, so a failed submit
/// would leave it unsignaled forever and hang the next dispatch.
pub(super) fn bind_and_dispatch_compute_kernel(
    full: &crate::core::context::GpuContextFullAccess,
    kernel: Arc<crate::vulkan::rhi::VulkanComputeKernel>,
    req: &EscalateRequestRunComputeKernel,
    push_constants: Vec<u8>,
) -> crate::core::error::Result<()> {
    let resolved = resolve_supplied_compute_bindings(full, &req.bindings, &kernel)?;
    let bound_surfaces = compute_bound_surface_layout_publish_pairs(&resolved);
    let dispatch = BatchedComputeKernelDispatch {
        kernel,
        bindings: resolved
            .into_iter()
            .map(|binding| binding.dispatch_binding)
            .collect(),
        push_constants,
        group_count_x: req.group_count_x,
        group_count_y: req.group_count_y,
        group_count_z: req.group_count_z,
    };
    dispatch_compute_recording_and_publish_bound_surface_layouts(
        full,
        std::slice::from_ref(&dispatch),
        &bound_surfaces,
    )
}

/// Run several dispatches as one recording: one submission, one fence wait.
///
/// The op exists because per-dispatch blocking is what a multi-pass filter
/// would otherwise pay N times over — `run_compute_kernel` submits and waits
/// every time. Here the caller pays once, and still returns with every write
/// visible, so nothing about the synchronous contract changes.
///
/// Every refusal — a decode, an unknown kernel, a binding that does not match
/// the shader, a surface this graph cannot resolve, one kernel named twice —
/// raises before the recording opens or aborts it, so a batch either runs
/// whole or submits nothing.
pub(in super::super) fn handle_run_compute_kernel_batch(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRunComputeKernelBatch,
) -> EscalateResponse {
    let mut push_constants_per_dispatch = Vec::with_capacity(req.dispatches.len());
    for (index, dispatch) in req.dispatches.iter().enumerate() {
        match decode_hex(&dispatch.push_constants_hex) {
            Ok(bytes) => push_constants_per_dispatch.push(bytes),
            Err(e) => {
                return EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!(
                        "run_compute_kernel_batch: dispatch {index}: push_constants_hex decode: {e}"
                    ),
                });
            }
        }
    }

    let dispatched = sandbox.escalate(move |full| {
        bind_and_dispatch_compute_kernel_batch(full, &req, push_constants_per_dispatch)
    });

    match dispatched {
        Ok(()) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("run_compute_kernel_batch failed: {e}"),
        }),
    }
}

/// Resolve every dispatch in a batch, then hand the lot to the recorder.
///
/// Resolution is complete before the first barrier is recorded: the same rule
/// the single-dispatch path follows, for the same reason — a refusal while a
/// command buffer is open costs an abort, and a partially-recorded batch is
/// not something a caller asked for.
pub(super) fn bind_and_dispatch_compute_kernel_batch(
    full: &crate::core::context::GpuContextFullAccess,
    req: &EscalateRequestRunComputeKernelBatch,
    push_constants_per_dispatch: Vec<Vec<u8>>,
) -> crate::core::error::Result<()> {
    use crate::core::error::Error;

    let mut bound_surfaces_across_the_batch: Vec<(String, TextureRegistration)> = Vec::new();
    let mut batch = Vec::with_capacity(req.dispatches.len());
    for ((index, dispatch), push_constants) in req
        .dispatches
        .iter()
        .enumerate()
        .zip(push_constants_per_dispatch)
    {
        let kernel = full
            .compute_kernel_by_id(&dispatch.kernel_id)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "dispatch {index} of this batch names no kernel registered under id {:?}",
                    dispatch.kernel_id
                ))
            })?;
        let resolved = resolve_supplied_compute_bindings(full, &dispatch.bindings, &kernel)
            .map_err(|e| Error::GpuError(format!("dispatch {index} of this batch: {e}")))?;
        bound_surfaces_across_the_batch
            .extend(compute_bound_surface_layout_publish_pairs(&resolved));
        let bindings = resolved
            .into_iter()
            .map(|binding| binding.dispatch_binding)
            .collect();
        batch.push(BatchedComputeKernelDispatch {
            kernel,
            bindings,
            push_constants,
            group_count_x: dispatch.group_count_x,
            group_count_y: dispatch.group_count_y,
            group_count_z: dispatch.group_count_z,
        });
    }

    dispatch_compute_recording_and_publish_bound_surface_layouts(
        full,
        &batch,
        &bound_surfaces_across_the_batch,
    )
}
