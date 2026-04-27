// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Surfaceless EGL display + OpenGL context plus the function
//! pointers the adapter needs to bind imported `EGLImage`s as
//! `GL_TEXTURE_2D`s.
//!
//! Owned by [`EglRuntime`], constructed once per process; the adapter
//! and customer-facing context hold an `Arc<EglRuntime>`. The runtime
//! serializes EGL/GL access via an internal `Mutex` because OpenGL
//! contexts are thread-bound — every `acquire_*` re-makes-current on
//! the calling thread before issuing GL commands and clears it on the
//! way out so a different thread can take over on the next call.

use std::ffi::{c_void, CStr, CString};
use std::mem::ManuallyDrop;
use std::os::raw::c_char;
use std::sync::Arc;

use khronos_egl as egl;
use parking_lot::{Mutex, MutexGuard};
use thiserror::Error;
use tracing::{debug, instrument};

/// Construction failures for [`EglRuntime`].
#[derive(Debug, Error)]
pub enum EglRuntimeError {
    /// The dynamic `libEGL.so.1` could not be loaded — typical on
    /// minimal CI containers without an X server / Mesa runtime.
    #[error("failed to load libEGL.so.1: {0}")]
    EglLoad(String),
    /// `eglInitialize` failed for the resolved display.
    #[error("EGL initialize failed: {0}")]
    Initialize(String),
    /// `eglBindAPI(EGL_OPENGL_API)` failed; the EGL implementation
    /// almost certainly only supports GLES on this device.
    #[error("EGL bind_api(OPENGL) failed: {0}")]
    BindApi(String),
    /// No matching EGL config (renderable + pbuffer-capable) found.
    #[error("no compatible EGL config (need OPENGL_BIT renderable type)")]
    NoConfig,
    /// `eglCreateContext` failed.
    #[error("EGL create_context failed: {0}")]
    CreateContext(String),
    /// `eglMakeCurrent` failed.
    #[error("EGL make_current failed: {0}")]
    MakeCurrent(String),
    /// A required EGL extension is missing on this device.
    #[error("missing required EGL extension: {0}")]
    MissingExtension(&'static str),
    /// A required GL extension is missing on this device.
    #[error("missing required GL extension: {0}")]
    MissingGlExtension(&'static str),
    /// `eglGetProcAddress` returned NULL for a function the adapter
    /// must call. This means the extension string lied about support.
    #[error("EGL/GL function pointer missing: {0}")]
    MissingProcAddr(&'static str),
    /// `eglCreateImage(EGL_LINUX_DMA_BUF_EXT, …)` failed.
    #[error("EGL create_image failed: {0}")]
    CreateImage(String),
}

/// `glEGLImageTargetTexture2DOES` — binds an `EGLImage` to the
/// currently-bound `GL_TEXTURE_2D` of the active context. This is the
/// load-bearing call: per the NVIDIA EGL DMA-BUF render-target
/// learning, the texture is render-target-capable iff the underlying
/// modifier was reported `external_only=FALSE`.
type PfnGlEGLImageTargetTexture2DOES =
    unsafe extern "system" fn(target: u32, image: *mut c_void);

// EGL DMA-BUF import attribute names that aren't in khronos-egl's
// re-exports. Values are stable across vendors per
// `EGL_EXT_image_dma_buf_import` and `..._modifiers`.
pub(crate) const EGL_LINUX_DMA_BUF_EXT: egl::Enum = 0x3270;
pub(crate) const EGL_LINUX_DRM_FOURCC_EXT: egl::Attrib = 0x3271;
pub(crate) const EGL_DMA_BUF_PLANE0_FD_EXT: egl::Attrib = 0x3272;
pub(crate) const EGL_DMA_BUF_PLANE0_OFFSET_EXT: egl::Attrib = 0x3273;
pub(crate) const EGL_DMA_BUF_PLANE0_PITCH_EXT: egl::Attrib = 0x3274;
pub(crate) const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: egl::Attrib = 0x3443;
pub(crate) const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: egl::Attrib = 0x3444;
pub(crate) const EGL_WIDTH: egl::Attrib = 0x3057;
pub(crate) const EGL_HEIGHT: egl::Attrib = 0x3056;

/// `DRM_FORMAT_*` four-character codes for the surface formats the
/// adapter currently supports. NV12 and other planar formats are
/// deferred — see issue #512's AI notes.
///
/// Exposed so adapter tests and the in-tree helper binary can build
/// the host-side surface descriptor without hard-coding the magic
/// number.
pub const DRM_FORMAT_ARGB8888: u32 = u32::from_le_bytes(*b"AR24");
pub const DRM_FORMAT_ABGR8888: u32 = u32::from_le_bytes(*b"AB24");

/// Surfaceless EGL + OpenGL runtime owned by the adapter.
pub struct EglRuntime {
    egl: egl::DynamicInstance<egl::EGL1_5>,
    display: egl::Display,
    context: egl::Context,
    image_target_texture_2d_oes: PfnGlEGLImageTargetTexture2DOES,
    /// Serializes EGL `make_current` across threads — the EGL spec
    /// allows a context to be current on at most one thread at a
    /// time.
    make_current_lock: Mutex<()>,
}

// SAFETY: the EGL display and context are raw pointers, but every
// dereference goes through the `make_current_lock` mutex (callers
// must hold a [`MakeCurrentGuard`] before touching the GL state).
// The pointers themselves are stable for the runtime's lifetime;
// concurrent use across threads is the entire reason `make_current`
// exists in the EGL spec.
unsafe impl Send for EglRuntime {}
unsafe impl Sync for EglRuntime {}

impl EglRuntime {
    /// Construct a surfaceless EGL+OpenGL runtime.
    ///
    /// Uses `eglGetDisplay(EGL_DEFAULT_DISPLAY)` — matches PyOpenGL's
    /// behavior under `PYOPENGL_PLATFORM=egl`.
    #[instrument(level = "debug", skip_all)]
    pub fn new() -> Result<Arc<Self>, EglRuntimeError> {
        // Load libEGL.so.1 dynamically. Static linkage would tie the
        // binary to one EGL implementation; the dynamic loader lets
        // LD_LIBRARY_PATH override and matches PyOpenGL behavior in
        // subprocess Python contexts.
        let egl = unsafe {
            egl::DynamicInstance::<egl::EGL1_5>::load_required()
                .map_err(|e| EglRuntimeError::EglLoad(format!("{e}")))?
        };

        let display = unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }.ok_or_else(|| {
            EglRuntimeError::Initialize("get_display(DEFAULT) returned NULL".into())
        })?;
        egl.initialize(display)
            .map_err(|e| EglRuntimeError::Initialize(format!("{e}")))?;

        // Probe the EGL extension list — both the DMA-BUF base and
        // the modifier extension must be present.
        let extensions = egl
            .query_string(Some(display), egl::EXTENSIONS)
            .map_err(|e| EglRuntimeError::Initialize(format!("query EGL extensions: {e}")))?;
        let ext_str = extensions.to_str().unwrap_or("");
        debug!(egl_extensions = ext_str, "EGL extensions");
        const REQUIRED_EGL_EXTENSIONS: &[&str] = &[
            "EGL_EXT_image_dma_buf_import",
            "EGL_EXT_image_dma_buf_import_modifiers",
            "EGL_KHR_image_base",
        ];
        for required in REQUIRED_EGL_EXTENSIONS {
            if !ext_has(ext_str, required) {
                return Err(EglRuntimeError::MissingExtension(required));
            }
        }

        // Bind to OpenGL — NOT GLES. Skia-on-GL composes through the
        // same texture id and expects desktop GL semantics
        // (samplerExternalOES / glEGLImageTargetTexture2DOES are
        // shared between GL and GLES).
        egl.bind_api(egl::OPENGL_API)
            .map_err(|e| EglRuntimeError::BindApi(format!("{e}")))?;

        let config = pick_config(&egl, display)?;
        let context = egl
            .create_context(
                display,
                config,
                None,
                &[egl::CONTEXT_CLIENT_VERSION, 3, egl::NONE],
            )
            .map_err(|e| EglRuntimeError::CreateContext(format!("{e}")))?;

        // Surfaceless: per `EGL_KHR_surfaceless_context` we can pass
        // NO_SURFACE for both draw and read. Many drivers honor it
        // even without advertising the extension.
        if !ext_has(ext_str, "EGL_KHR_surfaceless_context") {
            debug!("EGL_KHR_surfaceless_context not advertised — trying NO_SURFACE anyway");
        }
        egl.make_current(display, None, None, Some(context))
            .map_err(|e| EglRuntimeError::MakeCurrent(format!("{e}")))?;

        // Load GL function pointers via eglGetProcAddress under the
        // current context.
        let egl_loader = &egl;
        gl::load_with(|sym| {
            let cstr = match CString::new(sym) {
                Ok(s) => s,
                Err(_) => return std::ptr::null(),
            };
            egl_loader
                .get_proc_address(cstr.to_str().unwrap_or(""))
                .map(|f| f as *const c_void)
                .unwrap_or(std::ptr::null())
        });

        // Verify the GL OES extension we depend on is present. Modern
        // core profile reports extensions via glGetStringi only.
        if !gl_has_extension("GL_OES_EGL_image") {
            // Tear down so we don't leak EGL state on the failure path.
            let _ = egl.make_current(display, None, None, None);
            let _ = egl.destroy_context(display, context);
            let _ = egl.terminate(display);
            return Err(EglRuntimeError::MissingGlExtension("GL_OES_EGL_image"));
        }

        let image_target_texture_2d_oes = resolve_proc::<PfnGlEGLImageTargetTexture2DOES>(
            &egl,
            "glEGLImageTargetTexture2DOES",
        )?;

        // Release current — every call site re-makes-current under
        // the lock so the runtime is thread-safe.
        egl.make_current(display, None, None, None)
            .map_err(|e| EglRuntimeError::MakeCurrent(format!("clear: {e}")))?;

        Ok(Arc::new(Self {
            egl,
            display,
            context,
            image_target_texture_2d_oes,
            make_current_lock: Mutex::new(()),
        }))
    }

    /// Lock + make-current; returns a guard that releases the context
    /// on drop. Every adapter operation that touches EGL or GL calls
    /// this before doing anything.
    pub fn lock_make_current(&self) -> Result<MakeCurrentGuard<'_>, EglRuntimeError> {
        let lock = self.make_current_lock.lock();
        self.egl
            .make_current(self.display, None, None, Some(self.context))
            .map_err(|e| EglRuntimeError::MakeCurrent(format!("acquire: {e}")))?;
        Ok(MakeCurrentGuard {
            runtime: self,
            _lock: lock,
        })
    }

    /// Owned-Arc variant of [`Self::lock_make_current`] returning a
    /// `'static`, [`Send`] guard that anchors its own `Arc<EglRuntime>`.
    ///
    /// Required by the polyglot FFI bindings, where acquire and release
    /// happen across separate FFI calls and the borrow-style guard's
    /// lifetime can't span them. In-process Rust callers should prefer
    /// [`Self::lock_make_current`] — its borrow-checked guard catches
    /// scope mistakes at compile time.
    pub fn arc_lock_make_current(
        self: &Arc<Self>,
    ) -> Result<OwnedMakeCurrentGuard, EglRuntimeError> {
        OwnedMakeCurrentGuard::new(Arc::clone(self))
    }

    /// Wrap `eglCreateImage(EGL_LINUX_DMA_BUF_EXT, …)` for the
    /// caller. Returns the imported [`khronos_egl::Image`]; destroy
    /// via [`Self::destroy_image`].
    ///
    /// `attribs` MUST be terminated with `egl::ATTRIB_NONE`. The
    /// caller is responsible for the FD lifetime — EGL dups the fd
    /// internally, so closing the original after this call returns
    /// is fine.
    pub fn create_dma_buf_image(
        &self,
        attribs: &[egl::Attrib],
    ) -> Result<egl::Image, EglRuntimeError> {
        // EGL 1.5's safe API takes a `Context` not an Option — pass
        // a NO_CONTEXT-wrapped one because DMA-BUF imports are
        // explicitly defined to use EGL_NO_CONTEXT.
        let no_ctx = unsafe { egl::Context::from_ptr(egl::NO_CONTEXT) };
        let no_buffer = unsafe { egl::ClientBuffer::from_ptr(std::ptr::null_mut()) };
        self.egl
            .create_image(
                self.display,
                no_ctx,
                EGL_LINUX_DMA_BUF_EXT,
                no_buffer,
                attribs,
            )
            .map_err(|e| EglRuntimeError::CreateImage(format!("{e}")))
    }

    /// Destroy a previously-created [`khronos_egl::Image`]. Errors
    /// are logged at warn level; double-destroy is a no-op-ish path
    /// in EGL.
    pub fn destroy_image(&self, image: egl::Image) {
        if let Err(e) = self.egl.destroy_image(self.display, image) {
            tracing::warn!(?e, "destroy_image failed (ignored)");
        }
    }

    /// Bind `image` to the currently-bound `GL_TEXTURE_2D` of the
    /// active context. The caller MUST be holding a
    /// [`MakeCurrentGuard`] when invoking this.
    ///
    /// # Safety
    /// `image` must be a non-null `EGLImage` obtained from
    /// [`Self::create_dma_buf_image`].
    pub unsafe fn image_target_texture_2d(&self, image: egl::Image) {
        unsafe { (self.image_target_texture_2d_oes)(gl::TEXTURE_2D, image.as_ptr()) };
    }
}

impl Drop for EglRuntime {
    fn drop(&mut self) {
        let _ = self.egl.make_current(self.display, None, None, None);
        let _ = self.egl.destroy_context(self.display, self.context);
        let _ = self.egl.terminate(self.display);
    }
}

/// RAII guard that holds the runtime's make-current lock and the EGL
/// current-context state. On drop the context is cleared so a
/// different thread can re-make-current on its next call.
pub struct MakeCurrentGuard<'r> {
    runtime: &'r EglRuntime,
    _lock: parking_lot::MutexGuard<'r, ()>,
}

impl Drop for MakeCurrentGuard<'_> {
    fn drop(&mut self) {
        let _ = self.runtime.egl.make_current(self.runtime.display, None, None, None);
    }
}

/// Owned-Arc make-current guard. `'static` and [`Send`]; anchors its
/// own `Arc<EglRuntime>` so the guard can outlive any borrow of the
/// runtime — required by the polyglot FFI bindings, where acquire and
/// release happen across separate FFI calls.
pub struct OwnedMakeCurrentGuard {
    runtime: Arc<EglRuntime>,
    lock: ManuallyDrop<MutexGuard<'static, ()>>,
}

impl OwnedMakeCurrentGuard {
    fn new(runtime: Arc<EglRuntime>) -> Result<Self, EglRuntimeError> {
        let lock_borrowed = runtime.make_current_lock.lock();
        runtime
            .egl
            .make_current(
                runtime.display,
                None,
                None,
                Some(runtime.context),
            )
            .map_err(|e| EglRuntimeError::MakeCurrent(format!("acquire: {e}")))?;
        // SAFETY: `lock_borrowed` borrows `runtime.make_current_lock`. We
        // anchor an `Arc<EglRuntime>` inside the same struct, so the
        // referent outlives the (lifetime-extended) guard. The guard is
        // dropped before the Arc in our manual `Drop` impl, restoring
        // the borrow ordering.
        let lock = unsafe {
            std::mem::transmute::<MutexGuard<'_, ()>, MutexGuard<'static, ()>>(lock_borrowed)
        };
        Ok(Self {
            runtime,
            lock: ManuallyDrop::new(lock),
        })
    }
}

impl Drop for OwnedMakeCurrentGuard {
    fn drop(&mut self) {
        let _ = self
            .runtime
            .egl
            .make_current(self.runtime.display, None, None, None);
        // SAFETY: the lock has not yet been dropped (we never call
        // `ManuallyDrop::drop` outside this Drop impl). Drop it here to
        // release the mutex before our Arc reference goes away.
        unsafe { ManuallyDrop::drop(&mut self.lock) };
    }
}

// SAFETY: `MutexGuard<'_, ()>` is `Send` only when the underlying
// `Mutex<()>` is `Send + Sync` (parking_lot's mutex is). The
// `Arc<EglRuntime>` is `Send` (we declared `EglRuntime: Send + Sync`
// at the top of this module). EGL's `make_current` is per-thread —
// the guard's invariant ("EGL context is current on the holder's
// thread") ports across thread moves only if the holder
// re-makes-current after move; in practice the FFI binds the guard to
// the calling thread's life-cycle and never moves it.
unsafe impl Send for OwnedMakeCurrentGuard {}

/// Pick an EGL config that's renderable as OpenGL and supports
/// pbuffer surfaces. We don't actually create a pbuffer (surfaceless
/// context), but most drivers refuse to give us an OpenGL-renderable
/// config without at least one surface bit set.
fn pick_config(
    egl: &egl::DynamicInstance<egl::EGL1_5>,
    display: egl::Display,
) -> Result<egl::Config, EglRuntimeError> {
    let attribs = [
        egl::SURFACE_TYPE,
        egl::PBUFFER_BIT,
        egl::RENDERABLE_TYPE,
        egl::OPENGL_BIT,
        egl::RED_SIZE,
        8,
        egl::GREEN_SIZE,
        8,
        egl::BLUE_SIZE,
        8,
        egl::ALPHA_SIZE,
        8,
        egl::NONE,
    ];
    let mut configs: Vec<egl::Config> = Vec::with_capacity(1);
    egl.choose_config(display, &attribs, &mut configs)
        .map_err(|e| EglRuntimeError::Initialize(format!("choose_config: {e}")))?;
    configs.into_iter().next().ok_or(EglRuntimeError::NoConfig)
}

/// Membership check across an EGL/GL extension string.
///
/// Both `eglQueryString(EGL_EXTENSIONS)` and (legacy) `glGetString(GL_EXTENSIONS)`
/// return space-separated tokens; this avoids the substring-collision
/// bug where `GL_EXT_foo` would match the prefix of `GL_EXT_foo_bar`.
fn ext_has(haystack: &str, needle: &str) -> bool {
    haystack.split_ascii_whitespace().any(|tok| tok == needle)
}

/// Walk the GL_EXTENSIONS index list (modern core profile) — the
/// legacy `glGetString(GL_EXTENSIONS)` returns NULL for core 3.2+.
fn gl_has_extension(name: &str) -> bool {
    let mut count: gl::types::GLint = 0;
    unsafe { gl::GetIntegerv(gl::NUM_EXTENSIONS, &mut count) };
    for i in 0..count {
        let ptr = unsafe { gl::GetStringi(gl::EXTENSIONS, i as u32) };
        if ptr.is_null() {
            continue;
        }
        let cstr = unsafe { CStr::from_ptr(ptr as *const c_char) };
        if cstr.to_str().map(|s| s == name).unwrap_or(false) {
            return true;
        }
    }
    false
}

/// Resolve an EGL/GL extension function pointer by name and cast it
/// to the adapter-defined function type.
fn resolve_proc<F>(
    egl: &egl::DynamicInstance<egl::EGL1_5>,
    name: &'static str,
) -> Result<F, EglRuntimeError> {
    let raw = egl
        .get_proc_address(name)
        .ok_or(EglRuntimeError::MissingProcAddr(name))?;
    // SAFETY: extension function pointers are pointer-sized; F has
    // the spec-mandated signature for `name`. Mismatched ABI here is
    // a driver bug.
    Ok(unsafe { std::mem::transmute_copy::<extern "system" fn(), F>(&raw) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ext_has_does_not_match_prefix() {
        let s = "GL_EXT_foo GL_EXT_foo_bar GL_EXT_baz";
        assert!(ext_has(s, "GL_EXT_foo"));
        assert!(ext_has(s, "GL_EXT_foo_bar"));
        assert!(ext_has(s, "GL_EXT_baz"));
        assert!(!ext_has(s, "GL_EXT_fo"));
        assert!(!ext_has(s, "GL_EXT_foob"));
        assert!(!ext_has(s, ""));
    }

    #[test]
    fn drm_fourcc_codes_are_little_endian_chars() {
        // "AB24" → bytes [0x41, 0x42, 0x32, 0x34] → little-endian
        // u32 = 0x34_32_42_41.
        assert_eq!(DRM_FORMAT_ABGR8888, 0x34_32_42_41);
        // "AR24" → [0x41, 0x52, 0x32, 0x34] → 0x34_32_52_41.
        assert_eq!(DRM_FORMAT_ARGB8888, 0x34_32_52_41);
    }

    /// `arc_lock_make_current` returns a `'static`-lifetime guard that
    /// outlives any borrow of the [`EglRuntime`]. The polyglot FFI uses
    /// this so `acquire_*` / `release_*` can hold the EGL mutex across
    /// separate FFI calls. Drop releases the mutex (verified by
    /// re-acquiring on the same thread).
    #[test]
    fn arc_lock_make_current_is_static_and_releases_on_drop() {
        let runtime = match EglRuntime::new() {
            Ok(r) => r,
            Err(e) => {
                println!("arc_lock_make_current: skipping — no EGL on this host: {e}");
                return;
            }
        };
        let guard1 = runtime.arc_lock_make_current().expect("first acquire");
        // Concrete check on the lifetime: the guard must satisfy
        // `'static + Send` so the FFI can stash it in a HashMap entry.
        fn assert_static_send<T: Send + 'static>(_: &T) {}
        assert_static_send(&guard1);
        drop(guard1);
        // After release the mutex is free — re-acquiring on the same
        // thread must succeed without deadlocking.
        let _guard2 = runtime
            .arc_lock_make_current()
            .expect("re-acquire after drop");
    }
}
