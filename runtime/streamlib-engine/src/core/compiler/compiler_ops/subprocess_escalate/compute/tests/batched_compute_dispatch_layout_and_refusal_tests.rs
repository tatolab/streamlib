// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The layouts a batch leaves behind, and the batches it refuses or abandons
//! with the recorder still usable.

use super::*;

/// Both slots name one image, at one kind — aliasing the resolver
/// permits, since only differing kinds clash.
const TWO_SLOTS_ONE_IMAGE_SLANG: &str = "\
[[vk::binding(0, 0)]] [format(\"rgba8\")] RWTexture2D<float4> image_slot_a;
[[vk::binding(1, 0)]] [format(\"rgba8\")] RWTexture2D<float4> image_slot_b;
[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    image_slot_b[invocation.xy] = image_slot_a[invocation.xy];
}
";

/// A cross-process resolve synthesizes a fresh registration per call,
/// so two slots naming one image hold two layout cells. The publish
/// walks bindings, not first-touch images: a cell left behind would
/// hand the surface-share service a pre-dispatch layout, and the next
/// import would barrier from a layout the image has already left.
#[test]
fn every_registration_cell_naming_one_image_learns_the_landed_layout() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("per-cell layout publish: no GPU — skipping");
        return;
    };
    let kernel_id = register_slang_kernel(&sandbox, TWO_SLOTS_ONE_IMAGE_SLANG);
    sandbox
        .escalate(|full| {
            let kernel = full
                .compute_kernel_by_id(&kernel_id)
                .expect("the kernel just registered");
            let desc = TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                .with_usage(TextureUsages::STORAGE_BINDING);
            let held = full.acquire_texture(&desc)?;
            let cell_a = TextureRegistration::new(
                held.texture().clone(),
                streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
            );
            let cell_b = TextureRegistration::new(
                held.texture().clone(),
                streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
            );
            let recording = [BatchedComputeKernelDispatch {
                kernel,
                bindings: vec![
                    BatchedComputeKernelDispatchBinding {
                        binding: 0,
                        surface_bound_resource: SurfaceBoundKernelBindingResource::StorageImage(
                            cell_a.clone(),
                        ),
                    },
                    BatchedComputeKernelDispatchBinding {
                        binding: 1,
                        surface_bound_resource: SurfaceBoundKernelBindingResource::StorageImage(
                            cell_b.clone(),
                        ),
                    },
                ],
                push_constants: Vec::new(),
                group_count_x: 8,
                group_count_y: 8,
                group_count_z: 1,
            }];
            full.dispatch_compute_kernel_batch(&recording)?;
            assert_eq!(
                cell_a.current_layout(),
                streamlib_consumer_rhi::VulkanLayout::GENERAL,
                "the cell the first touch barriered from learns the landed layout"
            );
            assert_eq!(
                cell_b.current_layout(),
                streamlib_consumer_rhi::VulkanLayout::GENERAL,
                "and so does the second cell over the same image, which was never \
                         a barrier's source"
            );
            drop(held);
            Ok(())
        })
        .expect("a recording over two cells of one image dispatches");
}

/// A recording binds a kernel through one descriptor set, so the second bind
/// would hand the first recorded dispatch this dispatch's bindings — and nothing has
/// executed yet, so it would do it silently.
#[test]
fn a_batch_naming_one_kernel_twice_is_refused_saying_why() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("batch duplicate kernel: no GPU — skipping");
        return;
    };
    let brighten = register_slang_kernel(&sandbox, BRIGHTEN_SLANG);
    let held = seeded_chain_textures(&sandbox, ["twice-seed", "twice-middle", "twice-out"]);

    let response = handle_run_compute_kernel_batch(
        &sandbox,
        "twice".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![
                batched_dispatch(
                    &brighten,
                    ("unbrightened_image", "twice-seed"),
                    ("brightened_image", "twice-middle"),
                ),
                batched_dispatch(
                    &brighten,
                    ("unbrightened_image", "twice-middle"),
                    ("brightened_image", "twice-out"),
                ),
            ],
            request_id: "twice".to_string(),
        },
    );
    match response {
        EscalateResponse::Err(err) => {
            assert!(
                err.message.contains("descriptor set"),
                "the refusal must say why one kernel cannot appear twice: {}",
                err.message
            );
            assert!(
                err.message.contains("dispatch 1") && err.message.contains("dispatch 0"),
                "and must name both dispatches: {}",
                err.message
            );
        }
        other => panic!("naming one kernel twice must refuse, got {other:?}"),
    }
    drop(held);
}

/// A batch runs whole or not at all, and never strands the recorder.
///
/// The refused batch here fails on its *second* dispatch, so the first
/// one was already planned when the refusal fired. Nothing may reach
/// the GPU — asserted on the first dispatch's output pixels, which stay
/// at what they held — and the recorder must still take the next batch,
/// which is what `begin()` would refuse if the failure had left a
/// recording open.
#[test]
fn a_refused_batch_submits_nothing_and_leaves_the_recorder_usable() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("refused batch: no GPU — skipping");
        return;
    };
    let brighten = register_slang_kernel(&sandbox, BRIGHTEN_SLANG);
    let double = register_slang_kernel(&sandbox, DOUBLE_SLANG);
    let held = seeded_chain_textures(
        &sandbox,
        ["refused-seed", "refused-brightened", "refused-doubled"],
    );

    // The intermediate starts undefined; one batch establishes it, so
    // the assertion below is against known content rather than
    // whatever the allocator handed over.
    let established = handle_run_compute_kernel_batch(
        &sandbox,
        "establish".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![batched_dispatch(
                &brighten,
                ("unbrightened_image", "refused-seed"),
                ("brightened_image", "refused-brightened"),
            )],
            request_id: "establish".to_string(),
        },
    );
    assert!(
        matches!(established, EscalateResponse::Ok(_)),
        "{established:?}"
    );

    let submissions_before = sandbox.host_inner().queue_submission_count();
    let refused = handle_run_compute_kernel_batch(
        &sandbox,
        "refused".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![
                batched_dispatch(
                    &brighten,
                    ("unbrightened_image", "refused-seed"),
                    ("brightened_image", "refused-brightened"),
                ),
                batched_dispatch(
                    &double,
                    ("brightened_image", "no-such-surface"),
                    ("doubled_image", "refused-doubled"),
                ),
            ],
            request_id: "refused".to_string(),
        },
    );
    match refused {
        EscalateResponse::Err(err) => assert!(
            err.message.contains("dispatch 1") && err.message.contains("no-such-surface"),
            "the refusal must name the dispatch and the surface: {}",
            err.message
        ),
        other => panic!("an unresolvable binding must refuse, got {other:?}"),
    }
    assert_eq!(
        sandbox.host_inner().queue_submission_count(),
        submissions_before,
        "a refused batch submits nothing — not even the dispatches ahead of the \
                 one that was refused"
    );
    assert_every_pixel_is(
        &read_back_rgba8(
            &sandbox,
            "refused-brightened",
            held[1].texture(),
            "refused-readback",
        ),
        CHAIN_BRIGHTENED_RGBA,
        // Weak on its own — dispatch 0 would have recomputed this same
        // value — so the claim that nothing ran rests on the
        // submission count above. This checks the refusal did not
        // leave the surface unreadable.
        "the intermediate, still readable after the refused batch",
    );

    // The recorder survived: a fresh batch runs, which begin() would
    // refuse outright if the refusal above had left a recording open.
    let after = handle_run_compute_kernel_batch(
        &sandbox,
        "after".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![batched_dispatch(
                &double,
                ("brightened_image", "refused-brightened"),
                ("doubled_image", "refused-doubled"),
            )],
            request_id: "after".to_string(),
        },
    );
    assert!(
        matches!(after, EscalateResponse::Ok(_)),
        "the recorder must still be usable after a refused batch: {after:?}"
    );
    assert_every_pixel_is(
        &read_back_rgba8(
            &sandbox,
            "refused-doubled",
            held[2].texture(),
            "after-readback",
        ),
        CHAIN_DOUBLED_RGBA,
        "the batch that ran after the refused one",
    );
    drop(held);
}

/// The one failure that lands *inside* an open recording, and the only
/// test that reaches `abort_recording`.
///
/// Every other refusal — an unknown kernel, a binding the shader does
/// not declare, a surface that will not resolve, one kernel twice —
/// fires while resolving, before `begin()`. This one cannot: the
/// push-constant size is the kernel's own business and is only checked
/// when the payload is staged, which happens after the barriers are
/// recorded. So the recording is open when it fails, and a batch that
/// did not abort it would strand the recorder — the next `begin()`
/// refuses outright while a recording is in progress.
#[test]
fn a_batch_failing_inside_the_recording_aborts_it_and_the_recorder_survives() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("mid-recording batch failure: no GPU — skipping");
        return;
    };
    // The conformance kernel declares 4 push-constant bytes; the batch
    // below sends none.
    let kernel_id = register_read_one_write_another(&sandbox).handle_id;
    let brighten = register_slang_kernel(&sandbox, BRIGHTEN_SLANG);
    let held = seeded_chain_textures(&sandbox, ["aborted-seed", "aborted-middle", "aborted-out"]);

    let submissions_before = sandbox.host_inner().queue_submission_count();
    let response = handle_run_compute_kernel_batch(
        &sandbox,
        "aborted".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![EscalateRequestRunComputeKernelBatchDispatch {
                bindings: supplied(&[
                    (
                        "source_image",
                        EscalateComputeBindingKind::SampledTexture,
                        "aborted-seed",
                    ),
                    (
                        "output_image",
                        EscalateComputeBindingKind::StorageImage,
                        "aborted-middle",
                    ),
                ]),
                group_count_x: 8,
                group_count_y: 8,
                group_count_z: 1,
                kernel_id,
                push_constants_hex: String::new(),
            }],
            request_id: "aborted".to_string(),
        },
    );
    match response {
        EscalateResponse::Err(err) => assert!(
            err.message.contains("push-constant size mismatch")
                && err.message.contains("kernel declares 4"),
            "the refusal must name the size the kernel wanted: {}",
            err.message
        ),
        other => panic!("a kernel needing push constants must refuse, got {other:?}"),
    }
    assert_eq!(
        sandbox.host_inner().queue_submission_count(),
        submissions_before,
        "a batch that failed while recording submits nothing"
    );

    // The assertion this test exists for: the recorder took the next
    // batch. Without the abort it is still in `Recording`, and
    // `begin()` refuses a recording already in progress.
    let after = handle_run_compute_kernel_batch(
        &sandbox,
        "after-abort".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![batched_dispatch(
                &brighten,
                ("unbrightened_image", "aborted-seed"),
                ("brightened_image", "aborted-middle"),
            )],
            request_id: "after-abort".to_string(),
        },
    );
    assert!(
        matches!(after, EscalateResponse::Ok(_)),
        "the recorder must be usable after a batch aborted mid-recording: {after:?}"
    );
    assert_every_pixel_is(
        &read_back_rgba8(
            &sandbox,
            "aborted-middle",
            held[1].texture(),
            "after-abort-readback",
        ),
        CHAIN_BRIGHTENED_RGBA,
        "the batch that ran after the aborted one",
    );
    drop(held);
}

/// A dispatch that reads and writes one surface is refused, because the
/// two descriptors want the image in two layouts at once.
///
/// Reachable from Python — `bindings={"src": s, "dst": s}` is an
/// ordinary-looking mapping — and the shader's own names differ, so the
/// duplicate-name rule does not catch it. Left unrefused, the batch
/// records two contradictory barriers and the dispatch runs with one
/// descriptor's layout wrong; the single-dispatch path records no
/// barriers at all and is wrong the same way. Refused for both.
#[test]
fn one_surface_bound_as_two_kinds_in_one_dispatch_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("one surface at two kinds: no GPU — skipping");
        return;
    };
    let kernel_id = register_read_one_write_another(&sandbox).handle_id;
    let held = seeded_chain_textures(&sandbox, ["both-seed", "both-b", "both-c"]);

    let read_and_written = supplied(&[
        (
            "source_image",
            EscalateComputeBindingKind::SampledTexture,
            "both-seed",
        ),
        (
            "output_image",
            EscalateComputeBindingKind::StorageImage,
            "both-seed",
        ),
    ]);

    // Refused on the single-dispatch path...
    let single = handle_run_compute_kernel(
        &sandbox,
        "both-single".to_string(),
        EscalateRequestRunComputeKernel {
            bindings: read_and_written.clone(),
            group_count_x: 8,
            group_count_y: 8,
            group_count_z: 1,
            kernel_id: kernel_id.clone(),
            push_constants_hex: "00000000".to_string(),
            request_id: "both-single".to_string(),
        },
    );
    let message = refusal_message(single);
    assert!(
        message.contains("both-seed")
            && message.contains("`source_image`")
            && message.contains("`output_image`"),
        "the refusal must name the surface and both bindings: {message}"
    );

    // ...and on the batch path, which shares the resolver.
    let submissions_before = sandbox.host_inner().queue_submission_count();
    let batched = handle_run_compute_kernel_batch(
        &sandbox,
        "both-batched".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![EscalateRequestRunComputeKernelBatchDispatch {
                bindings: read_and_written,
                group_count_x: 8,
                group_count_y: 8,
                group_count_z: 1,
                kernel_id: kernel_id.clone(),
                push_constants_hex: "00000000".to_string(),
            }],
            request_id: "both-batched".to_string(),
        },
    );
    assert!(
        refusal_message(batched).contains("both-seed"),
        "the batch shares the resolver, so it refuses the same shape"
    );
    assert_eq!(
        sandbox.host_inner().queue_submission_count(),
        submissions_before,
        "and submits nothing"
    );

    // Two spellings, one texture. A published frame id resolves through
    // its pool slot's cache entry, so `both-seed` and `both-seed#3` are
    // the same image — and comparing the ids as strings would let this
    // pair through to the dispatch the arms above refuse.
    let two_spellings = handle_run_compute_kernel(
        &sandbox,
        "both-spellings".to_string(),
        EscalateRequestRunComputeKernel {
            bindings: supplied(&[
                (
                    "source_image",
                    EscalateComputeBindingKind::SampledTexture,
                    "both-seed",
                ),
                (
                    "output_image",
                    EscalateComputeBindingKind::StorageImage,
                    "both-seed#3",
                ),
            ]),
            group_count_x: 8,
            group_count_y: 8,
            group_count_z: 1,
            kernel_id,
            push_constants_hex: "00000000".to_string(),
            request_id: "both-spellings".to_string(),
        },
    );
    let message = refusal_message(two_spellings);
    assert!(
        message.contains("both-seed\"") && message.contains("both-seed#3"),
        "the refusal must name both spellings, since neither is wrong on its own: \
                 {message}"
    );
    drop(held);
}

/// An empty batch is not an error — there is simply nothing to submit,
/// and a caller who opened a scope and dispatched nothing has not made
/// a mistake worth raising on.
#[test]
fn an_empty_batch_submits_nothing_and_is_not_an_error() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("empty batch: no GPU — skipping");
        return;
    };
    let submissions_before = sandbox.host_inner().queue_submission_count();
    let response = handle_run_compute_kernel_batch(
        &sandbox,
        "empty".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: Vec::new(),
            request_id: "empty".to_string(),
        },
    );
    assert!(matches!(response, EscalateResponse::Ok(_)), "{response:?}");
    assert_eq!(
        sandbox.host_inner().queue_submission_count(),
        submissions_before
    );
}
