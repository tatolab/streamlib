// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Every `EglRuntime` and the engine's DRM modifier probe share the
//! process's one `EGL_DEFAULT_DISPLAY`, so no holder may tear it down
//! under another.

#![cfg(target_os = "linux")]

use std::ffi::CStr;

use streamlib::sdk::context::GpuContext;
use streamlib_adapter_opengl::EglRuntime;

fn assert_gl_answers_on_the_current_context(runtime_label: &str) {
    let renderer = unsafe { gl::GetString(gl::RENDERER) };
    assert!(
        !renderer.is_null(),
        "{runtime_label}: glGetString(GL_RENDERER) returned NULL on a current context"
    );
    let renderer = unsafe { CStr::from_ptr(renderer.cast()) };
    assert!(
        !renderer.to_bytes().is_empty(),
        "{runtime_label}: GL_RENDERER is empty"
    );
}

#[test]
fn dropping_one_egl_runtime_leaves_another_able_to_make_current() {
    let first_egl_runtime = match EglRuntime::new() {
        Ok(runtime) => runtime,
        Err(e) => {
            println!("skipping — no EGL on this host: {e}");
            return;
        }
    };
    let second_egl_runtime =
        EglRuntime::new().expect("a second EglRuntime on the same default display");

    drop(first_egl_runtime);

    let _current = second_egl_runtime
        .lock_make_current()
        .expect("the surviving EglRuntime makes its context current");
    assert_gl_answers_on_the_current_context("surviving EglRuntime");
}

#[test]
fn an_egl_runtime_made_before_the_engine_brings_up_its_device_survives_the_bring_up() {
    let egl_runtime = match EglRuntime::new() {
        Ok(runtime) => runtime,
        Err(e) => {
            println!("skipping — no EGL on this host: {e}");
            return;
        }
    };
    let gpu_context = match GpuContext::init_for_platform_sync() {
        Ok(gpu_context) => gpu_context,
        Err(e) => {
            println!("skipping — no Vulkan device on this host: {e}");
            return;
        }
    };

    let _current = egl_runtime
        .lock_make_current()
        .expect("the EglRuntime makes its context current after the device bring-up probed EGL");
    assert_gl_answers_on_the_current_context("EglRuntime made before the device");
    drop(gpu_context);
}
