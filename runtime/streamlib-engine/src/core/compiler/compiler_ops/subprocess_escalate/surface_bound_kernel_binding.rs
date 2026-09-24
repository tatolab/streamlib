// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Planning and resolving the surface-bound bindings a kernel invocation
//! names, shared by the compute, graphics and ray-tracing families.

use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseKernelBinding;
use crate::core::context::TextureRegistration;
use crate::core::rhi::SurfaceBoundKernelBindingKind;
use crate::host_rhi::HostTextureExt as _;

/// Publish each bound surface's post-dispatch layout to the surface-share
/// service, so a cross-process consumer's checkout names the layout the
/// dispatch actually left the image in — the service cell is otherwise
/// frozen at its registration-time UNDEFINED while the in-process
/// registration moves on.
///
/// Best-effort, escalate-path only: an id the service does not hold is an
/// in-process-only surface, not an error, and a publish failure costs the
/// consumer its content-preserving acquire, never the dispatch.
pub(super) fn publish_bound_surface_layouts_to_surface_share(
    full: &crate::core::context::GpuContextFullAccess,
    bound_surfaces: &[(String, TextureRegistration)],
) {
    let Some(store) = full.surface_store() else {
        return;
    };
    let mut published_surface_ids: Vec<&str> = Vec::with_capacity(bound_surfaces.len());
    // Walked back to front: a surface several dispatches of one batch bind
    // ends in the layout its *last* use required, and a cross-process
    // resolve holds a separate layout cell per occurrence — so the dedup
    // must keep the last one, not the first.
    for (surface_id, registration) in bound_surfaces.iter().rev() {
        if published_surface_ids.contains(&surface_id.as_str()) {
            continue;
        }
        published_surface_ids.push(surface_id);
        if let Err(publish_failure) =
            store.update_image_layout(surface_id, registration.current_layout())
        {
            tracing::debug!(
                "[escalate] layout publish for '{}' skipped: {}",
                surface_id,
                publish_failure
            );
        }
    }
}

/// One binding a draw or a trace supplied, as the planner reads it — whichever
/// wire array it arrived in.
pub(super) struct SuppliedKernelBindingUnderPlanning<'a> {
    pub(super) name: &'a str,
    pub(super) target_id: &'a str,
    pub(super) kind_wire_name: &'static str,
}

/// One binding a kernel declares, as the planner reads it.
pub(super) struct DeclaredKernelBindingUnderPlanning<'a> {
    pub(super) binding_slot: u32,
    /// `None` on a binding reflection left unnamed, which nothing can resolve
    /// by name.
    pub(super) name: Option<&'a str>,
    pub(super) kind_wire_name: &'static str,
    /// `None` for a kind no surface can be named for — buffers, and the
    /// acceleration structure a trace resolves through its own registry.
    pub(super) surface_bound_kind: Option<SurfaceBoundKernelBindingKind>,
}

/// What one validated binding resolved to: the slot to write, the kind to write
/// it as, and the surface to look up.
pub(super) struct PlannedSurfaceBoundKernelBinding<'a> {
    pub(super) binding_slot: u32,
    pub(super) kind: SurfaceBoundKernelBindingKind,
    pub(super) name: &'a str,
    pub(super) target_id: &'a str,
}

/// One planned binding carried together with the device texture it names.
///
/// The pair travels as one value rather than as two collections read at a
/// shared index: every step after resolution — the kind-clash check, the
/// pre-run barrier, the colour-target check, the `set_*` calls — needs the plan
/// and the registration together, and a shared index is a desynchronisation
/// waiting to be introduced.
pub(super) struct ResolvedSurfaceBoundKernelBinding<'a> {
    pub(super) planned: PlannedSurfaceBoundKernelBinding<'a>,
    pub(super) registration: TextureRegistration,
}

/// Refuse one binding name supplied twice in a single run's wire array.
///
/// Shared with the trace path, which runs this over the whole array before
/// splitting the acceleration structures out of it — the planner never sees
/// those, and one rule reads as one message wherever it fires. The names it saw
/// come back, so a caller's missing-binding check does not walk the array again.
pub(super) fn refuse_a_kernel_binding_name_supplied_twice<'a>(
    invocation_noun: &str,
    supplied_names: impl IntoIterator<Item = &'a str>,
    declared_names: &[&str],
) -> crate::core::error::Result<std::collections::HashSet<&'a str>> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for name in supplied_names {
        if !seen.insert(name) {
            return Err(crate::core::error::Error::GpuError(format!(
                "binding `{name}` was supplied twice; this kernel declares {}, each supplied \
                 exactly once per {invocation_noun}",
                crate::core::rhi::quote_declared_shader_binding_names(declared_names)
            )));
        }
    }
    Ok(seen)
}

/// Match a draw's or a trace's supplied bindings against the kernel's declared
/// ones.
///
/// The graphics and ray-tracing twin of [`plan_supplied_compute_bindings`](crate::core::compiler::compiler_ops::subprocess_escalate::compute::linux_and_macos::plan_supplied_compute_bindings),
/// with the same rules: every failure raises before any resource is bound and
/// long before a submission, and every message names the kernel's own bindings.
/// Bindings do not persist on a kernel, so one run supplies all of them or
/// none. `invocation_noun` is what one run of this pipeline kind is called, so
/// the refusals read as the caller's op does.
pub(super) fn plan_supplied_surface_bound_kernel_bindings<'a>(
    invocation_noun: &str,
    supplied: &[SuppliedKernelBindingUnderPlanning<'a>],
    declared: &[DeclaredKernelBindingUnderPlanning<'a>],
) -> crate::core::error::Result<Vec<PlannedSurfaceBoundKernelBinding<'a>>> {
    use crate::core::error::Error;

    let declared_names: Vec<&str> = declared.iter().filter_map(|d| d.name).collect();
    // Built only when a refusal fires — this runs per frame, and the happy
    // path should not pay for the error text.
    let kernel_declares = || crate::core::rhi::quote_declared_shader_binding_names(&declared_names);

    let seen = refuse_a_kernel_binding_name_supplied_twice(
        invocation_noun,
        supplied.iter().map(|entry| entry.name),
        &declared_names,
    )?;

    for name in &declared_names {
        if !seen.contains(name) {
            return Err(Error::GpuError(format!(
                "binding `{name}` was not supplied; bindings do not persist between \
                 {invocation_noun}s, so every {invocation_noun} supplies all of {}",
                kernel_declares()
            )));
        }
    }

    let mut planned = Vec::with_capacity(supplied.len());
    for entry in supplied {
        let declaration = declared
            .iter()
            .find(|d| d.name == Some(entry.name))
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "binding `{}` is not one this kernel declares; it declares {}",
                    entry.name,
                    kernel_declares()
                ))
            })?;
        if declaration.kind_wire_name != entry.kind_wire_name {
            return Err(Error::GpuError(format!(
                "binding `{}` was supplied as {} but this kernel declares it {}",
                entry.name, entry.kind_wire_name, declaration.kind_wire_name
            )));
        }
        let kind = declaration.surface_bound_kind.ok_or_else(|| {
            Error::GpuError(format!(
                "binding `{}` is {}, which a {invocation_noun} cannot name a surface for — the \
                 surface-backed kinds are storage_image and sampled_texture",
                entry.name, declaration.kind_wire_name
            ))
        })?;
        planned.push(PlannedSurfaceBoundKernelBinding {
            binding_slot: declaration.binding_slot,
            kind,
            name: entry.name,
            target_id: entry.target_id,
        });
    }
    Ok(planned)
}

/// Resolve every planned binding to the device texture it names, keeping the
/// two together.
///
/// Each registration is a refcount on the texture its descriptor will point at
/// — the caller holds them across the submission, because dropping the last one
/// before the GPU has run frees the image out from under it.
pub(super) fn resolve_planned_surface_bound_kernel_bindings<'a>(
    full: &crate::core::context::GpuContextFullAccess,
    planned: Vec<PlannedSurfaceBoundKernelBinding<'a>>,
) -> crate::core::error::Result<Vec<ResolvedSurfaceBoundKernelBinding<'a>>> {
    use crate::core::error::Error;

    let mut resolved = Vec::with_capacity(planned.len());
    for binding in planned {
        // Zero extent: a kernel binding names a surface the graph already has
        // as a device texture, which resolves from the same-process cache or
        // the surface-share service. The pixel-buffer fallback is the one path
        // that consults the extent, and it refuses a zero one — a
        // buffer-backed surface is not something a draw can bind.
        let registration = full
            .resolve_texture_registration_by_surface_id(binding.target_id, None, 0, 0)
            .map_err(|e| {
                Error::GpuError(format!(
                    "binding `{}` names surface {:?}, which this graph cannot resolve to a \
                     device texture: {e}",
                    binding.name, binding.target_id
                ))
            })?;
        resolved.push(ResolvedSurfaceBoundKernelBinding {
            planned: binding,
            registration,
        });
    }

    // One image cannot serve two kinds in one run. The descriptor layouts are
    // fixed and disagree — a combined image sampler is written
    // SHADER_READ_ONLY_OPTIMAL and a storage image GENERAL — so whatever layout
    // the texture is put in, one of the two descriptors is wrong.
    //
    // Compared after resolution and on the image, not on the id the caller
    // wrote: a published frame id and its pool slot are two spellings of one
    // texture (`<slot>#<generation>` resolves through the same cache entry as
    // `<slot>`), so a string comparison would let the pair through to exactly
    // the run this refuses.
    for (index, binding) in resolved.iter().enumerate() {
        // A texture carrying no image is its own error, raised where the
        // descriptor would be written. Skipped rather than compared, because
        // two absent images are not one texture and refusing them here would
        // send the caller looking for a duplicate they did not write.
        let Some(image) = binding.registration.texture().vulkan_inner().image() else {
            continue;
        };
        let clashing = resolved[..index].iter().find(|prior| {
            prior.planned.kind != binding.planned.kind
                && prior.registration.texture().vulkan_inner().image() == Some(image)
        });
        if let Some(prior) = clashing {
            // Both ids, as the caller wrote them: a published frame id and its
            // pool slot are different strings for one texture, so naming only
            // one would leave the reader looking for a duplicate that is not
            // there on the page.
            return Err(Error::GpuError(format!(
                "bindings `{}` (surface {:?}) and `{}` (surface {:?}) name one texture but as \
                 {:?} and {:?}; no image layout satisfies both descriptors, so this run reads \
                 and writes different surfaces or binds one of them alone",
                prior.planned.name,
                prior.planned.target_id,
                binding.planned.name,
                binding.planned.target_id,
                prior.planned.kind,
                binding.planned.kind
            )));
        }
    }
    Ok(resolved)
}

/// Barrier every bound input into the layout its descriptor requires, and
/// publish the layout each one landed in.
///
/// Neither `VulkanGraphicsKernel::offscreen_render` nor
/// `VulkanRayTracingKernel::trace_rays` barriers a bound input — the draw
/// path transitions its colour targets and nothing else — so a surface
/// arriving in the wrong layout would be read or written through a
/// descriptor its layout does not satisfy, and its registration
/// would keep claiming a layout the run has left behind. A run whose inputs
/// already sit in the right layout records nothing and mints no command
/// buffer.
pub(super) fn transition_bound_kernel_inputs_into_descriptor_layouts(
    full: &crate::core::context::GpuContextFullAccess,
    recorder_label: &str,
    consuming_stage: crate::vulkan::rhi::VulkanStage,
    bound_inputs: &[(&TextureRegistration, crate::core::rhi::VulkanLayout)],
) -> crate::core::error::Result<()> {
    use crate::vulkan::rhi::{VulkanAccess, VulkanStage};

    let mut images_already_barriered = Vec::new();
    let mut bindings_to_barrier = Vec::new();
    for (registration, required_layout) in bound_inputs {
        if registration.current_layout() == *required_layout {
            continue;
        }
        // One texture bound at two slots is one image and one barrier — a
        // second would name an oldLayout the first has already left, and the
        // two slots agree on the layout anyway or the kind clash would have
        // been refused already.
        let image = registration.texture().vulkan_inner().image();
        if images_already_barriered.contains(&image) {
            continue;
        }
        images_already_barriered.push(image);
        bindings_to_barrier.push((registration, *required_layout));
    }
    if bindings_to_barrier.is_empty() {
        return Ok(());
    }

    let mut recorder = full.create_command_recorder(recorder_label)?;
    recorder.begin()?;
    for (registration, required_layout) in &bindings_to_barrier {
        // Whatever wrote this surface before the run is not this run's to know
        // — a transfer upload, a camera, another node — so the source scope is
        // the wide one every other entry-from-an-unknown-producer barrier in
        // the engine uses.
        let recorded = recorder.record_image_barrier(
            registration.texture(),
            registration.current_layout(),
            *required_layout,
            VulkanStage::ALL_COMMANDS,
            consuming_stage,
            VulkanAccess::MEMORY_WRITE,
            VulkanAccess::SHADER_READ | VulkanAccess::SHADER_WRITE,
        );
        if let Err(e) = recorded {
            recorder.abort_recording();
            return Err(e);
        }
    }
    recorder.submit_and_wait()?;
    // Published for every binding, not just the ones that were barriered: a
    // cross-process import synthesizes a fresh registration per resolve, so two
    // slots naming one surface hold two layout cells for the one image.
    for (registration, required_layout) in bound_inputs {
        registration.update_layout(*required_layout);
    }
    Ok(())
}

/// The `(registration, required layout)` pairs the transition fn consumes,
/// derived from planner-resolved bindings.
pub(super) fn descriptor_layout_transition_pairs<'a>(
    bound_inputs: &'a [ResolvedSurfaceBoundKernelBinding<'_>],
) -> Vec<(&'a TextureRegistration, crate::core::rhi::VulkanLayout)> {
    bound_inputs
        .iter()
        .map(|binding| {
            (
                &binding.registration,
                binding.planned.kind.required_image_layout(),
            )
        })
        .collect()
}

/// The `(surface id, registration)` pairs the post-dispatch layout publish
/// consumes, from planner-resolved bindings — paired by construction, never
/// by a shared index.
pub(super) fn bound_surface_layout_publish_pairs(
    bound_inputs: &[ResolvedSurfaceBoundKernelBinding<'_>],
) -> Vec<(String, TextureRegistration)> {
    bound_inputs
        .iter()
        .map(|binding| {
            (
                binding.planned.target_id.to_string(),
                binding.registration.clone(),
            )
        })
        .collect()
}

/// One reflected binding as a register response spells it.
///
/// Every binding a registered kernel holds came through reflection, which
/// refuses an unnamed one — an absent name here is a broken invariant, not a
/// case to skip over.
pub(super) fn reflected_kernel_binding_response(
    kernel_id: &str,
    binding_slot: u32,
    kind_wire_name: &str,
    name: Option<&str>,
) -> crate::core::error::Result<EscalateResponseKernelBinding> {
    Ok(EscalateResponseKernelBinding {
        kind: kind_wire_name.to_string(),
        name: name
            .ok_or_else(|| {
                crate::core::error::Error::GpuError(format!(
                    "kernel {kernel_id} holds an unnamed binding at slot {binding_slot}; \
                     reflection refuses these, so this kernel did not come through registration"
                ))
            })?
            .to_string(),
    })
}
