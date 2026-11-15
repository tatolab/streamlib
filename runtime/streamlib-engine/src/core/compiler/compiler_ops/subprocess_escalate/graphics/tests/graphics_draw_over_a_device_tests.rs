// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Draws over a GPU device: the surfaces a draw reads and the targets it
//! writes, the escalate gate a draw every frame never enters, and the
//! barriers its bindings get.

use super::*;

/// The op end to end, over a real device: a draw resolves its binding
/// by the fragment shader's own name, renders into the surface the
/// request named, and leaves the engine's layout record agreeing with
/// the layout the pass left the image in.
///
/// The source is seeded with a known value and the target is read back
/// and compared against the shader's own arithmetic, so a draw that
/// bound nothing — or bound the target to itself — fails on the pixels
/// rather than passing silently. The seeded source is then moved to
/// `GENERAL`, which a combined image sampler does not satisfy: a draw
/// that did not barrier its bound inputs would read it through a
/// descriptor its layout disagrees with, and would leave the engine's
/// record still saying `GENERAL`.
#[test]
fn a_draw_reads_the_surface_its_binding_names_and_publishes_the_targets_layout() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_draw_reads_the_surface_its_binding_names: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let kernel_id = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("reg-draw", INVERT_SAMPLED_INPUT_FRAGMENT_SLANG),
    )
    .handle_id;

    // Held for the draw: dropping a pooled handle hands its slot back,
    // and the registration would then name a recycled texture.
    let held = sandbox
        .escalate(|full| {
            let source = full.host_inner().acquire_texture(
                &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                    .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST),
            )?;
            let target = full.host_inner().acquire_texture(
                &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                    .with_usage(TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC),
            )?;
            full.register_texture("draw-source", source.texture().clone());
            full.register_texture("draw-target", target.texture().clone());

            let (_pool_id, seed_buffer) = full.acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)?;
            let plane = seed_buffer.buffer_ref().plane_base_address(0);
            unsafe {
                for pixel in 0..(64 * 64) {
                    std::ptr::copy_nonoverlapping(SEED_RGBA.as_ptr(), plane.add(pixel * 4), 4);
                }
            }
            full.copy_pixel_buffer_to_texture(
                &seed_buffer,
                source.texture(),
                "draw-source",
                64,
                64,
            )?;

            // The seed publishes SHADER_READ_ONLY_OPTIMAL — the very
            // layout a sampled binding wants — so the draw's input
            // barrier would have nothing to do and this test would end
            // by re-reading what setup established. GENERAL is a layout
            // the descriptor does not satisfy, which is the state a
            // storage-image producer upstream leaves behind.
            let mut recorder = full.create_command_recorder("draw_source_into_general")?;
            recorder.begin()?;
            recorder.record_image_barrier(
                source.texture(),
                crate::core::rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                crate::core::rhi::VulkanLayout::GENERAL,
                crate::vulkan::rhi::VulkanStage::ALL_COMMANDS,
                crate::vulkan::rhi::VulkanStage::ALL_COMMANDS,
                crate::vulkan::rhi::VulkanAccess::MEMORY_WRITE,
                crate::vulkan::rhi::VulkanAccess::MEMORY_READ,
            )?;
            recorder.submit_and_wait()?;
            full.resolve_texture_registration_by_surface_id("draw-source", None, 64, 64)?
                .update_layout(crate::core::rhi::VulkanLayout::GENERAL);
            Ok((source, target))
        })
        .expect("a seeded source and a colour target");

    let mut run = make_run_req("run-draw", &kernel_id, "draw-target");
    run.bindings = vec![EscalateRequestRunGraphicsDrawBinding {
        kind: EscalateGraphicsBindingKind::SampledTexture,
        name: "source_image".to_string(),
        surface_uuid: "draw-source".to_string(),
    }];
    run.extent_width = 64;
    run.extent_height = 64;
    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
        EscalateRequest::RunGraphicsDraw(run),
    )
    .expect("must produce a response");
    match response {
        EscalateResponse::Ok(ok) => {
            assert_eq!(ok.request_id, "run-draw");
            assert_eq!(
                ok.handle_id, kernel_id,
                "the run response echoes the kernel_id"
            );
            assert!(
                ok.timeline_value.is_none(),
                "run_graphics_draw responses carry no timeline"
            );
        }
        other => panic!("the draw failed: {other:?}"),
    }

    // Asserted before the readback, which transitions the image itself:
    // `offscreen_render` leaves every colour target in
    // COLOR_ATTACHMENT_OPTIMAL and tells no registration, so an
    // unpublished layout would leave the next consumer's barrier
    // naming an oldLayout the image has already left.
    let published = sandbox
        .escalate(|full| {
            Ok(full
                .resolve_texture_registration_by_surface_id("draw-target", None, 64, 64)?
                .current_layout())
        })
        .expect("the colour target still resolves");
    assert_eq!(
        published,
        streamlib_consumer_rhi::VulkanLayout::COLOR_ATTACHMENT_OPTIMAL,
        "the draw must publish the layout it left the colour target in"
    );

    let rendered = sandbox
        .escalate(|full| {
            let readback =
                full.create_texture_readback("draw-readback", 64, 64, TextureFormat::Rgba8Unorm)?;
            let ticket = readback.submit(
                held.1.texture(),
                crate::core::rhi::TextureSourceLayout::ColorAttachment,
            )?;
            Ok(readback.wait_and_read(ticket, 2_000_000_000)?.to_vec())
        })
        .expect("the colour target reads back");
    for (pixel_index, pixel) in rendered.as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(
            *pixel, INVERTED_RGBA,
            "pixel {pixel_index} must be the inverted seed — the draw read \
                     `source_image`, by name, and painted the target it was given"
        );
    }

    // The bound input left GENERAL for the layout its descriptor
    // required, which is where the next consumer's barrier starts from.
    let source_layout = sandbox
        .escalate(|full| {
            Ok(full
                .resolve_texture_registration_by_surface_id("draw-source", None, 64, 64)?
                .current_layout())
        })
        .expect("the source still resolves");
    assert_eq!(
        source_layout,
        streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        "a sampled binding is barriered out of GENERAL into the layout its descriptor \
                 requires"
    );
    drop(held);
}

/// The pixels a draw does not cover are the load op's, and this op
/// carries no clear colour of its own — so the handler's choice of
/// transparent black over `LOAD` is what they read.
///
/// The draw is scissored to the left half of a target seeded with a
/// sentinel no stage writes, so nothing but the load op ever touches the
/// right half. `LOAD` there reads an attachment the pass has just
/// transitioned from `UNDEFINED`, whose contents the spec stops defining
/// at that point — on this device the seeded sentinel survives it, which
/// is what makes the assertion discriminate.
#[test]
fn the_pixels_a_draw_does_not_cover_read_transparent_black() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("the_pixels_a_draw_does_not_cover: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let kernel_id = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("reg-scissored", INVERT_SAMPLED_INPUT_FRAGMENT_SLANG),
    )
    .handle_id;

    let held = sandbox
        .escalate(|full| {
            let source = full.host_inner().acquire_texture(
                &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                    .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST),
            )?;
            let target = full.host_inner().acquire_texture(
                &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm).with_usage(
                    TextureUsages::RENDER_ATTACHMENT
                        | TextureUsages::COPY_SRC
                        | TextureUsages::COPY_DST,
                ),
            )?;
            full.register_texture("scissored-source", source.texture().clone());
            full.register_texture("scissored-target", target.texture().clone());

            for (texture, surface_id, seed) in [
                (source.texture(), "scissored-source", SEED_RGBA),
                (
                    target.texture(),
                    "scissored-target",
                    UNCOVERED_SENTINEL_RGBA,
                ),
            ] {
                let (_pool_id, seed_buffer) =
                    full.acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)?;
                let plane = seed_buffer.buffer_ref().plane_base_address(0);
                unsafe {
                    for pixel in 0..(64 * 64) {
                        std::ptr::copy_nonoverlapping(seed.as_ptr(), plane.add(pixel * 4), 4);
                    }
                }
                full.copy_pixel_buffer_to_texture(&seed_buffer, texture, surface_id, 64, 64)?;
            }
            Ok((source, target))
        })
        .expect("a seeded source and a seeded colour target");

    let mut run = make_run_req("run-scissored", &kernel_id, "scissored-target");
    run.bindings = vec![EscalateRequestRunGraphicsDrawBinding {
        kind: EscalateGraphicsBindingKind::SampledTexture,
        name: "source_image".to_string(),
        surface_uuid: "scissored-source".to_string(),
    }];
    run.extent_width = 64;
    run.extent_height = 64;
    run.scissor = Some(EscalateRequestRunGraphicsDrawScissor {
        x: 0,
        y: 0,
        width: 32,
        height: 64,
    });
    match handle_escalate_op(
        &sandbox,
        &registry,
        &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
        EscalateRequest::RunGraphicsDraw(run),
    )
    .expect("must produce a response")
    {
        EscalateResponse::Ok(_) => {}
        other => panic!("the scissored draw failed: {other:?}"),
    }

    let rendered = sandbox
        .escalate(|full| {
            let readback = full.create_texture_readback(
                "scissored-readback",
                64,
                64,
                TextureFormat::Rgba8Unorm,
            )?;
            let ticket = readback.submit(
                held.1.texture(),
                crate::core::rhi::TextureSourceLayout::ColorAttachment,
            )?;
            Ok(readback.wait_and_read(ticket, 2_000_000_000)?.to_vec())
        })
        .expect("the colour target reads back");
    for (pixel_index, pixel) in rendered.as_chunks::<4>().0.iter().enumerate() {
        if pixel_index % 64 < 32 {
            assert_eq!(
                *pixel, INVERTED_RGBA,
                "pixel {pixel_index} is inside the scissor and must be the inverted seed"
            );
        } else {
            assert_eq!(
                *pixel, TRANSPARENT_BLACK_RGBA,
                "pixel {pixel_index} is outside the scissor, so nothing painted it — the \
                         pass must have cleared it rather than loaded contents its own transition \
                         from UNDEFINED had already discarded"
            );
        }
    }
    drop(held);
}

/// A draw whose binding and colour target are one texture is refused:
/// the pass discards a colour target's contents on entry, so the
/// binding would read pixels the draw has already thrown away.
#[test]
fn a_draw_binding_its_own_colour_target_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_draw_binding_its_own_colour_target: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let kernel_id = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("reg-alias", INVERT_SAMPLED_INPUT_FRAGMENT_SLANG),
    )
    .handle_id;
    let held = sandbox
        .escalate(|full| {
            let texture = full.host_inner().acquire_texture(
                &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                    .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::RENDER_ATTACHMENT),
            )?;
            full.register_texture("alias-surface", texture.texture().clone());
            Ok(texture)
        })
        .expect("one texture to name twice");

    let mut run = make_run_req("run-alias", &kernel_id, "alias-surface");
    run.bindings = vec![EscalateRequestRunGraphicsDrawBinding {
        kind: EscalateGraphicsBindingKind::SampledTexture,
        name: "source_image".to_string(),
        surface_uuid: "alias-surface".to_string(),
    }];
    run.extent_width = 64;
    run.extent_height = 64;
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RunGraphicsDraw(run),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("already thrown away"),
        "must say why the alias is refused, got: {message}"
    );
    drop(held);
}

/// A 64×64 sampled source and colour target registered under the given ids.
/// Returned held: dropping a pooled handle hands its slot back, and the
/// registration would then name a recycled texture.
fn registered_draw_source_and_target(
    sandbox: &GpuContextLimitedAccess,
    source_surface_id: &str,
    target_surface_id: &str,
) -> (
    crate::core::context::PooledTextureHandle,
    crate::core::context::PooledTextureHandle,
) {
    sandbox
        .escalate(|full| {
            let source = full.host_inner().acquire_texture(
                &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                    .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST),
            )?;
            let target = full.host_inner().acquire_texture(
                &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                    .with_usage(TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC),
            )?;
            full.register_texture(source_surface_id, source.texture().clone());
            full.register_texture(target_surface_id, target.texture().clone());
            Ok((source, target))
        })
        .expect("a source and a colour target")
}

/// One 64×64 draw of the inverting kernel from `source_surface_id` into
/// `target_surface_id`.
fn inverting_draw_req(
    request_id: &str,
    kernel_id: &str,
    source_surface_id: &str,
    target_surface_id: &str,
) -> EscalateRequestRunGraphicsDraw {
    let mut run = make_run_req(request_id, kernel_id, target_surface_id);
    run.bindings = vec![EscalateRequestRunGraphicsDrawBinding {
        kind: EscalateGraphicsBindingKind::SampledTexture,
        name: "source_image".to_string(),
        surface_uuid: source_surface_id.to_string(),
    }];
    run.extent_width = 64;
    run.extent_height = 64;
    run
}

/// A frame's draw is per-frame work: it takes neither the process-wide escalate
/// gate nor the whole-device idle wait that comes with it (#2730).
#[test]
fn a_draw_every_frame_never_enters_the_escalate_gate() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_draw_every_frame_never_enters_the_escalate_gate: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let kernel_id = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("reg-gateless-draw", INVERT_SAMPLED_INPUT_FRAGMENT_SLANG),
    )
    .handle_id;
    let held =
        registered_draw_source_and_target(&sandbox, "gateless-draw-source", "gateless-draw-target");

    super::super::super::run_frames_with_the_escalate_gate_held(&sandbox, "draw", |frame_index| {
        let run = inverting_draw_req(
            &format!("frame-{frame_index}"),
            &kernel_id,
            "gateless-draw-source",
            "gateless-draw-target",
        );
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RunGraphicsDraw(run),
        )
        .expect("must produce a response")
    });
    drop(held);
}

/// With the escalate gate gone from a draw, the held kernel is what keeps a
/// second draw off a cached kernel whose bindings another holder is staging:
/// the draw waits until that holder lets go, then runs.
#[test]
fn a_draw_waits_while_another_holder_stages_bindings_on_its_kernel() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_draw_waits_while_another_holder_stages_bindings: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let kernel_id = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("reg-held-draw", INVERT_SAMPLED_INPUT_FRAGMENT_SLANG),
    )
    .handle_id;
    let held_textures =
        registered_draw_source_and_target(&sandbox, "held-draw-source", "held-draw-target");

    let held_kernel = sandbox
        .host_inner()
        .graphics_kernel_by_id_held_for_one_draw(&kernel_id)
        .expect("the registered kernel is cached");
    super::super::super::assert_a_per_frame_op_waits_for_its_held_kernel_then_succeeds(
        "draw",
        held_kernel,
        || {
            let run = inverting_draw_req(
                "held-draw",
                &kernel_id,
                "held-draw-source",
                "held-draw-target",
            );
            handle_escalate_op(
                &sandbox,
                &registry,
                &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
                EscalateRequest::RunGraphicsDraw(run),
            )
            .expect("must produce a response")
        },
    );
    drop(held_textures);
}

/// A storage image is written by the run, so it is barriered even when it
/// already sits in the layout its descriptor requires — nothing else orders the
/// write after the image's earlier readers. A sampled texture already in its
/// layout is only read, and needs none.
#[test]
fn a_storage_image_already_in_its_layout_is_still_barriered_and_a_settled_sampled_texture_is_not() {
    use super::super::super::surface_bound_kernel_binding::{
        ResolvedSurfaceBoundKernelBinding, bound_textures_a_run_must_barrier,
    };
    use crate::core::context::SurfaceBoundKernelBindingResource;
    use crate::core::rhi::VulkanLayout;

    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_storage_image_already_in_its_layout_is_still_barriered: no GPU — skipping");
        return;
    };
    let (storage_image, sampled_texture, held) = sandbox
        .escalate(|full| {
            let descriptor = TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::STORAGE_BINDING);
            let storage = full.host_inner().acquire_texture(&descriptor)?;
            let sampled = full.host_inner().acquire_texture(&descriptor)?;
            full.register_texture_with_layout(
                "settled-storage-image",
                storage.texture().clone(),
                VulkanLayout::GENERAL,
            );
            full.register_texture_with_layout(
                "settled-sampled-texture",
                sampled.texture().clone(),
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            );
            Ok((
                full.resolve_texture_registration_by_surface_id(
                    "settled-storage-image",
                    None,
                    0,
                    0,
                )?,
                full.resolve_texture_registration_by_surface_id(
                    "settled-sampled-texture",
                    None,
                    0,
                    0,
                )?,
                (storage, sampled),
            ))
        })
        .expect("two textures registered in the layouts their descriptors require");

    let bound_inputs = [
        ResolvedSurfaceBoundKernelBinding {
            binding_slot: 0,
            name: "output_image",
            target_id: "settled-storage-image",
            surface_bound_resource: SurfaceBoundKernelBindingResource::StorageImage(storage_image),
        },
        ResolvedSurfaceBoundKernelBinding {
            binding_slot: 1,
            name: "source_image",
            target_id: "settled-sampled-texture",
            surface_bound_resource: SurfaceBoundKernelBindingResource::SampledTexture(
                sampled_texture,
            ),
        },
    ];
    let barriered_layouts: Vec<VulkanLayout> = bound_textures_a_run_must_barrier(&bound_inputs)
        .into_iter()
        .map(|(_, required_layout)| required_layout)
        .collect();
    assert_eq!(
        barriered_layouts,
        vec![VulkanLayout::GENERAL],
        "only the storage image is barriered"
    );
    drop(held);
}
