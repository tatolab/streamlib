// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_opengl::tests::sample_external_oes` (#615) —
//! covers the `register_external_oes_host_surface` path that imports
//! a host DMA-BUF as a `GL_TEXTURE_EXTERNAL_OES` for sampler-only
//! consumption.
//!
//! Mirrors `sample_from_surface.rs` but the GLSL declares
//! `#extension GL_OES_EGL_image_external : require` and samples via
//! `samplerExternalOES`. The EXTERNAL_OES path is the bg-camera shape
//! AvatarCharacter Linux uses (#615) — it lets a sampler-only DMA-BUF
//! (linear / `external_only=TRUE` modifier on NVIDIA) reach a GLSL
//! shader without a CPU bounce.
//!
//! Three claims tested:
//!   1. Registration succeeds and acquire_read returns a non-zero
//!      texture id with `target == GL_TEXTURE_EXTERNAL_OES`.
//!   2. acquire_write is rejected with `BackendRejected` (the
//!      EXTERNAL_OES binding is sample-only by GL spec).
//!   3. Sampling through `samplerExternalOES` returns the host-written
//!      pixels — proves the EGL DMA-BUF round-trip reaches the GLSL
//!      compiler with the right binding.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use streamlib_adapter_abi::{AdapterError, SurfaceAdapter};
use streamlib_adapter_opengl::GL_TEXTURE_EXTERNAL_OES;

use common::{host_write_clear_color, HostFixture};

const VERTEX_SRC: &str = r#"#version 330 core
out vec2 v_uv;
void main() {
    // Single triangle covering NDC; UVs cover [0,1]^2.
    vec2 positions[3] = vec2[](
        vec2(-1.0, -1.0),
        vec2( 3.0, -1.0),
        vec2(-1.0,  3.0)
    );
    vec2 uvs[3] = vec2[](
        vec2(0.0, 0.0),
        vec2(2.0, 0.0),
        vec2(0.0, 2.0)
    );
    gl_Position = vec4(positions[gl_VertexID], 0.0, 1.0);
    v_uv = uvs[gl_VertexID];
}
"#;

const FRAGMENT_SRC_EXTERNAL_OES: &str = r#"#version 330 core
#extension GL_OES_EGL_image_external : require
in vec2 v_uv;
out vec4 frag_color;
uniform samplerExternalOES u_tex;
void main() {
    frag_color = texture(u_tex, v_uv);
}
"#;

fn compile_program(fs_src: &str) -> Result<u32, String> {
    unsafe {
        let vs = compile_shader(gl::VERTEX_SHADER, VERTEX_SRC)?;
        let fs = compile_shader(gl::FRAGMENT_SHADER, fs_src)?;
        let prog = gl::CreateProgram();
        gl::AttachShader(prog, vs);
        gl::AttachShader(prog, fs);
        gl::LinkProgram(prog);
        let mut ok: i32 = 0;
        gl::GetProgramiv(prog, gl::LINK_STATUS, &mut ok);
        gl::DeleteShader(vs);
        gl::DeleteShader(fs);
        if ok == 0 {
            let mut buf = [0u8; 1024];
            let mut len: i32 = 0;
            gl::GetProgramInfoLog(prog, buf.len() as i32, &mut len, buf.as_mut_ptr() as *mut _);
            let log = String::from_utf8_lossy(&buf[..len.max(0) as usize]).to_string();
            gl::DeleteProgram(prog);
            return Err(format!("link failed: {log}"));
        }
        Ok(prog)
    }
}

unsafe fn compile_shader(kind: u32, src: &str) -> Result<u32, String> {
    let s = unsafe { gl::CreateShader(kind) };
    let c_src = std::ffi::CString::new(src).expect("CString src");
    let ptrs = [c_src.as_ptr()];
    let lens = [c_src.as_bytes().len() as i32];
    unsafe {
        gl::ShaderSource(s, 1, ptrs.as_ptr(), lens.as_ptr());
        gl::CompileShader(s);
    }
    let mut ok: i32 = 0;
    unsafe { gl::GetShaderiv(s, gl::COMPILE_STATUS, &mut ok) };
    if ok == 0 {
        let mut buf = [0u8; 1024];
        let mut len: i32 = 0;
        unsafe {
            gl::GetShaderInfoLog(s, buf.len() as i32, &mut len, buf.as_mut_ptr() as *mut _);
            gl::DeleteShader(s);
        }
        return Err(String::from_utf8_lossy(&buf[..len.max(0) as usize]).to_string());
    }
    Ok(s)
}

#[test]
fn external_oes_view_target_and_write_rejection() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!(
                "external_oes_view_target_and_write_rejection: skipping — \
                 no Vulkan or no EGL"
            );
            return;
        }
    };
    let surface = fixture.register_external_oes_surface(7, 32, 32);

    // 1. acquire_read returns a view with target == GL_TEXTURE_EXTERNAL_OES.
    {
        let guard = fixture
            .adapter
            .acquire_read(&surface.descriptor)
            .expect("acquire_read");
        let view = guard.view();
        assert_ne!(view.gl_texture_id(), 0, "texture id must be non-zero");
        assert_eq!(
            view.target(),
            GL_TEXTURE_EXTERNAL_OES,
            "view.target() must be GL_TEXTURE_EXTERNAL_OES for external-OES \
             surfaces"
        );
    }

    // 2. acquire_write is rejected — the EXTERNAL_OES binding is
    //    sample-only by GL spec; FBO color-attachment binding doesn't
    //    work, so the adapter must refuse.
    match fixture.adapter.acquire_write(&surface.descriptor) {
        Ok(_) => panic!(
            "acquire_write must be rejected for surfaces registered as \
             GL_TEXTURE_EXTERNAL_OES"
        ),
        Err(AdapterError::BackendRejected { reason }) => {
            assert!(
                reason.contains("GL_TEXTURE_EXTERNAL_OES")
                    || reason.contains("EXTERNAL_OES"),
                "BackendRejected reason should mention EXTERNAL_OES, got: {reason}"
            );
        }
        Err(e) => panic!(
            "acquire_write should fail with BackendRejected, got: {e:?}"
        ),
    }
}

#[test]
fn sample_external_oes_round_trip() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!(
                "sample_external_oes_round_trip: skipping — no Vulkan or no EGL"
            );
            return;
        }
    };
    let width = 64;
    let height = 64;
    let surface = fixture.register_external_oes_surface(8, width, height);

    // Host seeds the surface — same color as `sample_from_surface`
    // so the assertion math is symmetrical: RGBA(0.25, 0.5, 0.75, 1.0).
    host_write_clear_color(&fixture.gpu, &surface, [0.25, 0.5, 0.75, 1.0]);

    let probe_pixels: Option<Vec<u8>> = {
        let guard = fixture
            .adapter
            .acquire_read(&surface.descriptor)
            .expect("acquire_read");
        let texture_id = guard.view().gl_texture_id();

        let _current = fixture
            .egl
            .lock_make_current()
            .expect("lock_make_current");
        unsafe {
            let prog = match compile_program(FRAGMENT_SRC_EXTERNAL_OES) {
                Ok(p) => p,
                Err(e) => {
                    println!(
                        "sample_external_oes_round_trip: skipping — driver \
                         rejected GL_OES_EGL_image_external in #version 330 \
                         core fragment shader: {e}"
                    );
                    return;
                }
            };

            // Build a probe RGBA8 texture + FBO of width×height.
            let mut probe_tex: u32 = 0;
            gl::GenTextures(1, &mut probe_tex);
            gl::BindTexture(gl::TEXTURE_2D, probe_tex);
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA8 as i32,
                width as i32,
                height as i32,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                std::ptr::null(),
            );
            gl::TexParameteri(
                gl::TEXTURE_2D,
                gl::TEXTURE_MIN_FILTER,
                gl::NEAREST as i32,
            );
            gl::TexParameteri(
                gl::TEXTURE_2D,
                gl::TEXTURE_MAG_FILTER,
                gl::NEAREST as i32,
            );

            let mut probe_fbo: u32 = 0;
            gl::GenFramebuffers(1, &mut probe_fbo);
            gl::BindFramebuffer(gl::FRAMEBUFFER, probe_fbo);
            gl::FramebufferTexture2D(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::TEXTURE_2D,
                probe_tex,
                0,
            );
            assert_eq!(
                gl::CheckFramebufferStatus(gl::FRAMEBUFFER),
                gl::FRAMEBUFFER_COMPLETE,
                "probe FBO must complete"
            );

            gl::UseProgram(prog);
            let loc =
                gl::GetUniformLocation(prog, b"u_tex\0".as_ptr() as *const _);
            gl::Uniform1i(loc, 0);
            gl::ActiveTexture(gl::TEXTURE0);
            // Bind under the EXTERNAL_OES target — this is the binding
            // the samplerExternalOES uniform reads from.
            gl::BindTexture(GL_TEXTURE_EXTERNAL_OES, texture_id);

            let mut vao: u32 = 0;
            gl::GenVertexArrays(1, &mut vao);
            gl::BindVertexArray(vao);

            gl::Viewport(0, 0, width as i32, height as i32);
            gl::ClearColor(0.0, 0.0, 0.0, 0.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
            gl::DrawArrays(gl::TRIANGLES, 0, 3);
            gl::Finish();

            let mut probe = vec![0u8; (width as usize) * (height as usize) * 4];
            gl::ReadPixels(
                0,
                0,
                width as i32,
                height as i32,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                probe.as_mut_ptr() as *mut _,
            );

            gl::BindTexture(GL_TEXTURE_EXTERNAL_OES, 0);
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            gl::DeleteVertexArrays(1, &vao);
            gl::DeleteFramebuffers(1, &probe_fbo);
            gl::DeleteTextures(1, &probe_tex);
            gl::DeleteProgram(prog);
            Some(probe)
        }
    };

    let probe_pixels = match probe_pixels {
        Some(p) => p,
        None => return, // skipped above
    };

    // Same expected pixels as `sample_from_surface` — host wrote
    // logical RGBA(0.25, 0.5, 0.75, 1.0); GL sampler returns the same
    // logical values regardless of binding target. Probe FBO stores
    // RGBA8 in memory byte order [R, G, B, A], so glReadPixels gives
    // [64, 128, 191, 255]. Tolerate ±6 LSB.
    let mismatch = probe_pixels.chunks_exact(4).enumerate().find(|(_, px)| {
        (px[0] as i32 - 64).abs() > 6
            || (px[1] as i32 - 128).abs() > 6
            || (px[2] as i32 - 191).abs() > 6
            || (px[3] as i32 - 255).abs() > 6
    });
    assert!(
        mismatch.is_none(),
        "GL EXTERNAL_OES sample saw unexpected pixel: {mismatch:?}"
    );
}
