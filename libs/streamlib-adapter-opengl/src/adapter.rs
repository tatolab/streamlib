// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `OpenGlSurfaceAdapter` — host-allocated `VkImage` consumed as a
//! GL texture.
//!
//! The adapter:
//! - Owns an [`crate::EglRuntime`] (surfaceless EGL display + OpenGL
//!   context + DMA-BUF import function pointers).
//! - Holds a registry of [`SurfaceState`] keyed by
//!   [`streamlib_adapter_abi::SurfaceId`]. Each entry caches the
//!   imported `EGLImage` and the bound `GL_TEXTURE_2D` id — building
//!   them once per surface, not once per acquire.
//! - Enforces the trait's typestate (one writer XOR many readers) at
//!   the registry-mutex level; concurrent GL access through the same
//!   context is serialized by [`EglRuntime::lock_make_current`].

use std::marker::PhantomData;
use std::sync::Arc;

use khronos_egl as egl;
use streamlib_adapter_abi::{
    AdapterError, ReadGuard, Registry, StreamlibSurface, SurfaceAdapter, SurfaceId, WriteGuard,
};
use tracing::{instrument, warn};

use crate::egl::{
    EglRuntime, EglRuntimeError, EGL_DMA_BUF_PLANE0_FD_EXT, EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
    EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT, EGL_DMA_BUF_PLANE0_OFFSET_EXT,
    EGL_DMA_BUF_PLANE0_PITCH_EXT, EGL_HEIGHT, EGL_LINUX_DRM_FOURCC_EXT, EGL_WIDTH,
};
use crate::state::{HostSurfaceRegistration, SurfaceState};
use crate::view::{OpenGlReadView, OpenGlWriteView, GL_TEXTURE_2D, GL_TEXTURE_EXTERNAL_OES};

/// OpenGL/EGL [`SurfaceAdapter`] implementation.
///
/// Construct with [`Self::new`] passing an [`EglRuntime`]. Register
/// host-allocated surfaces with [`Self::register_host_surface`];
/// consumers acquire scoped access through the standard
/// [`SurfaceAdapter::acquire_read`] / [`SurfaceAdapter::acquire_write`]
/// API or via the [`crate::OpenGlContext`] convenience.
pub struct OpenGlSurfaceAdapter {
    runtime: Arc<EglRuntime>,
    surfaces: Registry<SurfaceState>,
}

impl OpenGlSurfaceAdapter {
    /// Construct an empty adapter bound to `runtime`.
    pub fn new(runtime: Arc<EglRuntime>) -> Self {
        Self {
            runtime,
            surfaces: Registry::new(),
        }
    }

    /// Returns the underlying EGL runtime — used by the customer
    /// context, by tests, and by adapter-on-adapter composition.
    pub fn runtime(&self) -> &Arc<EglRuntime> {
        &self.runtime
    }

    /// Register a host-allocated surface with this adapter as a
    /// render-target-capable `GL_TEXTURE_2D`.
    ///
    /// Imports the DMA-BUF as an `EGLImage` with the host-chosen DRM
    /// modifier and binds it to a freshly-generated `GL_TEXTURE_2D`.
    /// The texture id is stable for the lifetime of the registration —
    /// every `acquire_*` returns the same id, so customers can hold
    /// long-lived FBOs / VAOs / shader bindings across acquires.
    ///
    /// Use this for host-allocated surfaces the host meant to be
    /// rendered into (`GpuContext::acquire_render_target_dma_buf_image`,
    /// where the modifier is render-target-capable). For sampler-only
    /// inputs (camera ring textures whose modifier is `external_only=TRUE`
    /// on NVIDIA — see
    /// `docs/learnings/nvidia-egl-dmabuf-render-target.md`) use
    /// [`Self::register_external_oes_host_surface`] instead.
    ///
    /// `id` MUST be unique across the adapter's lifetime; double-
    /// registration returns [`AdapterError::SurfaceAlreadyRegistered`]
    /// and leaves the existing entry untouched.
    #[instrument(level = "debug", skip(self, registration), fields(surface_id = id))]
    pub fn register_host_surface(
        &self,
        id: SurfaceId,
        registration: HostSurfaceRegistration,
    ) -> Result<(), AdapterError> {
        self.register_host_surface_inner(id, registration, GL_TEXTURE_2D)
    }

    /// Register a host-allocated surface for sampler-only consumption
    /// via `GL_TEXTURE_EXTERNAL_OES`.
    ///
    /// Same DMA-BUF import path as [`Self::register_host_surface`],
    /// but binds the resulting `EGLImage` via
    /// `glEGLImageTargetTexture2DOES(GL_TEXTURE_EXTERNAL_OES, image)`.
    /// The customer's GLSL must `#extension GL_OES_EGL_image_external_essl3 :
    /// require` (or the older `_essl1`) and sample via `samplerExternalOES`.
    ///
    /// Use this for surfaces the host did not (or could not) allocate
    /// with a render-target-capable modifier — typically camera ring
    /// textures whose underlying modifier is reported `external_only=TRUE`
    /// by `eglQueryDmaBufModifiersEXT`. The resulting GL texture is
    /// sample-only; FBO color-attachment binding is unsupported by GL.
    #[instrument(level = "debug", skip(self, registration), fields(surface_id = id))]
    pub fn register_external_oes_host_surface(
        &self,
        id: SurfaceId,
        registration: HostSurfaceRegistration,
    ) -> Result<(), AdapterError> {
        self.register_host_surface_inner(id, registration, GL_TEXTURE_EXTERNAL_OES)
    }

    fn register_host_surface_inner(
        &self,
        id: SurfaceId,
        registration: HostSurfaceRegistration,
        target: u32,
    ) -> Result<(), AdapterError> {
        // Reject duplicates up-front via the Registry's atomic insert
        // below; build the EGLImage + GL texture without holding the
        // registry lock so make-current and registry locks don't
        // shoulder each other across user code.
        let _current = self.runtime.lock_make_current().map_err(egl_to_adapter)?;

        // Build the DMA-BUF import attribute list. Modifier is split
        // into LO/HI 32-bit halves per the spec.
        let mod_lo = (registration.drm_format_modifier & 0xFFFF_FFFF) as egl::Attrib;
        let mod_hi = ((registration.drm_format_modifier >> 32) & 0xFFFF_FFFF) as egl::Attrib;
        let attribs: [egl::Attrib; 17] = [
            EGL_WIDTH,
            registration.width as egl::Attrib,
            EGL_HEIGHT,
            registration.height as egl::Attrib,
            EGL_LINUX_DRM_FOURCC_EXT,
            registration.drm_fourcc as egl::Attrib,
            EGL_DMA_BUF_PLANE0_FD_EXT,
            registration.dma_buf_fd as egl::Attrib,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT,
            registration.plane_offset as egl::Attrib,
            EGL_DMA_BUF_PLANE0_PITCH_EXT,
            registration.plane_stride as egl::Attrib,
            EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
            mod_lo,
            EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
            mod_hi,
            egl::NONE as egl::Attrib,
        ];

        let image = self
            .runtime
            .create_dma_buf_image(&attribs)
            .map_err(egl_to_adapter)?;

        let mut texture: u32 = 0;
        unsafe {
            gl::GenTextures(1, &mut texture);
            gl::BindTexture(target, texture);
            gl::TexParameteri(target, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(target, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(target, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(target, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
            // SAFETY: image was produced by create_image above and is
            // non-null. GL texture is bound. This binds the EGLImage's
            // backing storage to texture under the chosen target.
            self.runtime.image_target_texture(target, image);

            // Drain GL errors. For `GL_TEXTURE_2D`, a non-zero error
            // here indicates the EGLImage was external-only
            // (sampler-only) — the host allocator picked the wrong
            // modifier. For `GL_TEXTURE_EXTERNAL_OES`, the EGL spec
            // accepts every importable modifier, so a failure here
            // points at a missing extension or a malformed
            // modifier/fourcc.
            let err = gl::GetError();
            if err != gl::NO_ERROR {
                gl::DeleteTextures(1, &texture);
                self.runtime.destroy_image(image);
                let target_name = match target {
                    GL_TEXTURE_EXTERNAL_OES => "GL_TEXTURE_EXTERNAL_OES",
                    _ => "GL_TEXTURE_2D",
                };
                let hint = if target == GL_TEXTURE_2D {
                    " — likely external_only modifier (host should pick a \
                     render-target-capable tiled modifier, or call \
                     register_external_oes_host_surface for sampler-only \
                     consumption)"
                } else {
                    " — verify GL_OES_EGL_image_external is exposed by the \
                     GL implementation and the DRM modifier / fourcc are \
                     valid"
                };
                return Err(AdapterError::BackendRejected {
                    reason: format!(
                        "glEGLImageTargetTexture2DOES({}) failed: GL error 0x{:x}{}",
                        target_name, err, hint
                    ),
                });
            }

            gl::BindTexture(target, 0);
        }

        let state = SurfaceState {
            surface_id: id,
            image,
            texture,
            target,
            read_holders: 0,
            write_held: false,
        };
        if !self.surfaces.register(id, state) {
            // Lost a race to a concurrent registration with the same
            // id (or caller passed a duplicate). Tear down what we
            // just built so we don't leak the EGLImage / GL texture.
            unsafe {
                gl::DeleteTextures(1, &texture);
                self.runtime.destroy_image(image);
            }
            return Err(AdapterError::SurfaceAlreadyRegistered { surface_id: id });
        }
        Ok(())
    }

    /// Drop a registered surface. Pending guards continue to hold
    /// the GL texture id; the next acquire returns
    /// [`AdapterError::SurfaceNotFound`].
    ///
    /// Returns `true` if a surface was removed.
    #[instrument(level = "debug", skip(self), fields(surface_id = id))]
    pub fn unregister_host_surface(&self, id: SurfaceId) -> bool {
        let Some(state) = self.surfaces.unregister(id) else {
            return false;
        };

        match self.runtime.lock_make_current() {
            Ok(_current) => unsafe {
                gl::DeleteTextures(1, &state.texture);
                self.runtime.destroy_image(state.image);
            },
            Err(e) => {
                // Best-effort cleanup. If we can't make-current we
                // leak the texture id and EGLImage for the lifetime
                // of the runtime — better than panicking.
                warn!(?e, surface_id = id, "could not make-current to clean up unregistered surface");
            }
        }
        true
    }

    /// Snapshot the registry size — primarily for tests and
    /// observability.
    pub fn registered_count(&self) -> usize {
        self.surfaces.len()
    }

    fn try_begin_read(
        &self,
        surface: &StreamlibSurface,
    ) -> Result<Option<(u32, u32)>, AdapterError> {
        self.surfaces
            .try_begin_read(surface.id, |state| Ok((state.texture, state.target)))
    }

    fn try_begin_write(
        &self,
        surface: &StreamlibSurface,
    ) -> Result<Option<u32>, AdapterError> {
        // Write acquires only apply to render-target-capable
        // (`GL_TEXTURE_2D`) surfaces; the external-OES path is
        // sample-only by construction (FBO color-attachment binding
        // unsupported by GL on `GL_TEXTURE_EXTERNAL_OES`).
        self.surfaces.try_begin_write(surface.id, |state| {
            if state.target != GL_TEXTURE_2D {
                return Err(AdapterError::BackendRejected {
                    reason: format!(
                        "acquire_write rejected: surface {} was registered as \
                         GL_TEXTURE_EXTERNAL_OES (sampler-only); use \
                         acquire_read or register the surface via \
                         register_host_surface for write access",
                        surface.id
                    ),
                });
            }
            Ok(state.texture)
        })
    }
}

impl Drop for OpenGlSurfaceAdapter {
    fn drop(&mut self) {
        if self.surfaces.is_empty() {
            return;
        }
        match self.runtime.lock_make_current() {
            Ok(_current) => {
                self.surfaces.drain(|_id, state| unsafe {
                    gl::DeleteTextures(1, &state.texture);
                    self.runtime.destroy_image(state.image);
                });
            }
            Err(e) => {
                warn!(?e, "could not make-current during adapter drop — leaking GL textures");
                // Without a current context we can't safely delete the
                // GL textures or EGLImages. Drop the entries anyway so
                // we don't keep them alive past the adapter; the GL
                // resources leak for the lifetime of the runtime.
                self.surfaces.drain(|_id, _state| {});
            }
        }
    }
}

impl SurfaceAdapter for OpenGlSurfaceAdapter {
    type ReadView<'g> = OpenGlReadView<'g>;
    type WriteView<'g> = OpenGlWriteView<'g>;

    fn acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'g, Self>, AdapterError> {
        match self.try_begin_read(surface)? {
            Some((texture, target)) => Ok(ReadGuard::new(
                self,
                surface.id,
                OpenGlReadView {
                    texture,
                    target,
                    _marker: PhantomData,
                },
            )),
            None => Err(AdapterError::WriteContended {
                surface_id: surface.id,
                holder: "writer".to_string(),
            }),
        }
    }

    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        match self.try_begin_write(surface)? {
            Some(texture) => Ok(WriteGuard::new(
                self,
                surface.id,
                OpenGlWriteView {
                    texture,
                    _marker: PhantomData,
                },
            )),
            None => Err(AdapterError::WriteContended {
                surface_id: surface.id,
                holder: self.surfaces.describe_contention(surface.id),
            }),
        }
    }

    fn try_acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'g, Self>>, AdapterError> {
        match self.try_begin_read(surface)? {
            Some((texture, target)) => Ok(Some(ReadGuard::new(
                self,
                surface.id,
                OpenGlReadView {
                    texture,
                    target,
                    _marker: PhantomData,
                },
            ))),
            None => Ok(None),
        }
    }

    fn try_acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'g, Self>>, AdapterError> {
        match self.try_begin_write(surface)? {
            Some(texture) => Ok(Some(WriteGuard::new(
                self,
                surface.id,
                OpenGlWriteView {
                    texture,
                    _marker: PhantomData,
                },
            ))),
            None => Ok(None),
        }
    }

    fn end_read_access(&self, surface_id: SurfaceId) {
        let updated = self.surfaces.with_mut(surface_id, |state| {
            debug_assert!(state.read_holders > 0, "read release without acquire");
            state.read_holders = state.read_holders.saturating_sub(1);
        });
        if updated.is_none() {
            warn!(?surface_id, "end_read_access on unknown surface — racing unregister");
        }
        // Reads that just sample don't need a flush; if the caller
        // wrote uniforms or did indirect work they're responsible for
        // their own ordering. The adapter does NOT issue glFinish on
        // read release because it would serialize every reader.
    }

    fn end_write_access(&self, surface_id: SurfaceId) {
        let updated = self.surfaces.with_mut(surface_id, |state| {
            debug_assert!(state.write_held, "write release without acquire");
            state.write_held = false;
        });
        if updated.is_none() {
            warn!(?surface_id, "end_write_access on unknown surface — racing unregister");
            return;
        }

        // Drain the GL command stream so subsequent host Vulkan work
        // (or another adapter) sees the writes through the DMA-BUF.
        // glFinish > glFlush here: glFlush only kicks the queue; for
        // cross-API DMA-BUF handoff we need a full GPU drain.
        match self.runtime.lock_make_current() {
            Ok(_current) => unsafe {
                gl::Finish();
            },
            Err(e) => {
                warn!(?e, ?surface_id, "could not make-current on write release — host may see partial writes");
            }
        }
    }
}

fn egl_to_adapter(err: EglRuntimeError) -> AdapterError {
    AdapterError::BackendRejected {
        reason: format!("egl runtime: {err}"),
    }
}
