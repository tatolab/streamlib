// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A process that can get no window at all answers the create op with the
//! pump's own error, over the wire — the refusal a Python author wraps in
//! `try/except` when the window is optional, and the reason a refused
//! request raises at `setup()` rather than handing back a window that shows
//! nothing.
//!
//! Its own process, because the pump caches its outcome for the life of a
//! process: once any window has been minted here, no later call can be
//! refused, and once a call has been refused no later one can succeed.
//!
//! Needs a GPU (the escalate capability the op is dispatched against) but
//! deliberately no display server, which it takes away from itself.

#![cfg(target_os = "linux")]

use std::os::unix::net::UnixStream;

use serde_json::json;

use streamlib_engine::core::context::GpuContext;
use streamlib_engine::core::helper_process_transport::SubprocessBridge;

mod helper_process_escalate_socket;
use helper_process_escalate_socket::{
    HelperProcessEndOfTheEscalateSocket, refusal_message_of,
    send_lifecycle_command_and_let_the_helper_read_it,
};

const WINDOW_TITLE_THIS_PROCESS_CANNOT_SERVE: &str = "a window on a process with no display";

#[test]
fn a_process_with_no_display_server_answers_the_create_op_with_the_pumps_own_error() {
    // Before anything reads them, and before the pump — which is what makes
    // this a display-less process. Both backends' variables go: winit picks
    // X11 off `DISPLAY` and Wayland off `WAYLAND_DISPLAY`, and leaving either
    // behind would leave the process able to get a window after all.
    //
    // SAFETY: the test harness is running this test alone on this thread and
    // nothing else in this binary reads the environment concurrently.
    unsafe {
        std::env::remove_var("DISPLAY");
        std::env::remove_var("WAYLAND_DISPLAY");
    }

    let Some(gpu_context) = GpuContext::init_for_platform_sync().ok() else {
        println!(
            "a_process_with_no_display_server_answers_the_create_op_with_the_pumps_own_error: \
             no GPU device — skipping"
        );
        return;
    };
    let gpu_context_limited_access = gpu_context.limited_access();

    let (parent_end, helper_end) =
        UnixStream::pair().expect("a socketpair stands in for the spawned helper's");
    let bridge = SubprocessBridge::new(
        parent_end,
        gpu_context_limited_access,
        "processor-owned-window-headless".to_string(),
    )
    .expect("the bridge wraps the parent end");
    let mut helper = HelperProcessEndOfTheEscalateSocket::new(helper_end);

    send_lifecycle_command_and_let_the_helper_read_it(&bridge, &mut helper, "setup");

    let refused = helper.escalate_request_to_the_parent(json!({
        "op": "create_processor_owned_window",
        "window_title": WINDOW_TITLE_THIS_PROCESS_CANNOT_SERVE,
        "initial_width_in_physical_pixels": 320,
        "initial_height_in_physical_pixels": 240,
    }));

    let refusal = refusal_message_of(&refused, "minting a window with no display server");
    assert!(
        refusal.contains(WINDOW_TITLE_THIS_PROCESS_CANNOT_SERVE),
        "the refusal must name the window that could not be had, got: {refusal}"
    );
    assert!(
        refusal.contains("window event pump") || refusal.contains("event loop"),
        "the refusal must carry the pump's own account of why, not a substitute for it, got: \
         {refusal}"
    );
    assert!(
        !refusal.contains("setup"),
        "a process with no display server is not a phase error — reporting it as one sends the \
         author moving the call rather than handling the refusal, got: {refusal}"
    );
}
