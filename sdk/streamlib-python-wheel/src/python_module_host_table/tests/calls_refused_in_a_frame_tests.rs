// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A module's call too slow to run every frame, through the table: refused while a frame of the
//! node's runs on the calling thread, each by its own code and with its fix written in Python,
//! and run in the node's body, in a build and anywhere else.

use std::time::Duration;

use streamlib_scope::{FrameView, Scope, ScopeBeingDeclared, computed};
use tatolab_module_sdk::{
    ModuleHostAudioCaptureStreamRequest, ModuleHostAudioPlaybackStreamRequest,
    ModuleHostCallTooSlowForAFrame, ModuleHostError,
};

use super::audio_and_virtual_camera_stream_tests::module_host_lending_no_port;

/// A scope of no computed value, whose frames run at once.
fn a_running_scope() -> (Scope, FrameView) {
    ScopeBeingDeclared::new().start().unwrap()
}

#[test]
fn a_modules_own_device_open_in_a_frame_is_refused_showing_it_wrapped_in_a_computed_value() {
    let module_host = module_host_lending_no_port("own_device_open_in_a_frame");
    let (_scope, mut view) = a_running_scope();

    let refusal = view
        .run_frame(|| {
            module_host.refuse_while_a_frame_runs(
                ModuleHostCallTooSlowForAFrame::OpensADevice,
                "V4l2ZeroCopyCameraCapture",
            )
        })
        .expect("a scope of no computed value runs its frame")
        .unwrap_err();

    assert_eq!(
        refusal,
        ModuleHostError::CallRefusedInAFrame {
            does: ModuleHostCallTooSlowForAFrame::OpensADevice,
            refusal: "device_opened_in_a_frame: `V4l2ZeroCopyCameraCapture` opens a device and \
                      can't run every frame; wrap it in a computed value in the node's body: \
                      `device = computed(lambda: … V4l2ZeroCopyCameraCapture(…))`, then read \
                      `device.get()` in the per-frame function"
                .to_string()
        }
    );
}

#[test]
fn each_call_a_module_makes_is_refused_in_a_frame_by_what_it_does() {
    let module_host = module_host_lending_no_port("each_call_in_a_frame");
    let (_scope, mut view) = a_running_scope();

    let refusals = view
        .run_frame(|| {
            [
                module_host
                    .refuse_while_a_frame_runs(
                        ModuleHostCallTooSlowForAFrame::CreatesAKernel,
                        "compile",
                    )
                    .unwrap_err(),
                module_host
                    .refuse_while_a_frame_runs(
                        ModuleHostCallTooSlowForAFrame::StartsASession,
                        "connect",
                    )
                    .unwrap_err(),
                module_host
                    .open_audio_capture_stream(&ModuleHostAudioCaptureStreamRequest::default())
                    .err()
                    .unwrap(),
                module_host
                    .open_audio_playback_stream(&ModuleHostAudioPlaybackStreamRequest::default())
                    .err()
                    .unwrap(),
                module_host
                    .open_virtual_camera_stream("tatolab test camera")
                    .err()
                    .unwrap(),
                module_host
                    .lease_shared_native_service("video_codec_host")
                    .err()
                    .unwrap(),
            ]
            .map(|refusal| match refusal {
                ModuleHostError::CallRefusedInAFrame { does, refusal } => {
                    (does, refusal.split(" and can't").next().unwrap().to_owned())
                }
                refusal => panic!("refused as another kind: {refusal}"),
            })
        })
        .unwrap();

    use ModuleHostCallTooSlowForAFrame::{CreatesAKernel, OpensADevice, StartsASession};
    let expected = [
        (
            CreatesAKernel,
            "kernel_created_in_a_frame: `compile` creates a kernel",
        ),
        (
            StartsASession,
            "session_started_in_a_frame: `connect` starts a session",
        ),
        (
            OpensADevice,
            "device_opened_in_a_frame: `open_audio_capture_stream` opens a device",
        ),
        (
            OpensADevice,
            "device_opened_in_a_frame: `open_audio_playback_stream` opens a device",
        ),
        (
            OpensADevice,
            "device_opened_in_a_frame: `open_virtual_camera_stream` opens a device",
        ),
        (
            StartsASession,
            "session_started_in_a_frame: `lease_shared_native_service` starts a session",
        ),
    ]
    .map(|(does, lead)| (does, lead.to_owned()));
    assert_eq!(refusals, expected);
}

#[test]
fn a_modules_own_device_open_runs_in_the_body_in_a_build_and_anywhere_else() {
    let module_host = module_host_lending_no_port("own_device_open_outside_a_frame");
    let opens_a_device = ModuleHostCallTooSlowForAFrame::OpensADevice;
    module_host
        .refuse_while_a_frame_runs(opens_a_device, "V4l2ZeroCopyCameraCapture")
        .unwrap();

    let mut declared = ScopeBeingDeclared::new();
    let module_host_of_the_build = module_host.clone();
    let opened_in_a_build = declared.declare(|| {
        module_host
            .refuse_while_a_frame_runs(opens_a_device, "V4l2ZeroCopyCameraCapture")
            .unwrap();
        computed(move || {
            module_host_of_the_build
                .refuse_while_a_frame_runs(opens_a_device, "V4l2ZeroCopyCameraCapture")
                .map(|()| true)
                .map_err(|refusal| refusal.to_string())
        })
    });
    let (scope, mut view) = declared.start().unwrap();

    assert!(scope.wait_until_settled(Duration::from_secs(5)));
    assert_eq!(view.run_frame(|| *opened_in_a_build.get()), Some(true));
}

#[test]
fn a_modules_own_refusal_reaches_python_as_the_hosts_exception_of_its_kind() {
    let module_host = module_host_lending_no_port("own_refusal_reaches_python");
    let (_scope, mut view) = a_running_scope();
    let refusal = view
        .run_frame(|| {
            module_host.refuse_while_a_frame_runs(
                ModuleHostCallTooSlowForAFrame::OpensADevice,
                "V4l2ZeroCopyCameraCapture",
            )
        })
        .unwrap()
        .unwrap_err();

    pyo3::Python::initialize();
    pyo3::Python::attach(|python| {
        use pyo3::prelude::*;
        let lent_engine = pyo3::types::PyModule::new(python, "_engine").unwrap();
        crate::python_node_scope::add_the_node_scope_to(&lent_engine).unwrap();
        let lend = pyo3::types::PyDict::new(python);
        lend.set_item("lent_engine", &lent_engine).unwrap();
        python
            .run(
                c"import sys, types\n\
                  lent_names = [name for name in ('tatolab', 'tatolab.runtime') \
                  if name not in sys.modules]\n\
                  for name in lent_names:\n    sys.modules[name] = types.ModuleType(name)\n\
                  sys.modules['tatolab.runtime._engine'] = lent_engine\n",
                None,
                Some(&lend),
            )
            .unwrap();

        let raised = pyo3::PyErr::from(refusal);

        python
            .run(
                c"for name in lent_names + ['tatolab.runtime._engine']:\n    \
                  sys.modules.pop(name, None)\n",
                None,
                Some(&lend),
            )
            .unwrap();
        assert!(
            raised
                .get_type(python)
                .is(lent_engine.getattr("DeviceOpenedInAFrame").unwrap()),
            "{raised}"
        );
        assert_eq!(
            raised.value(python).to_string(),
            "`V4l2ZeroCopyCameraCapture` opens a device and can't run every frame; wrap it in a \
             computed value in the node's body: `device = computed(lambda: … \
             V4l2ZeroCopyCameraCapture(…))`, then read `device.get()` in the per-frame function"
        );
    });
}
