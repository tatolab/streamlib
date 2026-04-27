// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_opengl::tests::round_trip_render_to_surface` —
//! GL renders a known clear color into the WRITE-acquired texture
//! via FBO; host reads back via DMA-BUF; assert pixel equality.
//!
//! This is the load-bearing E2E for "the GL adapter actually wrote
//! through the DMA-BUF and another API can see it." Mocked unit
//! tests don't catch driver bugs in this path; the FBO-attach +
//! glClear + queue_wait_idle + readback chain does.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use streamlib_adapter_abi::SurfaceAdapter;

use common::{host_readback, HostFixture};

#[test]
fn round_trip_render_to_surface() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!("round_trip_render_to_surface: skipping — no Vulkan or no EGL");
            return;
        }
    };
    let width = 64;
    let height = 64;
    let surface = fixture.register_surface(2, width, height);

    // GL render scope — clear the FBO-attached texture to a known
    // RGBA color. The adapter's end_write_access drains GL on drop.
    {
        let guard = fixture
            .adapter
            .acquire_write(&surface.descriptor)
            .expect("acquire_write");
        let texture_id = guard.view().gl_texture_id();

        let _current = fixture
            .egl
            .lock_make_current()
            .expect("lock_make_current");
        unsafe {
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
            assert_eq!(
                gl::CheckFramebufferStatus(gl::FRAMEBUFFER),
                gl::FRAMEBUFFER_COMPLETE,
                "FBO must complete before glClear"
            );
            gl::Viewport(0, 0, width as i32, height as i32);
            // GL ClearColor: R, G, B, A in shader-logical order. The
            // DMA-BUF backing is BGRA8888 in memory, so on readback
            // byte 0 = B, 1 = G, 2 = R, 3 = A.
            gl::ClearColor(0.25, 0.5, 0.75, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
            gl::Finish();
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            gl::DeleteFramebuffers(1, &fbo);
        }
        // Adapter's `end_write_access` drains GL on guard drop.
    }

    // Vulkan readback. RGBA(0.25, 0.5, 0.75) → BGRA bytes
    // [B≈191, G≈128, R≈64, A=255].
    let bytes = host_readback(&fixture.gpu, &surface);
    assert_eq!(
        bytes.len(),
        (width as usize) * (height as usize) * 4,
        "readback buffer size"
    );
    let mismatch = bytes.chunks_exact(4).enumerate().find(|(_, px)| {
        (px[0] as i32 - 191).abs() > 6
            || (px[1] as i32 - 128).abs() > 6
            || (px[2] as i32 - 64).abs() > 6
            || (px[3] as i32 - 255).abs() > 6
    });
    assert!(
        mismatch.is_none(),
        "host saw unexpected pixel after GL clear: {mismatch:?}"
    );
}
