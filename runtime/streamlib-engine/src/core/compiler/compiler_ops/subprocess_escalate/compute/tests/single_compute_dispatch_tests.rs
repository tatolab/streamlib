// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A single compute dispatch: the surfaces it reads and writes, what it costs,
//! the escalate gate a dispatch or batch every frame never enters, and the
//! dispatches it refuses.

use super::*;

/// Each seed channel inverts exactly in unorm8: out = 255 - in.
const SEED_RGBA: [u8; 4] = [10, 20, 30, 255];
const INVERTED_RGBA: [u8; 4] = [245, 235, 225, 255];

/// The whole point of the change: two surfaces, bound by the shader's
/// own names, and the output pixels prove the source was read.
///
/// Not just "the dispatch was accepted": the source is seeded with a
/// known value and the output is read back and compared against the
/// shader's own arithmetic, so binding the two names backwards — the
/// exact failure a by-slot resolution would produce, since both
/// textures share extent, format and usage — fails the assertion
/// rather than passing silently.
///
/// The textures are registered in-process rather than acquired over
/// the escalate op, because an escalate-acquired texture is published
/// to the surface-share service and resolves through it — and this
/// test has no service. The subject here is the dispatch.
#[test]
fn a_dispatch_reads_one_surface_and_writes_another() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_dispatch_reads_one_surface_and_writes_another: no GPU — skipping");
        return;
    };
    let kernel_id = register_read_one_write_another(&sandbox).handle_id;

    // Held for the dispatch: dropping a pooled handle hands its slot
    // back, and the registration would then name a recycled texture.
    let held = sandbox
        .escalate(|full| {
            let desc = TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm).with_usage(
                TextureUsages::TEXTURE_BINDING
                    | TextureUsages::STORAGE_BINDING
                    | TextureUsages::COPY_SRC
                    | TextureUsages::COPY_DST,
            );
            let source = full.acquire_texture(&desc)?;
            let output = full.acquire_texture(&desc)?;
            full.register_texture("conformance-source", source.texture().clone());
            full.register_texture("conformance-output", output.texture().clone());

            // Seed the source with a constant the shader's arithmetic
            // transforms recognizably.
            let (_pool_id, seed_buffer) =
                full.acquire_pixel_buffer(64, 64, crate::core::rhi::PixelFormat::Rgba32)?;
            let plane = seed_buffer.buffer_ref().plane_base_address(0);
            unsafe {
                for pixel in 0..(64 * 64) {
                    std::ptr::copy_nonoverlapping(SEED_RGBA.as_ptr(), plane.add(pixel * 4), 4);
                }
            }
            full.copy_pixel_buffer_to_texture(
                &seed_buffer,
                source.texture(),
                "conformance-source",
                64,
                64,
            )?;
            Ok((source, output))
        })
        .expect("two pooled textures, the source seeded");
    let (source_id, output_id) = ("conformance-source", "conformance-output");

    let response = handle_run_compute_kernel(
        &sandbox,
        "run".to_string(),
        EscalateRequestRunComputeKernel {
            // Supplied in the reverse of declaration order, so a
            // resolution that walked slots instead of names would bind
            // the two backwards and fail the pixel assertion below.
            bindings: vec![
                EscalateRequestRunComputeKernelBinding {
                    kind: EscalateComputeBindingKind::StorageImage,
                    name: "output_image".to_string(),
                    target_id: output_id.to_string(),
                },
                EscalateRequestRunComputeKernelBinding {
                    kind: EscalateComputeBindingKind::SampledTexture,
                    name: "source_image".to_string(),
                    target_id: source_id.to_string(),
                },
            ],
            group_count_x: 8,
            group_count_y: 8,
            group_count_z: 1,
            kernel_id: kernel_id.clone(),
            push_constants_hex: "00000000".to_string(),
            request_id: "run".to_string(),
        },
    );
    match response {
        EscalateResponse::Ok(ok) => assert_eq!(ok.handle_id, kernel_id),
        other => panic!("the read-one-write-another dispatch failed: {other:?}"),
    }

    // The dispatch retired before the response, so the output is
    // readable now — compute leaves a storage image in GENERAL.
    let output_pixels = sandbox
        .escalate(|full| {
            let readback = full.create_texture_readback(
                "conformance-readback",
                64,
                64,
                TextureFormat::Rgba8Unorm,
            )?;
            let ticket = readback.submit(
                held.1.texture(),
                crate::core::rhi::TextureSourceLayout::General,
            )?;
            Ok(readback.wait_and_read(ticket, 2_000_000_000)?.to_vec())
        })
        .expect("the output texture reads back");

    for (pixel_index, pixel) in output_pixels.as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(
            *pixel, INVERTED_RGBA,
            "pixel {pixel_index} must be the inverted seed — the kernel read \
                     `source_image` and wrote `output_image`, by name"
        );
    }
    drop(held);
}

/// A frame's dispatch is per-frame work: it takes neither the process-wide
/// escalate gate nor the whole-device idle wait that comes with it (#2730).
#[test]
fn a_dispatch_every_frame_never_enters_the_escalate_gate() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_dispatch_every_frame_never_enters_the_escalate_gate: no GPU — skipping");
        return;
    };
    let kernel_id = register_read_one_write_another(&sandbox).handle_id;
    let held = seeded_chain_textures(
        &sandbox,
        ["gateless-dispatch-source", "gateless-dispatch-output"],
    );

    run_frames_with_the_escalate_gate_held(&sandbox, "dispatch", |frame_index| {
        handle_run_compute_kernel(
            &sandbox,
            format!("frame-{frame_index}"),
            EscalateRequestRunComputeKernel {
                bindings: vec![
                    EscalateRequestRunComputeKernelBinding {
                        kind: EscalateComputeBindingKind::SampledTexture,
                        name: "source_image".to_string(),
                        target_id: "gateless-dispatch-source".to_string(),
                    },
                    EscalateRequestRunComputeKernelBinding {
                        kind: EscalateComputeBindingKind::StorageImage,
                        name: "output_image".to_string(),
                        target_id: "gateless-dispatch-output".to_string(),
                    },
                ],
                group_count_x: 8,
                group_count_y: 8,
                group_count_z: 1,
                kernel_id: kernel_id.clone(),
                push_constants_hex: "00000000".to_string(),
                request_id: format!("frame-{frame_index}"),
            },
        )
    });
    drop(held);
}

/// A batch is a frame's multi-pass dispatch, so it stays off the escalate gate
/// as a single dispatch does (#2730).
#[test]
fn a_batch_every_frame_never_enters_the_escalate_gate() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_batch_every_frame_never_enters_the_escalate_gate: no GPU — skipping");
        return;
    };
    let brighten = register_slang_kernel(&sandbox, BRIGHTEN_SLANG);
    let double = register_slang_kernel(&sandbox, DOUBLE_SLANG);
    let held = seeded_chain_textures(
        &sandbox,
        [
            "gateless-batch-seed",
            "gateless-batch-brightened",
            "gateless-batch-doubled",
        ],
    );

    run_frames_with_the_escalate_gate_held(&sandbox, "batch", |frame_index| {
        handle_run_compute_kernel_batch(
            &sandbox,
            format!("frame-{frame_index}"),
            EscalateRequestRunComputeKernelBatch {
                dispatches: vec![
                    batched_dispatch(
                        &brighten,
                        ("unbrightened_image", "gateless-batch-seed"),
                        ("brightened_image", "gateless-batch-brightened"),
                    ),
                    batched_dispatch(
                        &double,
                        ("brightened_image", "gateless-batch-brightened"),
                        ("doubled_image", "gateless-batch-doubled"),
                    ),
                ],
                request_id: format!("frame-{frame_index}"),
            },
        )
    });
    assert_every_pixel_is(
        &read_back_rgba8(
            &sandbox,
            "gateless-batch-doubled",
            held[2].texture(),
            "gateless-batch-readback",
        ),
        CHAIN_DOUBLED_RGBA,
        "the last frame's output — each pass still retires before the next reads it",
    );
    drop(held);
}

/// A pixel-buffer surface is a legal id a Python caller can hold, and
/// binding it must refuse by name — not fall into the buffer→texture
/// synthesis path with a zero extent.
///
/// The second half is the guard's real substance: a legitimate
/// resolver's cached canvas for the same slot must survive the refused
/// dispatch. Without the zero-extent guard the refusal still fires
/// (the 0×0 create fails), but only after evicting that canvas — so
/// this test resolves the surface at its real extent before and after,
/// and asserts the same texture comes back.
#[test]
fn binding_a_buffer_backed_surface_is_refused_by_name() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("binding_a_buffer_backed_surface: no GPU — skipping");
        return;
    };
    let kernel_id = register_read_one_write_another(&sandbox).handle_id;

    let (buffer_surface_id, held_buffer) = sandbox
        .escalate(|full| {
            let desc = TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::STORAGE_BINDING);
            let output = full.acquire_texture(&desc)?;
            full.register_texture("buffer-refusal-output", output.texture().clone());
            let (pool_id, buffer) =
                full.acquire_pixel_buffer(64, 64, crate::core::rhi::PixelFormat::Rgba32)?;
            Ok((pool_id.to_string(), (buffer, output)))
        })
        .expect("a pixel buffer and an output texture");

    // A legitimate resolve at the real extent populates the slot's
    // cached canvas — the thing the refused dispatch must not evict.
    // The registration is held alive across the test: were the canvas
    // evicted and recreated, the driver could hand the replacement a
    // recycled handle value, and comparing dead handles would lie.
    let registration_before = sandbox
        .escalate(|full| {
            full.resolve_texture_registration_by_surface_id(&buffer_surface_id, None, 64, 64)
        })
        .expect("the buffer surface resolves at its real extent");
    let canvas_before = registration_before.texture().vulkan_inner().image();

    let response = handle_run_compute_kernel(
        &sandbox,
        "run-buffer".to_string(),
        EscalateRequestRunComputeKernel {
            bindings: vec![
                EscalateRequestRunComputeKernelBinding {
                    kind: EscalateComputeBindingKind::SampledTexture,
                    name: "source_image".to_string(),
                    target_id: buffer_surface_id.clone(),
                },
                EscalateRequestRunComputeKernelBinding {
                    kind: EscalateComputeBindingKind::StorageImage,
                    name: "output_image".to_string(),
                    target_id: "buffer-refusal-output".to_string(),
                },
            ],
            group_count_x: 8,
            group_count_y: 8,
            group_count_z: 1,
            kernel_id,
            push_constants_hex: "00000000".to_string(),
            request_id: "run-buffer".to_string(),
        },
    );
    match response {
        EscalateResponse::Err(err) => assert!(
            err.message.contains("`source_image`")
                && err.message.contains("cannot resolve to a device texture"),
            "must name the binding and refuse it as a non-texture: {}",
            err.message
        ),
        other => panic!("a buffer-backed binding must refuse, got {other:?}"),
    }

    let canvas_after = sandbox
        .escalate(|full| {
            let registration =
                full.resolve_texture_registration_by_surface_id(&buffer_surface_id, None, 64, 64)?;
            Ok(registration.texture().vulkan_inner().image())
        })
        .expect("the buffer surface still resolves after the refused dispatch");
    assert_eq!(
        canvas_before, canvas_after,
        "the refused dispatch must not evict the slot's cached canvas — a fresh \
                 texture here means the zero-extent guard fired after the eviction, not before"
    );
    drop(registration_before);
    drop(held_buffer);
}

#[test]
fn dispatching_an_unregistered_kernel_id_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("dispatching_an_unregistered_kernel_id: no GPU — skipping");
        return;
    };
    let response = handle_run_compute_kernel(
        &sandbox,
        "rid-3".to_string(),
        EscalateRequestRunComputeKernel {
            bindings: Vec::new(),
            group_count_x: 1,
            group_count_y: 1,
            group_count_z: 1,
            kernel_id: "never-registered".to_string(),
            push_constants_hex: String::new(),
            request_id: "rid-3".to_string(),
        },
    );
    match response {
        EscalateResponse::Err(err) => assert!(
            err.message.contains("no kernel registered under id"),
            "got: {}",
            err.message
        ),
        other => panic!("expected Err, got {other:?}"),
    }
}

/// The single op rides the same machinery as a batch of one: barriers
/// and dispatch in one recording, one submission, one fence wait —
/// where the kernel-fence path it replaced paid up to two submissions
/// and three waits — and each binding rests in the layout its
/// descriptor requires, which the pixels cannot check on this driver.
#[test]
fn a_single_dispatch_costs_one_submission_and_one_stall_and_rests_its_layouts() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("single dispatch machinery: no GPU — skipping");
        return;
    };
    let brighten = register_slang_kernel(&sandbox, BRIGHTEN_SLANG);
    let held = seeded_chain_textures(&sandbox, ["single-seed", "single-brightened"]);
    let run_single_dispatch = |request_id: &str| {
        handle_run_compute_kernel(
            &sandbox,
            request_id.to_string(),
            EscalateRequestRunComputeKernel {
                bindings: vec![
                    EscalateRequestRunComputeKernelBinding {
                        kind: EscalateComputeBindingKind::SampledTexture,
                        name: "unbrightened_image".to_string(),
                        target_id: "single-seed".to_string(),
                    },
                    EscalateRequestRunComputeKernelBinding {
                        kind: EscalateComputeBindingKind::StorageImage,
                        name: "brightened_image".to_string(),
                        target_id: "single-brightened".to_string(),
                    },
                ],
                group_count_x: 8,
                group_count_y: 8,
                group_count_z: 1,
                kernel_id: brighten.clone(),
                push_constants_hex: String::new(),
                request_id: request_id.to_string(),
            },
        )
    };

    // Warmed up before measuring: a per-frame claim is about the
    // steady state, and the opening dispatch on a fresh context also
    // builds the shared recorder.
    let warm_up = run_single_dispatch("single-warm-up");
    assert!(matches!(warm_up, EscalateResponse::Ok(_)), "{warm_up:?}");
    let submissions_before = sandbox.host_inner().queue_submission_count();
    let stalls_before = sandbox.host_inner().counted_host_waits_on_gpu_work_count();
    let measured = run_single_dispatch("single-measured");
    assert!(matches!(measured, EscalateResponse::Ok(_)), "{measured:?}");
    let submissions = sandbox.host_inner().queue_submission_count() - submissions_before;
    let stalls = sandbox.host_inner().counted_host_waits_on_gpu_work_count() - stalls_before;
    assert_eq!(
        submissions, 1,
        "the barriers and the dispatch must go out as one command buffer — a \
                 second submission means a separate transition recording is back"
    );
    assert_eq!(
        stalls, 1,
        "and cost the caller exactly one fence wait — more means the kernel's \
                 own fence or a transition recorder's wait is back in the path"
    );

    assert_eq!(
        tracked_layout_of_surface(&sandbox, "single-seed"),
        streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        "the sampled source rests in the layout its descriptor requires"
    );
    assert_eq!(
        tracked_layout_of_surface(&sandbox, "single-brightened"),
        streamlib_consumer_rhi::VulkanLayout::GENERAL,
        "the storage output rests in GENERAL"
    );
    assert_every_pixel_is(
        &read_back_rgba8(
            &sandbox,
            "single-brightened",
            held[1].texture(),
            "single-readback",
        ),
        CHAIN_BRIGHTENED_RGBA,
        "the single dispatch's output — the machinery change must not move pixels",
    );
    drop(held);
}

/// The one single-dispatch refusal that fires inside an open
/// recording: push constants the kernel does not declare are refused
/// by `set_push_constants` after the barriers are recorded, so the
/// abort must leave the shared recorder usable for whatever records
/// next.
#[test]
fn a_single_dispatch_failing_inside_the_recording_leaves_the_recorder_usable() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("single dispatch abort: no GPU — skipping");
        return;
    };
    let brighten = register_slang_kernel(&sandbox, BRIGHTEN_SLANG);
    let held = seeded_chain_textures(&sandbox, ["abort-seed", "abort-brightened"]);
    let run_single_dispatch = |request_id: &str, push_constants_hex: &str| {
        handle_run_compute_kernel(
            &sandbox,
            request_id.to_string(),
            EscalateRequestRunComputeKernel {
                bindings: vec![
                    EscalateRequestRunComputeKernelBinding {
                        kind: EscalateComputeBindingKind::SampledTexture,
                        name: "unbrightened_image".to_string(),
                        target_id: "abort-seed".to_string(),
                    },
                    EscalateRequestRunComputeKernelBinding {
                        kind: EscalateComputeBindingKind::StorageImage,
                        name: "brightened_image".to_string(),
                        target_id: "abort-brightened".to_string(),
                    },
                ],
                group_count_x: 8,
                group_count_y: 8,
                group_count_z: 1,
                kernel_id: brighten.clone(),
                push_constants_hex: push_constants_hex.to_string(),
                request_id: request_id.to_string(),
            },
        )
    };

    let message = refusal_message(run_single_dispatch("abort-refused", "00000000"));
    assert!(
        message.contains("push-constant size mismatch"),
        "the refusal must be the in-recording one, or this proves nothing: {message}"
    );
    let recovered = run_single_dispatch("abort-recovered", "");
    assert!(
        matches!(recovered, EscalateResponse::Ok(_)),
        "the shared recorder must survive a refused single dispatch: {recovered:?}"
    );
    drop(held);
}
