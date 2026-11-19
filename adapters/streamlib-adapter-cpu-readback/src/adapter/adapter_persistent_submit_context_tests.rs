// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::{Arc, OnceLock, mpsc};
use std::time::Duration;

use streamlib::sdk::context::GpuContext;
use streamlib::sdk::engine::HostGpuDeviceExt;
use streamlib::sdk::engine::host_rhi::HostVulkanDevice;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use super::adapter_persistent_submit_context::AdapterPersistentSubmitContext;

/// Longer than any reset over an idle fence takes, and short of the forever
/// a wait on a fence nothing will signal takes.
const A_WAIT_THAT_RETURNS: Duration = Duration::from_secs(10);

/// Long enough that a reset which skipped its wait has long since returned.
const A_RESET_THAT_WAITS_HAS_NOT_RETURNED: Duration = Duration::from_millis(500);

/// One device for the whole test binary, never torn down: a test that
/// fails by hanging leaves a thread waiting on its fence, and tearing the
/// device down under that thread would hang the binary instead of failing
/// the test.
static SHARED_GPU: OnceLock<Option<GpuContext>> = OnceLock::new();

fn the_host_device() -> Option<Arc<HostVulkanDevice>> {
    let Some(gpu) = SHARED_GPU.get_or_init(|| GpuContext::init_for_platform_sync().ok()) else {
        println!("skipping: no Vulkan device available");
        return None;
    };
    Some(Arc::clone(gpu.device().vulkan_device()))
}

/// Run `steps` on its own thread and answer what it returned, or panic
/// naming `what_hung` when it is still running after `A_WAIT_THAT_RETURNS`.
fn returns_in_time<T: Send + 'static>(
    what_hung: &str,
    steps: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (finished, finished_receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = finished.send(steps());
    });
    finished_receiver
        .recv_timeout(A_WAIT_THAT_RETURNS)
        .unwrap_or_else(|_| panic!("{what_hung}"))
}

#[test]
fn a_submit_that_failed_leaves_the_next_reset_and_teardown_free_to_return() {
    let Some(host_device) = the_host_device() else {
        return;
    };
    let the_next_reset = returns_in_time(
        "the next reset or the teardown waited on a fence nothing will signal",
        move || {
            let device = host_device.device();
            let submit_context =
                AdapterPersistentSubmitContext::new(device, host_device.queue_family_index())
                    .expect("the submit context is created");
            submit_context
                .reset_for_recording(device)
                .expect("the first reset waits on the fence created signaled");
            let refused: Result<(), &str> =
                submit_context.submit_carrying_the_fence(|_fence| Err("the submit failed"));
            assert_eq!(refused, Err("the submit failed"));
            let the_next_reset = submit_context
                .reset_for_recording(device)
                .map_err(|refusal| refusal.to_string());
            submit_context.destroy(device);
            the_next_reset
        },
    );
    the_next_reset.expect("the next reset goes ahead");
}

#[test]
fn a_queued_submit_holds_the_next_reset_until_the_gpu_finishes_it() {
    let Some(host_device) = the_host_device() else {
        return;
    };
    let device = host_device.device().clone();
    let mut timeline_type = vk::SemaphoreTypeCreateInfo::builder()
        .semaphore_type(vk::SemaphoreType::TIMELINE)
        .initial_value(0);
    let semaphore_info = vk::SemaphoreCreateInfo::builder().push_next(&mut timeline_type);
    let gate = unsafe { device.create_semaphore(&semaphore_info, None) }
        .expect("the gate timeline is created");
    let submit_context =
        AdapterPersistentSubmitContext::new(&device, host_device.queue_family_index())
            .expect("the submit context is created");
    submit_context
        .reset_for_recording(&device)
        .expect("the first reset waits on the fence created signaled");
    // A batch with no commands that waits on the gate: the fence signals only
    // once the host opens it.
    let wait_on_the_gate = [vk::SemaphoreSubmitInfo::builder()
        .semaphore(gate)
        .value(1)
        .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .build()];
    let submit = vk::SubmitInfo2::builder()
        .wait_semaphore_infos(&wait_on_the_gate)
        .build();
    submit_context
        .submit_carrying_the_fence(|fence| unsafe {
            device.queue_submit2(host_device.queue(), &[submit], fence)
        })
        .expect("the gated batch queues");

    let (reset_returned, reset_receiver) = mpsc::channel();
    let reset_device = device.clone();
    let reset_thread = std::thread::spawn(move || {
        let reset = submit_context
            .reset_for_recording(&reset_device)
            .map_err(|refusal| refusal.to_string());
        let _ = reset_returned.send(reset);
        submit_context
    });
    assert!(
        reset_receiver
            .recv_timeout(A_RESET_THAT_WAITS_HAS_NOT_RETURNED)
            .is_err(),
        "the reset went ahead while the GPU still held the fence's batch"
    );
    let open_the_gate = vk::SemaphoreSignalInfo::builder().semaphore(gate).value(1);
    unsafe { device.signal_semaphore(&open_the_gate) }.expect("the gate opens");
    reset_receiver
        .recv_timeout(A_WAIT_THAT_RETURNS)
        .expect("the reset returned once the batch finished")
        .expect("the reset goes ahead");
    let submit_context = reset_thread.join().expect("the reset thread finished");
    submit_context.destroy(&device);
    unsafe { device.destroy_semaphore(gate, None) };
}
