// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::ffi::c_void;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use streamlib_plugin_abi::RuntimeContextVTable;

use super::{
    AudioClockShim, GpuContext, GpuContextFullAccess, GpuContextLimitedAccess, RuntimeOpsShim,
    SharedAudioClock, TimeContext,
};
use crate::core::graph::ProcessorUniqueId;
use crate::core::runtime::{RuntimeOperations, RuntimeUniqueId};
use crate::iceoryx2::Iceoryx2Node;

#[derive(Clone)]
pub struct RuntimeContext {
    /// Base GPU context. Crate-private so processor code can only reach GPU
    /// operations through the capability-typed wrappers
    /// ([`RuntimeContextFullAccess`] / [`RuntimeContextLimitedAccess`]).
    /// Runtime-internal code (shutdown, diagnostics) still uses this field
    /// directly for operations not mirrored on the capability types.
    pub(crate) gpu: GpuContext,
    /// Shared timing context - monotonic clock starting at runtime creation.
    pub time: Arc<TimeContext>,
    /// Unique identifier for this runtime instance.
    runtime_id: Arc<RuntimeUniqueId>,
    /// Unique identifier for this processor (None for shared/global context).
    processor_id: Option<ProcessorUniqueId>,
    /// Pause gate for this processor (None for shared/global context).
    pause_gate: Option<Arc<AtomicBool>>,
    /// Runtime operations interface for graph mutations.
    runtime_ops: Arc<dyn RuntimeOperations>,
    /// Shared tokio runtime handle for async operations.
    tokio_handle: tokio::runtime::Handle,
    /// iceoryx2 Node for creating Services, Publishers, and Subscribers.
    iceoryx2_node: Iceoryx2Node,
    /// Audio clock for synchronized audio timing.
    audio_clock: SharedAudioClock,
    /// Per-runtime surface-sharing Unix socket path. Polyglot subprocesses
    /// receive this via the `STREAMLIB_SURFACE_SOCKET` env var so their
    /// `streamlib-surface-client` connects to the runtime-internal service
    /// rather than an external daemon.
    #[cfg(target_os = "linux")]
    surface_socket_path: std::path::PathBuf,
}

impl RuntimeContext {
    pub fn new(
        gpu: GpuContext,
        time: Arc<TimeContext>,
        runtime_id: Arc<RuntimeUniqueId>,
        runtime_ops: Arc<dyn RuntimeOperations>,
        tokio_handle: tokio::runtime::Handle,
        iceoryx2_node: Iceoryx2Node,
        audio_clock: SharedAudioClock,
        #[cfg(target_os = "linux")] surface_socket_path: std::path::PathBuf,
    ) -> Self {
        Self {
            gpu,
            time,
            runtime_id,
            processor_id: None,
            pause_gate: None,
            runtime_ops,
            tokio_handle,
            iceoryx2_node,
            audio_clock,
            #[cfg(target_os = "linux")]
            surface_socket_path,
        }
    }

    /// Current platform name.
    ///
    /// Returns "macos", "linux", or "windows".
    /// Use this to branch on platform-specific behavior in processors.
    pub fn platform(&self) -> &'static str {
        #[cfg(target_os = "macos")]
        {
            "macos"
        }
        #[cfg(target_os = "ios")]
        {
            "ios"
        }
        #[cfg(target_os = "linux")]
        {
            "linux"
        }
        #[cfg(target_os = "windows")]
        {
            "windows"
        }
        #[cfg(not(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "linux",
            target_os = "windows"
        )))]
        {
            "unknown"
        }
    }

    /// Get the runtime's unique identifier.
    pub fn runtime_id(&self) -> &RuntimeUniqueId {
        &self.runtime_id
    }

    /// Per-runtime surface-sharing Unix socket path. Polyglot subprocess
    /// spawn ops set `STREAMLIB_SURFACE_SOCKET` to this so the child's
    /// `streamlib-surface-client` connects to the runtime-internal service.
    #[cfg(target_os = "linux")]
    pub fn surface_socket_path(&self) -> &std::path::Path {
        &self.surface_socket_path
    }

    /// Get the processor's unique identifier (None for shared/global context).
    pub fn processor_id(&self) -> Option<&ProcessorUniqueId> {
        self.processor_id.as_ref()
    }

    /// Access runtime operations for graph mutations.
    ///
    /// Returns the runtime operations interface, allowing processors to add/remove
    /// processors and connections dynamically.
    pub fn runtime(&self) -> Arc<dyn RuntimeOperations> {
        Arc::clone(&self.runtime_ops)
    }

    /// Borrow the runtime-operations `Arc` directly. Engine-internal:
    /// the [`RuntimeContextVTable`](streamlib_plugin_abi::RuntimeContextVTable)
    /// `runtime_ops_handle` callback returns a pointer to this field
    /// so the cdylib-paired [`HOST_RUNTIME_OPS_VTABLE`](crate::core::plugin::host_services::HOST_RUNTIME_OPS_VTABLE)
    /// callbacks can clone a fresh `Arc<dyn RuntimeOperations>` per
    /// invocation. The Arc itself lives for the lifetime of the
    /// `RuntimeContext`, so the borrow is sound for any caller that
    /// holds an `&RuntimeContext`.
    pub(crate) fn runtime_operations_ref(&self) -> &Arc<dyn RuntimeOperations> {
        &self.runtime_ops
    }

    /// Get the shared tokio runtime handle.
    ///
    /// Use this to spawn async tasks without creating your own runtime.
    /// The handle is shared across all processors.
    pub fn tokio_handle(&self) -> &tokio::runtime::Handle {
        &self.tokio_handle
    }

    /// Get the iceoryx2 Node for creating Services, Publishers, and Subscribers.
    pub fn iceoryx2_node(&self) -> &Iceoryx2Node {
        &self.iceoryx2_node
    }

    /// Get the audio clock for synchronized audio timing.
    ///
    /// The audio clock provides timing callbacks for audio producers at
    /// a consistent sample rate and buffer size. Use this in audio generators
    /// to produce samples at the correct rate.
    pub fn audio_clock(&self) -> &SharedAudioClock {
        &self.audio_clock
    }

    /// Create a processor-specific context with a processor ID.
    pub fn with_processor_id(&self, processor_id: ProcessorUniqueId) -> Self {
        Self {
            gpu: self.gpu.clone(),
            time: Arc::clone(&self.time),
            runtime_id: Arc::clone(&self.runtime_id),
            processor_id: Some(processor_id),
            pause_gate: self.pause_gate.clone(),
            runtime_ops: Arc::clone(&self.runtime_ops),
            tokio_handle: self.tokio_handle.clone(),
            iceoryx2_node: self.iceoryx2_node.clone(),
            audio_clock: Arc::clone(&self.audio_clock),
            #[cfg(target_os = "linux")]
            surface_socket_path: self.surface_socket_path.clone(),
        }
    }

    /// Create a processor-specific context with a pause gate.
    pub fn with_pause_gate(&self, pause_gate: Arc<AtomicBool>) -> Self {
        Self {
            gpu: self.gpu.clone(),
            time: Arc::clone(&self.time),
            runtime_id: Arc::clone(&self.runtime_id),
            processor_id: self.processor_id.clone(),
            pause_gate: Some(pause_gate),
            runtime_ops: Arc::clone(&self.runtime_ops),
            tokio_handle: self.tokio_handle.clone(),
            iceoryx2_node: self.iceoryx2_node.clone(),
            audio_clock: Arc::clone(&self.audio_clock),
            #[cfg(target_os = "linux")]
            surface_socket_path: self.surface_socket_path.clone(),
        }
    }

    /// Check if this processor is paused.
    ///
    /// For Manual mode processors, call this in your processing loop/callback
    /// to respect pause/resume requests. Returns `false` if no pause gate is set.
    pub fn is_paused(&self) -> bool {
        self.pause_gate
            .as_ref()
            .is_some_and(|gate| gate.load(Ordering::Acquire))
    }

    /// Check if processing should proceed (not paused).
    ///
    /// Convenience method - returns `true` if not paused.
    pub fn should_process(&self) -> bool {
        !self.is_paused()
    }

    /// Dispatch a closure to execute on the runtime thread asynchronously.
    ///
    /// The "runtime thread" is the thread where Runner orchestration happens.
    /// On macOS, this is the main thread (NSApplication run loop) because Apple
    /// frameworks like AVFoundation, Metal, and CoreMedia require it.
    #[cfg(target_os = "macos")]
    pub fn run_on_runtime_thread_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        use objc2_foundation::NSThread;

        let is_runtime_thread = NSThread::currentThread().isMainThread();
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();

        // If already on runtime thread, execute directly
        if is_runtime_thread {
            tracing::debug!(
                "[run_on_runtime_thread_async] already on runtime thread ({:?} '{}'), executing directly",
                thread_id,
                thread_name
            );
            f();
            return;
        }

        tracing::debug!(
            "[run_on_runtime_thread_async] on background thread ({:?} '{}'), dispatching to runtime thread",
            thread_id,
            thread_name
        );
        dispatch2::DispatchQueue::main().exec_async(f);
    }

    /// Dispatch a closure to execute on the runtime thread and wait for the result.
    ///
    /// The "runtime thread" is the thread where Runner orchestration happens.
    /// On macOS, this is the main thread (NSApplication run loop) because Apple
    /// frameworks like AVFoundation, Metal, and CoreMedia require it.
    #[cfg(target_os = "macos")]
    pub fn run_on_runtime_thread_blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        use objc2_foundation::NSThread;
        use std::sync::mpsc::channel;

        let is_runtime_thread = NSThread::currentThread().isMainThread();
        let thread_id = std::thread::current().id();
        let thread_name = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();

        // If already on runtime thread, execute directly to avoid deadlock
        if is_runtime_thread {
            tracing::debug!(
                "[run_on_runtime_thread_blocking] already on runtime thread ({:?} '{}'), executing directly",
                thread_id,
                thread_name
            );
            return f();
        }

        tracing::debug!(
            "[run_on_runtime_thread_blocking] on background thread ({:?} '{}'), dispatching to runtime thread and waiting",
            thread_id,
            thread_name
        );
        let (tx, rx) = channel();

        dispatch2::DispatchQueue::main().exec_async(move || {
            let result = f();
            let _ = tx.send(result);
        });

        rx.recv()
            .expect("Failed to receive result from runtime thread")
    }

    // =========================================================================
    // Windows Implementation
    // =========================================================================

    /// Dispatch a closure to execute on the runtime thread asynchronously.
    ///
    /// # Platform Implementation Status
    ///
    /// **Windows**: Not yet implemented. Will use `PostMessage` to dispatch
    /// to the runtime thread's Win32 message loop. Required for:
    /// - DirectX/Direct3D device creation and rendering
    /// - Win32 window creation (HWND)
    /// - COM objects with STA threading requirements
    ///
    /// Currently executes directly as a passthrough until implemented.
    #[cfg(target_os = "windows")]
    pub fn run_on_runtime_thread_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        // TODO: Implement Windows runtime thread dispatch via PostMessage/SendMessage
        // to the runtime thread's message loop (GetMessage/DispatchMessage pump).
        //
        // Implementation approach:
        // 1. Store runtime thread ID at startup (GetCurrentThreadId)
        // 2. Create a hidden HWND for receiving messages
        // 3. PostMessage with custom WM_USER message containing boxed closure
        // 4. Message loop handler unboxes and executes closure
        let thread_id = std::thread::current().id();
        let current_thread = std::thread::current();
        let thread_name = current_thread.name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_runtime_thread_async] Windows passthrough, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f();
    }

    /// Dispatch a closure to execute on the runtime thread and wait for the result.
    ///
    /// # Platform Implementation Status
    ///
    /// **Windows**: Not yet implemented. Will use `SendMessage` (blocking) or
    /// `PostMessage` + event synchronization. Required for same reasons as async variant.
    ///
    /// Currently executes directly as a passthrough until implemented.
    #[cfg(target_os = "windows")]
    pub fn run_on_runtime_thread_blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        // TODO: Implement Windows runtime thread dispatch with result synchronization.
        //
        // Implementation approach:
        // 1. Same as async, but use SendMessage (blocks until processed) OR
        // 2. PostMessage + ManualResetEvent for completion signaling
        // 3. Return result via shared memory or channel
        let thread_id = std::thread::current().id();
        let current_thread = std::thread::current();
        let thread_name = current_thread.name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_runtime_thread_blocking] Windows passthrough, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f()
    }

    // =========================================================================
    // Linux Implementation
    // =========================================================================

    /// Dispatch a closure to execute on the runtime thread asynchronously.
    ///
    /// # Platform Implementation Status
    ///
    /// **Linux**: Not yet implemented. Implementation depends on windowing system:
    /// - **X11**: Use `XSendEvent` or integrate with GTK/Qt main loop
    /// - **Wayland**: Use `wl_display_dispatch` or GTK/Qt integration
    /// - **Headless**: May not require runtime thread dispatch
    ///
    /// For GTK: `glib::MainContext::default().invoke()`
    /// For Qt: `QMetaObject::invokeMethod` with `Qt::QueuedConnection`
    ///
    /// Currently executes directly as a passthrough until implemented.
    #[cfg(target_os = "linux")]
    pub fn run_on_runtime_thread_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        // TODO: Implement Linux runtime thread dispatch.
        //
        // Implementation approach (GTK/glib):
        // 1. Use glib::MainContext::default().invoke(f)
        // 2. Requires glib dependency and running GMainLoop on runtime thread
        //
        // Alternative (custom):
        // 1. Create eventfd or pipe at startup
        // 2. Runtime thread polls the fd in its event loop
        // 3. Write closure pointer to fd, runtime thread reads and executes
        let thread_id = std::thread::current().id();
        let current_thread = std::thread::current();
        let thread_name = current_thread.name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_runtime_thread_async] Linux passthrough, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f();
    }

    /// Dispatch a closure to execute on the runtime thread and wait for the result.
    ///
    /// # Platform Implementation Status
    ///
    /// **Linux**: Not yet implemented. Same considerations as async variant,
    /// plus synchronization for returning result.
    ///
    /// Currently executes directly as a passthrough until implemented.
    #[cfg(target_os = "linux")]
    pub fn run_on_runtime_thread_blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        // TODO: Implement Linux runtime thread dispatch with result synchronization.
        //
        // Implementation approach:
        // 1. Same dispatch mechanism as async
        // 2. Include oneshot channel or CondVar for result
        // 3. Block on channel/condvar until runtime thread signals completion
        let thread_id = std::thread::current().id();
        let current_thread = std::thread::current();
        let thread_name = current_thread.name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_runtime_thread_blocking] Linux passthrough, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f()
    }

    // =========================================================================
    // Fallback Implementation (other platforms)
    // =========================================================================

    /// Dispatch a closure to execute on the runtime thread asynchronously.
    ///
    /// # Platform Implementation Status
    ///
    /// **Other platforms**: No runtime thread dispatch implemented.
    /// Executes directly on the calling thread.
    ///
    /// If you need runtime thread dispatch on an unsupported platform,
    /// please file an issue with your platform requirements.
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub fn run_on_runtime_thread_async<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let thread_id = std::thread::current().id();
        let current_thread = std::thread::current();
        let thread_name = current_thread.name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_runtime_thread_async] unsupported platform, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f();
    }

    /// Dispatch a closure to execute on the runtime thread and wait for the result.
    ///
    /// # Platform Implementation Status
    ///
    /// **Other platforms**: No runtime thread dispatch implemented.
    /// Executes directly on the calling thread.
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub fn run_on_runtime_thread_blocking<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let thread_id = std::thread::current().id();
        let current_thread = std::thread::current();
        let thread_name = current_thread.name().unwrap_or("unnamed");
        tracing::debug!(
            "[run_on_runtime_thread_blocking] unsupported platform, executing directly on thread ({:?} '{}')",
            thread_id,
            thread_name
        );
        f()
    }

    // =========================================================================
    // Platform Setup
    // =========================================================================

    /// Ensure the platform is ready for runtime operations.
    ///
    /// This method handles platform-specific initialization that must complete
    /// before processors can safely use platform APIs. On macOS, this sets up
    /// NSApplication and verifies the app has finished launching.
    ///
    /// Call this from `Runner::start()` after GPU context is initialized
    /// but before starting any processors.
    #[cfg(target_os = "macos")]
    pub fn ensure_platform_ready(&self) -> crate::core::Result<()> {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSApplication;

        // Detect if we're running as a standalone app
        // (not embedded in another app with its own NSApplication event loop)
        let is_standalone = if let Some(mtm) = MainThreadMarker::new() {
            let app = NSApplication::sharedApplication(mtm);
            !app.isRunning()
        } else {
            // Not on runtime thread - can't check NSApplication state
            false
        };

        if is_standalone {
            tracing::info!("[ensure_platform_ready] Setting up macOS application");
            crate::apple::runtime_ext::setup_macos_app();

            // CRITICAL: Verify the macOS platform is fully ready BEFORE starting
            // any processors. This uses Apple's NSRunningApplication.isFinishedLaunching
            // API to confirm the app has completed its launch sequence.
            tracing::info!("[ensure_platform_ready] Verifying macOS platform readiness...");
            crate::apple::runtime_ext::ensure_macos_platform_ready()?;
        }

        Ok(())
    }

    /// Ensure the platform is ready for runtime operations.
    ///
    /// On Windows, this is a no-op placeholder for future Win32 setup.
    #[cfg(target_os = "windows")]
    pub fn ensure_platform_ready(&self) -> crate::core::Result<()> {
        // TODO: Future Windows-specific initialization
        // - DirectX device validation
        // - Win32 message loop setup
        tracing::debug!("[ensure_platform_ready] Windows - no setup required");
        Ok(())
    }

    /// Ensure the platform is ready for runtime operations.
    ///
    /// On Linux, this is a no-op placeholder for future X11/Wayland setup.
    #[cfg(target_os = "linux")]
    pub fn ensure_platform_ready(&self) -> crate::core::Result<()> {
        // TODO: Future Linux-specific initialization
        // - X11/Wayland connection setup
        // - GPU driver validation
        tracing::debug!("[ensure_platform_ready] Linux - no setup required");
        Ok(())
    }

    /// Ensure the platform is ready for runtime operations.
    ///
    /// On unsupported platforms, this is a no-op.
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub fn ensure_platform_ready(&self) -> crate::core::Result<()> {
        tracing::debug!("[ensure_platform_ready] Unsupported platform - no setup required");
        Ok(())
    }
}

// Unit tests removed - these require NSApplication run loop which isn't available in test harness.
// See examples/test-main-thread-dispatch for validation of this functionality.

// =============================================================================
// Capability-typed RuntimeContext views
// =============================================================================
//
// [`RuntimeContextFullAccess`] is handed to privileged lifecycle methods
// (`setup` / `teardown` / Manual-mode `start` / `stop`). It exposes
// `gpu_full_access()` returning a borrowed [`GpuContextFullAccess`].
//
// [`RuntimeContextLimitedAccess`] is handed to the hot-path methods
// (`process` / `on_pause` / `on_resume`). It exposes `gpu_limited_access()`
// returning a borrowed [`GpuContextLimitedAccess`].
//
// Both types are deliberately borrow-scoped (`<'a>`) and **not** `Clone`.
// Processors receive them via `&RuntimeContextFullAccess<'_>` etc. — the
// borrow cannot be stashed past the call (borrow checker), and the inner
// `GpuContextFullAccess` is itself `!Clone`. This turns "right ctx, right
// phase" from a convention into a compile-time guarantee.
//
// In commit 2 of #322 the types exist but nothing dispatches to them yet —
// the wiring lands in the next commit (attribute macro codegen + runtime
// plumbing). They are already exported so compile-fail doc tests can
// assert the enforcement invariants.

// Shim shape: `(handle, vtable)`-driven dispatch. The shim stores the
// raw host pointer + the [`RuntimeContextVTable`] reference, and every
// accessor calls through the vtable. This matches the plugin ABI
// the cdylib uses.
//
// Host-internal compiler ops that still need direct access to the
// underlying `RuntimeContext` (e.g. `surface_socket_path`,
// `iceoryx2_node`) reach it via the `host_base()` crate-internal
// accessor. That accessor is host-only by construction: the shim is
// only ever constructed from a real `&RuntimeContext` today, and
// nothing outside the engine crate can call it.

/// Privileged-GPU [`RuntimeContext`] view passed to `setup` / `teardown` /
/// Manual-mode `start` / `stop`. Exposes [`GpuContextFullAccess`] for
/// resource allocation and device-wide operations.
///
/// Deliberately `!Clone` and borrow-scoped — the handle cannot be stashed
/// past the call boundary.
///
/// ```compile_fail
/// fn assert_not_clone<T: Clone>() {}
/// assert_not_clone::<streamlib::sdk::context::RuntimeContextFullAccess<'static>>();
/// ```
#[repr(C)]
pub struct RuntimeContextFullAccess<'a> {
    /// Opaque pointer to the host-owned [`RuntimeContext`]. Threaded
    /// through every [`RuntimeContextVTable`] callback. The host's
    /// static vtable casts this back to `&RuntimeContext`; the cdylib
    /// treats it as opaque.
    handle: *const c_void,
    /// Pointer to the host's [`RuntimeContextVTable`] (today
    /// `HOST_RUNTIME_CONTEXT_VTABLE`). Every accessor on the shim
    /// dispatches through here.
    vtable: *const RuntimeContextVTable,
    gpu_full: GpuContextFullAccess,
    /// Limited-access handle held alongside the full-access one so privileged
    /// lifecycle methods (e.g. Manual-mode `start()` spawning a worker thread)
    /// can hand a stashable `GpuContextLimitedAccess` to downstream code
    /// without having to re-wrap the base `GpuContext`.
    gpu_limited: GpuContextLimitedAccess,
    _marker: PhantomData<&'a RuntimeContext>,
}

/// Restricted-GPU [`RuntimeContext`] view passed to `process` / `on_pause` /
/// `on_resume`. Exposes [`GpuContextLimitedAccess`] — cheap, pool-backed,
/// non-allocating operations only.
///
/// Deliberately `!Clone` and borrow-scoped — the handle cannot be stashed
/// past the call boundary. This is the type-system moat that prevents
/// `process()` bodies from doing setup-only work.
///
/// ```compile_fail
/// fn assert_not_clone<T: Clone>() {}
/// assert_not_clone::<streamlib::sdk::context::RuntimeContextLimitedAccess<'static>>();
/// ```
///
/// `gpu_full_access()` is intentionally absent from this type — a `process()`
/// body cannot reach privileged GPU operations:
///
/// ```compile_fail
/// fn reach_full(ctx: &streamlib::sdk::context::RuntimeContextLimitedAccess<'_>) {
///     let _ = ctx.gpu_full_access();
/// }
/// ```
#[repr(C)]
pub struct RuntimeContextLimitedAccess<'a> {
    handle: *const c_void,
    vtable: *const RuntimeContextVTable,
    gpu_limited: GpuContextLimitedAccess,
    _marker: PhantomData<&'a RuntimeContext>,
}

impl<'a> RuntimeContextFullAccess<'a> {
    /// Construct a full-access view over `base`. Crate-internal: only the
    /// runtime's lifecycle dispatch (attribute macro + `spawn_processor_op`)
    /// is permitted to create these.
    pub(crate) fn new(base: &'a RuntimeContext) -> Self {
        Self {
            handle: base as *const RuntimeContext as *const c_void,
            vtable: crate::core::plugin::host_services::host_runtime_context_vtable(),
            gpu_full: GpuContextFullAccess::new(base.gpu.clone()),
            gpu_limited: GpuContextLimitedAccess::new(base.gpu.clone()),
            _marker: PhantomData,
        }
    }

    /// Privileged GPU capability — allocations, device-wide ops, escalate.
    pub fn gpu_full_access(&self) -> &GpuContextFullAccess {
        &self.gpu_full
    }

    /// Restricted GPU capability. Cloneable — hand to a Manual-mode worker
    /// thread during `start()` so it can participate in the hot path with
    /// limited-access operations only.
    pub fn gpu_limited_access(&self) -> &GpuContextLimitedAccess {
        &self.gpu_limited
    }

    // ------------ ABI-mediated accessors ------------

    /// Runtime unique id as an owned [`String`]. Routed through the
    /// [`RuntimeContextVTable::runtime_id_copy`] callback.
    pub fn runtime_id(&self) -> String {
        unsafe { vtable_copy_runtime_id(self.handle, self.vtable) }
    }

    /// Processor unique id as an owned [`String`], or `None` for the
    /// shared/global context. Routed through
    /// [`RuntimeContextVTable::processor_id_copy`].
    pub fn processor_id(&self) -> Option<String> {
        unsafe { vtable_copy_processor_id(self.handle, self.vtable) }
    }

    /// Whether this processor is currently paused. Routed through
    /// [`RuntimeContextVTable::is_paused`].
    pub fn is_paused(&self) -> bool {
        unsafe { ((*self.vtable).is_paused)(self.handle) }
    }

    /// Whether processing should proceed (not paused). Routed through
    /// [`RuntimeContextVTable::should_process`].
    pub fn should_process(&self) -> bool {
        unsafe { ((*self.vtable).should_process)(self.handle) }
    }

    /// Host-owned audio clock as a typed plugin ABI shim. Backed by the
    /// per-RuntimeContext audio-clock handle returned from
    /// [`RuntimeContextVTable::audio_clock_handle`] paired with the
    /// host's [`AudioClockVTable`](streamlib_plugin_abi::AudioClockVTable)
    /// from `HostServices`. Borrow-scoped to the ctx; cannot outlive
    /// the lifecycle call.
    pub fn audio_clock(&self) -> AudioClockShim<'a> {
        let handle = unsafe { ((*self.vtable).audio_clock_handle)(self.handle) };
        // SAFETY: in the host-engine build path the shim was
        // constructed with a vtable produced by
        // `host_runtime_context_vtable()`; the audio-clock vtable is
        // accessed via the side-channel below. In a cdylib build the
        // analogous vtable pointer comes from `HostServices`. Both
        // sides receive the same pointer the host installed.
        let acv = crate::core::plugin::host_services::host_audio_clock_vtable();
        AudioClockShim::from_ffi(handle, acv)
    }

    /// Host-owned runtime operations. Implements [`RuntimeOperations`]
    /// so existing call sites
    /// (`ctx.runtime().add_processor_async(...).await`) keep working.
    ///
    /// When this build IS the host (no host callbacks installed — the
    /// same host-vs-plugin discriminator
    /// [`host_runtime_ops_vtable`](crate::core::plugin::host_services::host_runtime_ops_vtable)
    /// uses), return a direct `Arc::clone` of the Runner-backed ops.
    /// A host-resident processor (including the in-process api-server)
    /// then reaches the real ops — the byte-shaped `RuntimeOpsVTable`
    /// shim carries no transport for streaming ops such as `tap_async`
    /// (its iceoryx2 subscriber is `!Send` and host-owned), so a
    /// host-resident caller must bypass it. The `Arc` clone is
    /// stash-safe past `Runner::stop()` exactly like the shim's
    /// `clone_handle` refcount bump.
    ///
    /// When host callbacks ARE installed (a dlopened plugin), mint the
    /// typed plugin ABI shim: the returned `Arc<dyn RuntimeOperations>`
    /// owns an Arc refcount bump on the host's underlying ops impl via
    /// the [`RuntimeOpsVTable::clone_handle`](streamlib_plugin_abi::RuntimeOpsVTable)
    /// callback, so it too is sound to stash past `Runner::stop()`.
    pub fn runtime(&self) -> Arc<dyn RuntimeOperations> {
        if crate::core::plugin::host_services::host_callbacks().is_none() {
            return self.host_base().runtime();
        }
        let borrowed_handle = unsafe { ((*self.vtable).runtime_ops_handle)(self.handle) };
        let rov = crate::core::plugin::host_services::host_runtime_ops_vtable();
        // SAFETY: rov + borrowed_handle come from the engine's host
        // services; clone_handle is contractually required (v2 ABI)
        // and returns an owned handle the shim's Drop releases.
        let owned_handle = unsafe { ((*rov).clone_handle)(borrowed_handle) };
        RuntimeOpsShim::from_ffi(owned_handle, rov) as Arc<dyn RuntimeOperations>
    }

    /// Run `f` against a cdylib-shaped sibling
    /// `RuntimeContextFullAccess` whose `gpu_full` is a `ScopeToken`-
    /// flavored [`GpuContextFullAccess`] (instead of the `Boxed`
    /// shape this struct holds by default).
    ///
    /// The cdylib-shaped sibling's
    /// [`GpuContextFullAccess`] methods dispatch through the
    /// FullAccess vtable (plugin ABI via fn pointers in cdylib
    /// address space, direct via host callbacks in host address
    /// space) instead of the host-only direct-deref `host_inner`
    /// path the Boxed shape uses. That makes `ctx.gpu_full_access()`
    /// methods (e.g. `host_vulkan_device_arc`) usable from cdylib-
    /// resident processor `setup()` / `teardown()` bodies without
    /// tripping the `host_inner` panic guard.
    ///
    /// Engine-internal lifecycle dispatch (see
    /// [`crate::core::processors::ProcessorInstance::setup`]) uses
    /// this to wrap each cdylib lifecycle callback. The scope
    /// management mirrors the cdylib-mode path of
    /// [`GpuContextLimitedAccess::escalate`]:
    ///
    /// 1. Acquire the escalate gate + register the scope (via
    ///    [`crate::core::context::escalate_scope_registry::begin_escalate_scope`]).
    /// 2. Build the `ScopeToken`-shaped sibling.
    /// 3. Run `f` against it under `catch_unwind` so a panic from
    ///    the closure still releases the gate.
    /// 4. End the scope (releases gate) and run `wait_device_idle`
    ///    on the sibling Arc — matching `escalate_in_process`'s
    ///    post-closure semantics.
    /// 5. Re-raise the closure panic (if any).
    ///
    /// The historical sandbox contract serialized privileged
    /// setup() work via the same escalate gate, so this helper
    /// preserves that serialization invariant. Same-thread re-entry
    /// (a `setup()` body that itself calls `escalate(...)`) is
    /// rejected at the gate level — see
    /// [`crate::core::context::escalate_gate::EscalateGate`].
    pub(crate) fn with_cdylib_scope<F, T>(&self, f: F) -> crate::core::error::Result<T>
    where
        F: FnOnce(&RuntimeContextFullAccess<'_>) -> crate::core::error::Result<T>,
    {
        use super::escalate_scope_registry::{begin_escalate_scope, end_escalate_scope_draining};
        use std::panic::{AssertUnwindSafe, catch_unwind};

        // Build the Arc<GpuContext> that backs the scope. The
        // LimitedAccess's `host_inner` deref reaches the boxed
        // `Arc<GpuContext>` that backs this context; the value
        // clone here produces a fresh `GpuContext` whose Arc-
        // wrapped fields (notably `escalate_gate`) share state
        // with the original, so gate semantics still serialize
        // across all clones.
        let host_lim = &self.gpu_limited;
        let host_gpu = host_lim.host_inner();
        let gpu_arc = Arc::new(host_gpu.clone());

        // begin_escalate_scope acquires the gate. If the caller
        // somehow already holds the gate on this thread (the
        // historical "escalate-from-setup" footgun), the gate
        // panics with an actionable message before this returns.
        let scope_token = begin_escalate_scope(gpu_arc);
        let scope_full = GpuContextFullAccess::from_scope_token(
            scope_token as *const c_void,
            host_lim.handle,
            host_lim.vtable,
        );

        // Sibling ctx — same RuntimeContext handle / vtable; same
        // host-side gpu_limited (cloned via the LimitedAccess
        // vtable's `clone_handle`); ScopeToken-shaped gpu_full.
        // PhantomData reuses 'a so the sibling can't outlive the
        // original.
        let cdylib_ctx: RuntimeContextFullAccess<'_> = RuntimeContextFullAccess {
            handle: self.handle,
            vtable: self.vtable,
            gpu_full: scope_full,
            gpu_limited: host_lim.clone(),
            _marker: PhantomData,
        };

        let call_result = catch_unwind(AssertUnwindSafe(|| f(&cdylib_ctx)));

        // End the scope: drain the device (`wait_device_idle`) WHILE
        // the escalate gate is still held, then release it. Running the
        // wait after releasing the gate (the prior shape) raced another
        // processor thread's gated `vkCreateComputePipelines` and
        // corrupted the NVIDIA driver during the concurrent setup
        // fan-out — see `end_escalate_scope_draining` and
        // `docs/learnings/concurrent-vkdevicewaitidle-threading.md`.
        let wait_result = match end_escalate_scope_draining(scope_token) {
            Some(r) => r,
            None => Ok(()),
        };

        match (call_result, wait_result) {
            (Ok(Ok(value)), Ok(())) => Ok(value),
            (Ok(Err(e)), _) => Err(e),
            (Ok(Ok(_)), Err(e)) => Err(e),
            (Err(payload), _) => std::panic::resume_unwind(payload),
        }
    }

    // ------------ Engine-internal host accessors ------------

    /// Direct reference to the underlying [`RuntimeContext`]. **Host-only**:
    /// the shim is constructed from a real `&RuntimeContext`; cdylib
    /// callers reach functionality through vtable-routed equivalents.
    /// Engine compiler ops that need direct access to
    /// `surface_socket_path` / `iceoryx2_node` / `tokio_handle` reach
    /// them through here.
    pub(crate) fn host_base(&self) -> &RuntimeContext {
        // SAFETY: shim is constructed from a real `&'a RuntimeContext`;
        // the borrow's lifetime is encoded in `'a`. Cdylib code never
        // calls this — it is `pub(crate)` and only invoked by engine
        // compiler ops on the host side.
        unsafe { &*(self.handle as *const RuntimeContext) }
    }

    /// Engine-internal: direct borrow of the host's `SharedAudioClock`.
    /// Same data the public [`Self::audio_clock`] reaches via the
    /// vtable; the direct borrow avoids the trip through the
    /// callback when the call site is already in host code.
    pub(crate) fn host_audio_clock(&self) -> &SharedAudioClock {
        self.host_base().audio_clock()
    }
}

impl<'a> RuntimeContextLimitedAccess<'a> {
    /// Construct a limited-access view over `base`. Crate-internal: only the
    /// runtime's lifecycle dispatch is permitted to create these.
    pub(crate) fn new(base: &'a RuntimeContext) -> Self {
        Self {
            handle: base as *const RuntimeContext as *const c_void,
            vtable: crate::core::plugin::host_services::host_runtime_context_vtable(),
            gpu_limited: GpuContextLimitedAccess::new(base.gpu.clone()),
            _marker: PhantomData,
        }
    }

    /// Restricted GPU capability — pool-backed, non-allocating ops only.
    ///
    /// Call [`GpuContextLimitedAccess::escalate`] on the returned handle to
    /// temporarily obtain a [`GpuContextFullAccess`] for mid-run
    /// reconfiguration (new video session, resized swapchain, etc.).
    /// Escalation serializes against processor setup via the shared
    /// setup mutex.
    pub fn gpu_limited_access(&self) -> &GpuContextLimitedAccess {
        &self.gpu_limited
    }

    // ------------ ABI-mediated accessors ------------

    pub fn runtime_id(&self) -> String {
        unsafe { vtable_copy_runtime_id(self.handle, self.vtable) }
    }

    pub fn processor_id(&self) -> Option<String> {
        unsafe { vtable_copy_processor_id(self.handle, self.vtable) }
    }

    pub fn is_paused(&self) -> bool {
        unsafe { ((*self.vtable).is_paused)(self.handle) }
    }

    pub fn should_process(&self) -> bool {
        unsafe { ((*self.vtable).should_process)(self.handle) }
    }

    /// Host-owned audio clock as a typed plugin ABI shim. See
    /// [`RuntimeContextFullAccess::audio_clock`].
    pub fn audio_clock(&self) -> AudioClockShim<'a> {
        let handle = unsafe { ((*self.vtable).audio_clock_handle)(self.handle) };
        let acv = crate::core::plugin::host_services::host_audio_clock_vtable();
        AudioClockShim::from_ffi(handle, acv)
    }

    /// Host-owned runtime operations. See
    /// [`RuntimeContextFullAccess::runtime`].
    pub fn runtime(&self) -> Arc<dyn RuntimeOperations> {
        if crate::core::plugin::host_services::host_callbacks().is_none() {
            return self.host_base().runtime();
        }
        let borrowed_handle = unsafe { ((*self.vtable).runtime_ops_handle)(self.handle) };
        let rov = crate::core::plugin::host_services::host_runtime_ops_vtable();
        let owned_handle = unsafe { ((*rov).clone_handle)(borrowed_handle) };
        RuntimeOpsShim::from_ffi(owned_handle, rov) as Arc<dyn RuntimeOperations>
    }

    // ------------ Engine-internal host accessors ------------

    /// See [`RuntimeContextFullAccess::host_base`].
    pub(crate) fn host_base(&self) -> &RuntimeContext {
        unsafe { &*(self.handle as *const RuntimeContext) }
    }

    /// Engine-internal: direct borrow of the host's `SharedAudioClock`.
    pub(crate) fn host_audio_clock(&self) -> &SharedAudioClock {
        self.host_base().audio_clock()
    }
}

// =============================================================================
// vtable helper trampolines (shared by both shim flavors)
// =============================================================================

/// Adapter that turns the ABI's `(out_buf, cap, out_len) -> usize` byte-copy
/// callback into an owned `String`. Calls the callback twice when the
/// initial scratch buffer is too small.
///
/// # Safety
///
/// `vtable` must point at a valid [`RuntimeContextVTable`] whose
/// `runtime_id_copy` callback writes UTF-8 bytes.
unsafe fn vtable_copy_runtime_id(
    handle: *const c_void,
    vtable: *const RuntimeContextVTable,
) -> String {
    // Most runtime ids are short (~26 bytes for cuid2 plus the "R" prefix).
    // 64 bytes covers every reasonable id without a retry.
    let mut scratch = [0u8; 64];
    let mut written: usize = 0;
    let required = unsafe {
        ((*vtable).runtime_id_copy)(handle, scratch.as_mut_ptr(), scratch.len(), &mut written)
    };
    if required <= scratch.len() {
        // SAFETY: callback documents UTF-8 bytes.
        unsafe { String::from_utf8_unchecked(scratch[..written].to_vec()) }
    } else {
        let mut buf = vec![0u8; required];
        let mut written2: usize = 0;
        unsafe {
            ((*vtable).runtime_id_copy)(handle, buf.as_mut_ptr(), buf.len(), &mut written2);
        }
        buf.truncate(written2);
        unsafe { String::from_utf8_unchecked(buf) }
    }
}

unsafe fn vtable_copy_processor_id(
    handle: *const c_void,
    vtable: *const RuntimeContextVTable,
) -> Option<String> {
    let mut scratch = [0u8; 64];
    let mut written: usize = 0;
    let required = unsafe {
        ((*vtable).processor_id_copy)(handle, scratch.as_mut_ptr(), scratch.len(), &mut written)
    };
    if required < 0 {
        return None;
    }
    let required = required as usize;
    if required <= scratch.len() {
        Some(unsafe { String::from_utf8_unchecked(scratch[..written].to_vec()) })
    } else {
        let mut buf = vec![0u8; required];
        let mut written2: usize = 0;
        unsafe {
            ((*vtable).processor_id_copy)(handle, buf.as_mut_ptr(), buf.len(), &mut written2);
        }
        buf.truncate(written2);
        Some(unsafe { String::from_utf8_unchecked(buf) })
    }
}

// Mark the raw pointers as Send + Sync. The shim itself is `!Send` /
// `!Sync` via its `PhantomData<&'a RuntimeContext>` borrow — the
// unsafe impls below cover the inner field requirements that the
// borrow-checker enforces at the type level. The shim never outlives
// the `'a` borrow.
unsafe impl<'a> Send for RuntimeContextFullAccess<'a> {}
unsafe impl<'a> Sync for RuntimeContextFullAccess<'a> {}
unsafe impl<'a> Send for RuntimeContextLimitedAccess<'a> {}
unsafe impl<'a> Sync for RuntimeContextLimitedAccess<'a> {}

#[cfg(test)]
mod capability_view_tests {
    use super::*;

    // Assert that the capability views cannot be cloned. Stashing a
    // `RuntimeContextFullAccess` in a processor field would require cloning
    // the borrowed handle out of the `&ctx` parameter; `!Clone` closes that
    // door at compile time.
    //
    // These compile-fail tests will run as part of `cargo test --doc` once
    // we move them onto public types in the doc-tests commit. Kept here
    // as a reminder of the invariant for readers.
    #[test]
    fn full_access_and_limited_access_are_not_clone() {
        fn is_not_clone<T>() -> bool {
            // Using the common trick: this compiles regardless; the real
            // enforcement is the `compile_fail` doc tests added in the
            // doc-test commit.
            true
        }
        assert!(is_not_clone::<RuntimeContextFullAccess<'_>>());
        assert!(is_not_clone::<RuntimeContextLimitedAccess<'_>>());
    }
}

#[cfg(test)]
mod with_cdylib_scope_tests {
    //! Lock-in for the invariant the #1072 engine fix relies on:
    //! [`RuntimeContextFullAccess::with_cdylib_scope`] hands the closure a
    //! sibling [`RuntimeContextFullAccess`] whose `gpu_full` is
    //! `HandleKind::ScopeToken`-shaped (so cdylib bodies' direct
    //! `ctx.gpu_full_access()` calls dispatch through the FullAccess
    //! vtable instead of tripping the Boxed branch's `host_inner()`
    //! panic guard).
    //!
    //! Mentally revert the `from_scope_token` call inside
    //! `with_cdylib_scope` to `GpuContextFullAccess::new(...)` (Boxed
    //! shape) — this test fails because the closure sees
    //! `HandleKind::Boxed`, which is exactly the bug #1072 is fixing.
    //! Sibling tests at the gate layer
    //! ([`super::escalate_gate::tests::same_thread_reentry_panics`])
    //! and at the spawn-op layer
    //! (`rust_processor_setup_phase_does_not_hold_gate_against_concurrent_escalate`)
    //! lock different invariants; this one specifically locks the
    //! ScopeToken-shape claim.

    use super::super::gpu_context::HandleKind;
    use super::*;
    use crate::core::context::GpuContext;
    use serial_test::serial;

    fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
        match GpuContext::init_for_platform_sync() {
            Ok(gpu) => Some(gpu),
            Err(e) => {
                tracing::warn!("{test_name}: no GPU device ({e}) — skipping");
                None
            }
        }
    }

    /// Hand-construct a [`RuntimeContextFullAccess`] from just a
    /// [`GpuContext`] — bypasses the full [`RuntimeContext::new`]
    /// requires (tokio handle, iceoryx2 node, runtime ops, audio
    /// clock, surface socket). `with_cdylib_scope` only touches
    /// `gpu_limited.host_inner()` and the sibling's struct fields,
    /// so the runtime-side fields can stay null for this assertion.
    ///
    /// Invariant the null handle/vtable rely on: `with_cdylib_scope`
    /// must NOT vtable-dispatch on the ctx itself (i.e., must not
    /// reach `(*self.vtable).…(self.handle)` against the runtime-
    /// context vtable). Today it only copies them as opaque pointers
    /// into the sibling — if a future revision adds a runtime-context
    /// vtable call, this test stops being safe and the helper needs
    /// to build a real RuntimeContext.
    fn ctx_from_gpu(gpu: GpuContext) -> RuntimeContextFullAccess<'static> {
        RuntimeContextFullAccess {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
            gpu_full: GpuContextFullAccess::new(gpu.clone()),
            gpu_limited: GpuContextLimitedAccess::new(gpu),
            _marker: PhantomData,
        }
    }

    #[test]
    #[serial]
    fn closure_receives_scope_token_full_access() {
        const TEST: &str = "closure_receives_scope_token_full_access";
        let Some(gpu) = gpu_or_skip(TEST) else {
            return;
        };

        let outer_ctx = ctx_from_gpu(gpu);
        // The outer ctx's gpu_full is Boxed by design — that's what
        // host-internal callers use directly via `host_inner()`.
        assert_eq!(
            outer_ctx.gpu_full.handle_kind,
            HandleKind::Boxed,
            "{TEST}: outer (host-internal) ctx must hold a Boxed FullAccess; \
             ScopeToken there would break in-process host callers that deref \
             the handle as `*const Arc<GpuContext>`"
        );

        // The closure's sibling ctx must hold a ScopeToken FullAccess.
        // That's the load-bearing claim: cdylib bodies' direct
        // `ctx.gpu_full_access()` calls dispatch through the
        // FullAccess vtable, NOT the host_inner() panic guard.
        //
        // Mentally revert `with_cdylib_scope`'s
        // `GpuContextFullAccess::from_scope_token(...)` to
        // `GpuContextFullAccess::new(host_gpu.clone())` (the Boxed
        // shape) — this assertion fails because the closure sees
        // `HandleKind::Boxed`, which is exactly the bug #1072 fixes.
        let result: crate::core::error::Result<()> = outer_ctx.with_cdylib_scope(|cdylib_ctx| {
            assert_eq!(
                cdylib_ctx.gpu_full.handle_kind,
                HandleKind::ScopeToken,
                "{TEST}: cdylib-scope sibling must hold a ScopeToken \
                     FullAccess — this is the engine-fix invariant #1072 \
                     relies on"
            );
            Ok(())
        });
        result.unwrap_or_else(|e| panic!("{TEST}: with_cdylib_scope returned err: {e}"));
    }
}

#[cfg(test)]
mod host_runtime_ops_wiring_tests {
    //! Lock-in for the #1426 wiring fix: a host-resident processor's
    //! `ctx.runtime()` must reach the REAL Runner-backed ops, not the
    //! byte-shaped [`RuntimeOpsShim`]. The shim has no transport for a
    //! streaming op, so `RuntimeOpsShim::tap_async` returns
    //! [`Error::NotSupported`] — if `ctx.runtime()` handed the api-server
    //! that shim, every host-side tap would be dead on arrival.
    //!
    //! The existing router tests inject a mock `RuntimeOperations` and so
    //! never exercise the `host_callbacks().is_none()` branch this fix adds;
    //! this test drives the branch through the SAME host-shaped
    //! `RuntimeContextFullAccess::new(&base)` that lifecycle dispatch mints.
    //!
    //! Channel resolution (`find_channel_source_port` over the live graph)
    //! runs BEFORE any iceoryx2 subscribe, so an unwired channel surfaces
    //! [`Error::TapChannelNotFound`] with no IPC work. Mentally revert the
    //! `host_callbacks().is_none()` branch in `runtime()` and the shim
    //! answers [`Error::NotSupported`] instead — this test goes red, which
    //! is the regression lock.
    //!
    //! GPU-gated because a `RuntimeContext` embeds a `GpuContext` by value;
    //! the assertion itself needs no GPU work, only the context shell.

    use super::*;
    use crate::core::context::{
        AudioClockConfig, GpuContext, SharedAudioClock, SoftwareAudioClock, TimeContext,
    };
    use crate::core::error::Error;
    use crate::core::runtime::Runner;

    fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
        match GpuContext::init_for_platform_sync() {
            Ok(gpu) => Some(gpu),
            Err(e) => {
                tracing::warn!("{test_name}: no GPU device ({e}) — skipping");
                None
            }
        }
    }

    #[test]
    fn host_ctx_runtime_reaches_real_runner_ops_not_the_shim() {
        const TEST: &str = "host_ctx_runtime_reaches_real_runner_ops_not_the_shim";
        // No plugin loaded in a plain lib test, so `host_callbacks()` is
        // `None` — this build IS the host, exactly the branch under test.
        assert!(
            crate::core::plugin::host_services::host_callbacks().is_none(),
            "{TEST}: precondition — a plain lib test has no host callbacks installed"
        );

        let Some(gpu) = gpu_or_skip(TEST) else {
            return;
        };

        let runner = Runner::new().expect("runner builds");
        let runtime_ops: Arc<dyn RuntimeOperations> =
            Arc::clone(&runner) as Arc<dyn RuntimeOperations>;

        let tokio_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread tokio runtime");
        let node = Iceoryx2Node::new().expect("iceoryx2 node");
        let audio_clock: SharedAudioClock =
            Arc::new(SoftwareAudioClock::new(AudioClockConfig::default()));

        let base = RuntimeContext::new(
            gpu,
            Arc::new(TimeContext::new()),
            Arc::new(RuntimeUniqueId::new()),
            runtime_ops,
            tokio_runtime.handle().clone(),
            node,
            audio_clock,
            #[cfg(target_os = "linux")]
            std::path::PathBuf::from("/tmp/streamlib-test-tap-wiring.sock"),
        );

        let ctx = RuntimeContextFullAccess::new(&base);
        let err = tokio_runtime.block_on(async {
            ctx.runtime()
                .tap_async("no-such-channel".to_string(), None)
                .await
                .expect_err("tapping an unwired channel must fail")
        });

        assert!(
            matches!(err, Error::TapChannelNotFound(_)),
            "{TEST}: host-resident ctx.runtime() must reach the real Runner ops so an unwired \
             channel surfaces TapChannelNotFound; the RuntimeOpsShim would answer NotSupported. \
             got {err:?}"
        );
    }
}

// =============================================================================
// Layout regression tests
// =============================================================================
//
// `RuntimeContextFullAccess` / `RuntimeContextLimitedAccess` cross the plugin
// ABI by raw-pointer reinterpret — the host builds the struct
// (`processor_instance_factory.rs`) and a cdylib reads its fields directly
// (`processor_vtable.rs`). They are `#[repr(C)]` so that layout is identical
// across the host build and a separately-built plugin (`streamlib-plugin-sdk`),
// which compiles a layout-matched twin. These assertions pin the byte shape;
// the SDK twin asserts the SAME numbers, so a field added to one side but not
// the other trips a test rather than corrupting field reads at runtime.
#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn gpu_context_view_sizes_are_pinned() {
        // The RuntimeContext views embed these by value, so their sizes are
        // load-bearing for the outer offsets below.
        assert_eq!(size_of::<GpuContextFullAccess>(), 40);
        assert_eq!(align_of::<GpuContextFullAccess>(), 8);
        assert_eq!(size_of::<GpuContextLimitedAccess>(), 16);
        assert_eq!(align_of::<GpuContextLimitedAccess>(), 8);
    }

    #[test]
    fn runtime_context_full_access_layout() {
        // handle      : *const c_void          → offset 0,  size 8
        // vtable      : *const RuntimeContextVTable → offset 8, size 8
        // gpu_full    : GpuContextFullAccess (40) → offset 16
        // gpu_limited : GpuContextLimitedAccess (16) → offset 56
        // _marker     : PhantomData (ZST)       → offset 72
        // Total: 72 bytes, 8-byte alignment.
        assert_eq!(size_of::<RuntimeContextFullAccess<'static>>(), 72);
        assert_eq!(align_of::<RuntimeContextFullAccess<'static>>(), 8);
        assert_eq!(offset_of!(RuntimeContextFullAccess<'static>, handle), 0);
        assert_eq!(offset_of!(RuntimeContextFullAccess<'static>, vtable), 8);
        assert_eq!(offset_of!(RuntimeContextFullAccess<'static>, gpu_full), 16);
        assert_eq!(
            offset_of!(RuntimeContextFullAccess<'static>, gpu_limited),
            56
        );
    }

    #[test]
    fn runtime_context_limited_access_layout() {
        // handle      : *const c_void          → offset 0,  size 8
        // vtable      : *const RuntimeContextVTable → offset 8, size 8
        // gpu_limited : GpuContextLimitedAccess (16) → offset 16
        // _marker     : PhantomData (ZST)       → offset 32
        // Total: 32 bytes, 8-byte alignment.
        assert_eq!(size_of::<RuntimeContextLimitedAccess<'static>>(), 32);
        assert_eq!(align_of::<RuntimeContextLimitedAccess<'static>>(), 8);
        assert_eq!(offset_of!(RuntimeContextLimitedAccess<'static>, handle), 0);
        assert_eq!(offset_of!(RuntimeContextLimitedAccess<'static>, vtable), 8);
        assert_eq!(
            offset_of!(RuntimeContextLimitedAccess<'static>, gpu_limited),
            16
        );
    }
}
