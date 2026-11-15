// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A batch of compute passes: a later pass reading what an earlier one wrote,
//! and the submissions and stalls a batch costs.

use super::*;

/// The claim the whole op rests on: a later pass reads what an earlier
/// pass wrote, inside one recording.
///
/// The intermediate is written as a storage image and read as a sampled
/// texture, so the batch owes it both a memory dependency and a layout
/// transition — and a barrier taken from the texture's *pre-batch*
/// layout would discard the very writes pass 2 is there for. The two
/// shaders do not commute, so a swapped order fails on the pixels
/// rather than passing quietly.
#[test]
fn a_later_pass_in_a_batch_reads_what_an_earlier_pass_wrote() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("batched chain: no GPU — skipping");
        return;
    };
    let validation_counts_before = sandbox
        .host_inner()
        .device()
        .inner
        .validation_layer_message_counts();
    let brighten = register_slang_kernel(&sandbox, BRIGHTEN_SLANG);
    let double = register_slang_kernel(&sandbox, DOUBLE_SLANG);
    let held = seeded_chain_textures(
        &sandbox,
        ["chain-seed", "chain-brightened", "chain-doubled"],
    );

    let response = handle_run_compute_kernel_batch(
        &sandbox,
        "chain".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![
                batched_dispatch(
                    &brighten,
                    ("unbrightened_image", "chain-seed"),
                    ("brightened_image", "chain-brightened"),
                ),
                batched_dispatch(
                    &double,
                    ("brightened_image", "chain-brightened"),
                    ("doubled_image", "chain-doubled"),
                ),
            ],
            request_id: "chain".to_string(),
        },
    );
    assert!(
        matches!(response, EscalateResponse::Ok(_)),
        "the two-pass chain failed: {response:?}"
    );

    assert_every_pixel_is(
        &read_back_rgba8(
            &sandbox,
            "chain-doubled",
            held[2].texture(),
            "chain-readback",
        ),
        CHAIN_DOUBLED_RGBA,
        "the chain's output — pass 2 must have read pass 1's writes, not the \
                 seed and not an undefined intermediate",
    );
    assert_every_pixel_is(
        &read_back_rgba8(
            &sandbox,
            "chain-brightened",
            held[1].texture(),
            "chain-intermediate-readback",
        ),
        CHAIN_BRIGHTENED_RGBA,
        "the intermediate — pass 1's own output",
    );

    // Each texture's tracked layout is published as the layout its
    // *last* use in the batch left it in, which is where the next
    // batch's barrier starts from. Asserted because the pixels above
    // cannot check it: a source layout of UNDEFINED licenses the driver
    // to discard contents, and this one declines to, so a batch that
    // barriered every pass from the pre-batch layout would still read
    // back correctly here while being wrong by the spec.
    assert_eq!(
        tracked_layout_of_surface(&sandbox, "chain-brightened"),
        streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        "the intermediate was written as a storage image and then read as a sampled \
                 texture, so it ends in the layout its last use required"
    );
    assert_eq!(
        tracked_layout_of_surface(&sandbox, "chain-doubled"),
        streamlib_consumer_rhi::VulkanLayout::GENERAL,
        "the final output was only ever written, so it ends in GENERAL"
    );
    drop(held);
    if let Some(validation_counts_before) = validation_counts_before {
        let validation_counts_after = sandbox
            .host_inner()
            .device()
            .inner
            .validation_layer_message_counts()
            .expect("the messenger stays installed for the whole test");
        assert_eq!(
            validation_counts_after.error_count, validation_counts_before.error_count,
            "the two-pass image chain raised a validation error — under synchronization \
             validation, a missing or mis-scoped image barrier between the passes"
        );
    }
}

/// The reason the op exists, counted rather than timed: N passes
/// batched cost one submission and one stall, where N separate
/// `run_compute_kernel` ops cost one of each per pass.
///
/// Both arms run the same dispatches on the same device in the same
/// test, so the comparison is against the path this op replaces —
/// not against a remembered number.
#[test]
fn a_batch_costs_one_submission_and_one_stall_where_separate_dispatches_cost_n() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("batch submission count: no GPU — skipping");
        return;
    };
    let brighten = register_slang_kernel(&sandbox, BRIGHTEN_SLANG);
    let double = register_slang_kernel(&sandbox, DOUBLE_SLANG);
    let held = seeded_chain_textures(
        &sandbox,
        ["counted-seed", "counted-brightened", "counted-doubled"],
    );

    let dispatches = vec![
        batched_dispatch(
            &brighten,
            ("unbrightened_image", "counted-seed"),
            ("brightened_image", "counted-brightened"),
        ),
        batched_dispatch(
            &double,
            ("brightened_image", "counted-brightened"),
            ("doubled_image", "counted-doubled"),
        ),
    ];

    // Measured on the second run, not the first: a per-frame claim is
    // about the steady state, and the opening batch on a fresh context
    // also builds the recorder and finds its fence already signaled.
    let mut batched_submissions = 0;
    let mut batched_stalls = 0;
    for run in 0..2 {
        let submissions_before = sandbox.host_inner().queue_submission_count();
        let stalls_before = sandbox.host_inner().counted_host_waits_on_gpu_work_count();
        let response = handle_run_compute_kernel_batch(
            &sandbox,
            format!("counted-{run}"),
            EscalateRequestRunComputeKernelBatch {
                dispatches: dispatches.clone(),
                request_id: format!("counted-{run}"),
            },
        );
        assert!(matches!(response, EscalateResponse::Ok(_)), "{response:?}");
        batched_submissions = sandbox.host_inner().queue_submission_count() - submissions_before;
        batched_stalls =
            sandbox.host_inner().counted_host_waits_on_gpu_work_count() - stalls_before;
    }

    assert_eq!(
        batched_submissions, 1,
        "two batched dispatches must go out as one command buffer"
    );
    assert_eq!(
        batched_stalls, 1,
        "and cost the caller exactly one fence wait — a second would mean the \
                 recorder waits again at the next begin() on a fence it already drained"
    );

    let submissions_before = sandbox.host_inner().queue_submission_count();
    let stalls_before = sandbox.host_inner().counted_host_waits_on_gpu_work_count();
    for dispatch in &dispatches {
        let response = handle_run_compute_kernel(
            &sandbox,
            "separate".to_string(),
            EscalateRequestRunComputeKernel {
                bindings: dispatch.bindings.clone(),
                group_count_x: dispatch.group_count_x,
                group_count_y: dispatch.group_count_y,
                group_count_z: dispatch.group_count_z,
                kernel_id: dispatch.kernel_id.clone(),
                push_constants_hex: dispatch.push_constants_hex.clone(),
                request_id: "separate".to_string(),
            },
        );
        assert!(matches!(response, EscalateResponse::Ok(_)), "{response:?}");
    }
    let separate_submissions = sandbox.host_inner().queue_submission_count() - submissions_before;
    let separate_stalls =
        sandbox.host_inner().counted_host_waits_on_gpu_work_count() - stalls_before;

    assert_eq!(
        separate_submissions,
        dispatches.len(),
        "a single dispatch rides the batch machinery as a recording of one, so N \
                 separate ops cost exactly N submissions — and if this is zero the counter \
                 is not counting and the batched assertion above proves nothing"
    );
    assert_eq!(
        separate_stalls,
        dispatches.len(),
        "and exactly N fence waits, one per op — paying this once instead of N \
                 times is the batch's whole advantage: {separate_stalls} vs {batched_stalls}"
    );
    drop(held);
}
