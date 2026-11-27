// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A call too slow to run every frame: refused while a frame runs on the calling thread, by its
//! own code and with its fix, and run in the node's body, in a build and anywhere else.

use std::time::Duration;

use streamlib_scope::{CallRefusedInAFrame, CallTooSlowForAFrame, ScopeBeingDeclared, computed};

fn create_a_kernel() -> Result<(), CallRefusedInAFrame> {
    CallTooSlowForAFrame::CreatesAKernel.refused_while_a_frame_runs("create_compute_kernel")
}

#[test]
fn each_call_is_refused_in_a_frame_by_its_own_code_and_shows_it_wrapped_in_a_computed_value() {
    let (_scope, mut view) = ScopeBeingDeclared::new().start().unwrap();
    let calls = [
        (
            CallTooSlowForAFrame::CreatesAKernel,
            "create_compute_kernel",
        ),
        (
            CallTooSlowForAFrame::OpensADevice,
            "open_capture_stream_in_requested_format",
        ),
        (CallTooSlowForAFrame::StartsASession, "open_encode_session"),
    ];

    let refused = view
        .run_frame(|| calls.map(|(does, call)| does.refused_while_a_frame_runs(call)))
        .expect("a scope of no computed value runs its frame");

    assert_eq!(
        refused,
        calls.map(|(does, call)| Err(CallRefusedInAFrame {
            does,
            call: call.to_owned()
        }))
    );
    assert_eq!(
        calls.map(|(does, _)| does.code()),
        [
            "kernel_created_in_a_frame",
            "device_opened_in_a_frame",
            "session_started_in_a_frame"
        ]
    );
    let [Err(kernel_refused), ..] = refused else {
        unreachable!("every call was refused")
    };
    assert_eq!(
        kernel_refused.to_string(),
        "`create_compute_kernel` creates a kernel and can't run every frame; wrap it in a \
         computed value in the node's body: `let kernel = computed(move |ctx| … \
         create_compute_kernel(…));`, then read `kernel.get()` in the per-frame function"
    );
}

#[test]
fn a_call_runs_in_the_body_in_a_build_and_outside_any_frame() {
    assert_eq!(create_a_kernel(), Ok(()));
    let mut declared = ScopeBeingDeclared::new();
    let built = declared.declare(|| {
        assert_eq!(create_a_kernel(), Ok(()));
        computed(|| create_a_kernel().map(|()| true))
    });
    let (scope, mut view) = declared.start().unwrap();

    assert!(scope.wait_until_settled(Duration::from_secs(5)));
    assert_eq!(view.run_frame(|| *built.get()), Some(true));
    assert_eq!(create_a_kernel(), Ok(()));
}
