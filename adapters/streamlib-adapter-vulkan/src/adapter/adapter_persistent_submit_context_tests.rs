// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::{Arc, mpsc};
use std::time::Duration;

use streamlib::sdk::context::GpuContext;
use streamlib::sdk::engine::HostGpuDeviceExt;

use super::AdapterPersistentSubmitContext;

/// Longer than any reset over an idle fence takes, and short of the forever
/// a wait on a fence nothing will signal takes.
const A_WAIT_THAT_RETURNS: Duration = Duration::from_secs(10);

#[test]
fn a_reset_whose_submit_never_queued_leaves_the_next_reset_and_teardown_free_to_return() {
    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("skipping: no Vulkan device available");
        return;
    };
    let host_device = Arc::clone(gpu.device().vulkan_device());
    let (finished, finished_receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let device = host_device.device();
        let submit_context =
            AdapterPersistentSubmitContext::new(device, host_device.queue_family_index())
                .expect("the submit context is created");
        submit_context
            .reset_for_recording(device)
            .expect("the first reset waits on the fence created signaled");
        // The recording or the submit that follows the reset fails here, so
        // no batch carrying the fence ever queues.
        let the_next_reset = submit_context
            .reset_for_recording(device)
            .map_err(|refusal| refusal.to_string());
        submit_context.destroy(device);
        let _ = finished.send(the_next_reset);
    });

    finished_receiver
        .recv_timeout(A_WAIT_THAT_RETURNS)
        .expect("the next reset or the teardown waited on a fence nothing will signal")
        .expect("the next reset goes ahead");
    drop(gpu);
}
