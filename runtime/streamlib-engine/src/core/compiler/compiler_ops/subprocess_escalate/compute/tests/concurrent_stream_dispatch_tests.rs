// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Dispatches from two streams at once: submitted while the other's is still
//! running, each writing its own target.

use super::*;

/// One invocation that keeps the queue busy long enough for a second
/// dispatch to be submitted behind it.
const SPIN_THEN_WRITE_SLANG: &str = "\
[[vk::binding(0, 0)]] [format(\"rgba8\")] RWTexture2D<float4> spun_image;
[numthreads(1, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    float spun = 0.25;
    for (uint step = 0; step < (1u << 26); step++) {
        spun = frac(spun * 1.000173 + 0.381966);
    }
    spun_image[invocation.xy] = float4(spun, spun, spun, 1.0);
}
";

/// A kernel of its own for the second stream: a kernel's one descriptor set
/// is rewritten only once its previous dispatch has retired.
const WRITE_AT_ONCE_SLANG: &str = "\
[[vk::binding(0, 0)]] [format(\"rgba8\")] RWTexture2D<float4> written_image;
[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    written_image[invocation.xy] = float4(0.5, 0.5, 0.5, 1.0);
}
";

fn single_storage_image_dispatch(
    kernel_id: &str,
    binding_name: &str,
    texture_id: &str,
    group_count: u32,
) -> EscalateRequestRunComputeKernel {
    EscalateRequestRunComputeKernel {
        bindings: vec![EscalateRequestRunComputeKernelBinding {
            kind: EscalateComputeBindingKind::StorageImage,
            name: binding_name.to_string(),
            target_id: texture_id.to_string(),
        }],
        group_count_x: group_count,
        group_count_y: group_count,
        group_count_z: 1,
        kernel_id: kernel_id.to_string(),
        push_constants_hex: String::new(),
        request_id: format!("{kernel_id}-dispatch"),
    }
}

/// Two helpers' dispatches, as two streams make them: the second is
/// submitted while the first's work is still running, read off the queue
/// completion timeline the first submission signals. A dispatch holding one
/// shared lock through its wait would submit the second only after the
/// first's timeline value was reached.
///
/// The second stream dispatches once before the first starts, as a stream
/// does every frame after its first: lavapipe creates an image view under
/// the lock its queue holds while work runs, so a first-ever bind of a
/// texture waits for the queue there.
#[test]
fn a_second_streams_dispatch_is_submitted_while_the_firsts_is_still_running() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("overlapping dispatches: no GPU — skipping");
        return;
    };
    let spin_kernel_id = register_slang_kernel(&sandbox, SPIN_THEN_WRITE_SLANG);
    let write_kernel_id = register_slang_kernel(&sandbox, WRITE_AT_ONCE_SLANG);
    let held = seeded_chain_textures(&sandbox, ["overlap-spun", "overlap-written"]);
    let second_streams_dispatch =
        || single_storage_image_dispatch(&write_kernel_id, "written_image", "overlap-written", 8);
    let warm_up = handle_run_compute_kernel(
        &sandbox,
        "second-stream-warm-up".to_string(),
        second_streams_dispatch(),
    );
    assert!(matches!(warm_up, EscalateResponse::Ok(_)), "{warm_up:?}");

    let host = sandbox.host_inner();
    let device = std::sync::Arc::clone(&host.device().inner);
    let wait_for_the_submission_count_to_reach = |submission_count: usize| {
        let gave_up_at = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while host.queue_submission_count() < submission_count {
            assert!(
                std::time::Instant::now() < gave_up_at,
                "a dispatch never reached the queue"
            );
            std::thread::yield_now();
        }
    };
    let first_streams_work_has_completed =
        |first_streams_submission: &crate::vulkan::rhi::HostVulkanQueueSubmissionsSnapshot| {
            device
                .every_queue_submission_in_snapshot_has_completed(first_streams_submission)
                .expect("reading the queue completion timelines")
        };

    let submissions_before = host.queue_submission_count();
    std::thread::scope(|scope| {
        let first_stream = scope.spawn(|| {
            handle_run_compute_kernel(
                &sandbox,
                "first-stream".to_string(),
                single_storage_image_dispatch(&spin_kernel_id, "spun_image", "overlap-spun", 1),
            )
        });
        wait_for_the_submission_count_to_reach(submissions_before + 1);
        let first_streams_submission = device.snapshot_of_queue_submissions_made_so_far();
        assert!(
            !first_streams_work_has_completed(&first_streams_submission),
            "the first stream's work finished before the second could submit behind it; \
             the spin shader no longer holds the queue long enough"
        );

        let second_stream = scope.spawn(|| {
            handle_run_compute_kernel(
                &sandbox,
                "second-stream".to_string(),
                second_streams_dispatch(),
            )
        });
        wait_for_the_submission_count_to_reach(submissions_before + 2);
        assert!(
            !first_streams_work_has_completed(&first_streams_submission),
            "the second stream's dispatch reached the queue only after the first's \
             submission had completed: the two serialised on a wait"
        );

        for (stream, response) in [
            ("second", second_stream.join()),
            ("first", first_stream.join()),
        ] {
            let response = response.expect("a stream's thread");
            assert!(
                matches!(response, EscalateResponse::Ok(_)),
                "the {stream} stream's dispatch: {response:?}"
            );
        }
    });
    drop(held);
}

/// Two streams dispatching one cached kernel take turns: the second
/// rewrites the kernel's descriptor set only after the first's submission
/// has retired, so each dispatch writes its own target.
#[test]
fn two_streams_dispatching_one_kernel_each_write_their_own_target() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("one kernel, two streams: no GPU — skipping");
        return;
    };
    let write_kernel_id = register_slang_kernel(&sandbox, WRITE_AT_ONCE_SLANG);
    let held = seeded_chain_textures(&sandbox, ["shared-kernel-a", "shared-kernel-b"]);
    std::thread::scope(|scope| {
        let streams: Vec<_> = ["shared-kernel-a", "shared-kernel-b"]
            .into_iter()
            .map(|texture_id| {
                let sandbox = &sandbox;
                let write_kernel_id = &write_kernel_id;
                scope.spawn(move || {
                    (0..16)
                        .map(|_| {
                            handle_run_compute_kernel(
                                sandbox,
                                texture_id.to_string(),
                                single_storage_image_dispatch(
                                    write_kernel_id,
                                    "written_image",
                                    texture_id,
                                    8,
                                ),
                            )
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for stream in streams {
            for response in stream.join().expect("a stream's thread") {
                assert!(matches!(response, EscalateResponse::Ok(_)), "{response:?}");
            }
        }
    });
    for (texture_id, texture) in ["shared-kernel-a", "shared-kernel-b"]
        .into_iter()
        .zip(&held)
    {
        let pixels = read_back_rgba8(&sandbox, texture_id, texture.texture(), texture_id);
        assert_every_pixel_is(&pixels, [128, 128, 128, 255], texture_id);
    }
    drop(held);
}
