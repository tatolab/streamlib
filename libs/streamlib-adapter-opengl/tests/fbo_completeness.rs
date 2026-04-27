// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_opengl::tests::fbo_completeness_on_nvidia` —
//! direct inverse of the failing probe in
//! `docs/learnings/nvidia-egl-dmabuf-render-target.md`. Acquire WRITE
//! on a surface, attach the resulting GL texture to an FBO, assert
//! `glCheckFramebufferStatus == GL_FRAMEBUFFER_COMPLETE` and
//! `glGetError == GL_NO_ERROR`. A green run on NVIDIA proves the
//! host-allocator-picked-modifier path actually delivers a render-
//! target-capable `GL_TEXTURE_2D`.
//!
//! Equally informative on Mesa (Intel/AMD) — the same FBO-completion
//! check fires regardless of vendor.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use streamlib_adapter_abi::SurfaceAdapter;

use common::HostFixture;

#[test]
fn fbo_completeness_on_nvidia() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!("fbo_completeness: skipping — no Vulkan or no EGL");
            return;
        }
    };
    let surface = fixture.register_surface(1, 64, 64);
    let texture_id = {
        let guard = fixture
            .adapter
            .acquire_write(&surface.descriptor)
            .expect("acquire_write");
        guard.view().gl_texture_id()
    };
    assert_ne!(texture_id, 0, "GL texture id must be non-zero");

    // Re-acquire write to attach to an FBO — the previous guard
    // already dropped, so the surface is free again.
    let _guard = fixture
        .adapter
        .acquire_write(&surface.descriptor)
        .expect("acquire_write");

    let _current = fixture
        .egl
        .lock_make_current()
        .expect("lock_make_current");

    unsafe {
        // Drain any pre-existing error so this test fails on its
        // own GL calls, not on a leftover from another test in the
        // same process.
        loop {
            let e = gl::GetError();
            if e == gl::NO_ERROR {
                break;
            }
        }

        let mut fbo: u32 = 0;
        gl::GenFramebuffers(1, &mut fbo);
        gl::BindFramebuffer(gl::FRAMEBUFFER, fbo);
        gl::FramebufferTexture2D(
            gl::FRAMEBUFFER,
            gl::COLOR_ATTACHMENT0,
            gl::TEXTURE_2D,
            texture_id,
            0,
        );
        let status = gl::CheckFramebufferStatus(gl::FRAMEBUFFER);
        let err = gl::GetError();

        // Cleanup before asserting so any stray FBO doesn't leak
        // into the next test.
        gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
        gl::DeleteFramebuffers(1, &fbo);

        assert_eq!(
            status,
            gl::FRAMEBUFFER_COMPLETE,
            "FBO not complete: 0x{:x} — likely an `external_only` modifier; \
             host-side allocator should pick a render-target-capable tiled modifier \
             (see docs/learnings/nvidia-egl-dmabuf-render-target.md)",
            status
        );
        assert_eq!(
            err,
            gl::NO_ERROR,
            "FBO attach raised GL error 0x{:x}",
            err
        );
    }
}
