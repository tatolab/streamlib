// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_opengl::tests::sample_from_surface` — host
//! seeds the surface with a known clear color via Vulkan; subprocess
//! acquires READ on the adapter, samples the texture in a fragment
//! shader, draws to a probe FBO, host reads the probe back, asserts
//! pixel equality.
//!
//! This is the dual of `round_trip_render_to_surface` — that one
//! tests "GL writes through the DMA-BUF and Vulkan sees it"; this
//! one tests "Vulkan writes through the DMA-BUF and GL sees it as a
//! sampler." Both directions need the modifier-aware import to
//! work for the texture to be a real `GL_TEXTURE_2D`.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use streamlib_adapter_abi::SurfaceAdapter;

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

const FRAGMENT_SRC: &str = r#"#version 330 core
in vec2 v_uv;
out vec4 frag_color;
uniform sampler2D u_tex;
void main() {
    frag_color = texture(u_tex, v_uv);
}
"#;

fn compile_program() -> Result<u32, String> {
    unsafe {
        let vs = compile_shader(gl::VERTEX_SHADER, VERTEX_SRC)?;
        let fs = compile_shader(gl::FRAGMENT_SHADER, FRAGMENT_SRC)?;
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
fn sample_from_surface() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!("sample_from_surface: skipping — no Vulkan or no EGL");
            return;
        }
    };
    let width = 64;
    let height = 64;
    let surface = fixture.register_surface(3, width, height);

    // Host seeds the surface — RGBA(0.25, 0.5, 0.75, 1.0) → BGRA
    // bytes [191, 128, 64, 255].
    host_write_clear_color(&fixture.gpu, &surface, [0.25, 0.5, 0.75, 1.0]);

    // GL read scope: bind the surface texture, sample into a probe
    // FBO of the same size, glReadPixels back.
    let probe_pixels: Vec<u8> = {
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

            let prog = compile_program().expect("compile shaders");
            gl::UseProgram(prog);
            let loc =
                gl::GetUniformLocation(prog, b"u_tex\0".as_ptr() as *const _);
            gl::Uniform1i(loc, 0);
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, texture_id);

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

            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            gl::DeleteVertexArrays(1, &vao);
            gl::DeleteFramebuffers(1, &probe_fbo);
            gl::DeleteTextures(1, &probe_tex);
            gl::DeleteProgram(prog);
            probe
        }
    };

    // The host wrote logical RGBA(0.25, 0.5, 0.75, 1.0). With the
    // matched DRM fourcc (ARGB8888 for Vulkan Bgra8Unorm), the GL
    // sampler returns logical R=64/255, G=128/255, B=191/255,
    // A=255/255. The probe FBO stores RGBA8 in memory byte order
    // [R, G, B, A] — so glReadPixels gives bytes
    // [64, 128, 191, 255]. Tolerate ±6 LSB.
    let mismatch = probe_pixels.chunks_exact(4).enumerate().find(|(_, px)| {
        (px[0] as i32 - 64).abs() > 6
            || (px[1] as i32 - 128).abs() > 6
            || (px[2] as i32 - 191).abs() > 6
            || (px[3] as i32 - 255).abs() > 6
    });
    assert!(
        mismatch.is_none(),
        "GL sample saw unexpected pixel: {mismatch:?}"
    );
}
